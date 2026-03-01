use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::margin::*;
use crate::types::*;

// ============================================================================
// Engine State
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineState {
    pub accounts: BTreeMap<AccountId, Account>,
    pub markets: BTreeMap<MarketId, MarketConfig>,
    pub correlations: CorrelationMatrix,
    pub event_count: u64,
}

impl EngineState {
    pub fn new() -> Self {
        Self {
            accounts: BTreeMap::new(),
            markets: BTreeMap::new(),
            correlations: CorrelationMatrix::new(),
            event_count: 0,
        }
    }

    /// Compute a deterministic hash of the current state for replay verification.
    /// Uses a simple byte-level hash. In production, use SHA-256.
    pub fn state_hash(&self) -> String {
        let serialized = serde_json::to_string(self).unwrap();
        // Deterministic hash: FNV-1a inspired
        let mut hash: u64 = 14695981039346656037;
        for byte in serialized.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        format!("{:016x}", hash)
    }

    /// Get or create an account
    fn get_or_create_account(&mut self, account_id: AccountId) -> &mut Account {
        self.accounts
            .entry(account_id)
            .or_insert_with(|| Account::new(account_id))
    }

    /// Process a single event and return the result
    pub fn process_event(&mut self, event: &Event) -> EventResult {
        self.event_count += 1;
        match event {
            Event::CreateMarket {
                market_id,
                initial_price,
                im_fraction,
                mm_fraction,
            } => {
                self.markets.insert(
                    market_id.clone(),
                    MarketConfig {
                        market_id: market_id.clone(),
                        mark_price: *initial_price,
                        im_fraction: *im_fraction,
                        mm_fraction: *mm_fraction,
                    },
                );
                EventResult::Ok
            }

            Event::Deposit { account_id, amount } => {
                let account = self.get_or_create_account(*account_id);
                account.collateral += amount;
                EventResult::Ok
            }

            Event::Withdrawal { account_id, amount } => {
                let account = match self.accounts.get(account_id) {
                    Some(a) => a,
                    None => {
                        return EventResult::AccountNotFound {
                            account_id: *account_id,
                        }
                    }
                };

                // Check if withdrawal would leave enough margin
                let post_collateral = account.collateral - amount;
                if post_collateral < dec!(0) {
                    return EventResult::WithdrawalRejected {
                        reason: "Insufficient collateral balance".to_string(),
                    };
                }

                // Simulate post-withdrawal state
                let mut simulated = account.clone();
                simulated.collateral = post_collateral;
                let equity = portfolio_equity(&simulated, &self.markets);
                let im = correlation_adjusted_margin(
                    &simulated, &self.markets, &self.correlations, true,
                );

                if simulated.has_positions() && equity < im {
                    return EventResult::WithdrawalRejected {
                        reason: format!(
                            "Post-withdrawal equity ({}) would be below initial margin requirement ({})",
                            equity, im
                        ),
                    };
                }

                let account = self.accounts.get_mut(account_id).unwrap();
                account.collateral -= amount;
                EventResult::Ok
            }

            Event::Trade {
                account_id,
                market_id,
                size,
                price,
            } => {
                // Validate market exists
                if !self.markets.contains_key(market_id) {
                    return EventResult::MarketNotFound {
                        market_id: market_id.clone(),
                    };
                }

                // Get or create account
                let account = self.get_or_create_account(*account_id);

                // Simulate post-trade state to check margin
                let mut simulated = account.clone();
                let pos = simulated
                    .positions
                    .entry(market_id.clone())
                    .or_insert_with(Position::new);
                let realized_pnl = pos.apply_fill(*size, *price);
                simulated.collateral += realized_pnl;

                // Clean up zero positions
                if pos.size == dec!(0) {
                    simulated.positions.remove(market_id);
                }

                // Check post-trade margin requirement (correlation-adjusted)
                let equity = portfolio_equity(&simulated, &self.markets);
                let im = correlation_adjusted_margin(
                    &simulated, &self.markets, &self.correlations, true,
                );

                if simulated.has_positions() && equity < im {
                    return EventResult::TradeRejected {
                        reason: format!(
                            "Post-trade equity ({}) would be below initial margin requirement ({})",
                            equity, im
                        ),
                    };
                }

                // Apply the trade for real
                let account = self.accounts.get_mut(account_id).unwrap();
                let pos = account
                    .positions
                    .entry(market_id.clone())
                    .or_insert_with(Position::new);
                let realized_pnl = pos.apply_fill(*size, *price);
                account.collateral += realized_pnl;

                // Clean up zero positions
                if pos.size == dec!(0) {
                    account.positions.remove(market_id);
                }

                EventResult::Ok
            }

            Event::MarkPriceUpdate { market_id, price } => {
                match self.markets.get_mut(market_id) {
                    Some(market) => {
                        market.mark_price = *price;
                    }
                    None => {
                        return EventResult::MarketNotFound {
                            market_id: market_id.clone(),
                        };
                    }
                }

                // Check all accounts for liquidation after price update
                let liquidation_result = self.check_and_execute_liquidations();
                if let Some(result) = liquidation_result {
                    return result;
                }

                EventResult::Ok
            }

            Event::FundingPayment {
                account_id,
                market_id,
                amount,
            } => {
                let account = match self.accounts.get_mut(account_id) {
                    Some(a) => a,
                    None => {
                        return EventResult::AccountNotFound {
                            account_id: *account_id,
                        }
                    }
                };

                if !self.markets.contains_key(market_id) {
                    return EventResult::MarketNotFound {
                        market_id: market_id.clone(),
                    };
                }

                // Funding is applied directly to collateral
                account.collateral += amount;
                EventResult::Ok
            }

            Event::SetCorrelation {
                market_a,
                market_b,
                correlation,
            } => {
                let key = CorrelationKey::new(market_a, market_b);
                self.correlations.insert(key, *correlation);
                EventResult::Ok
            }
        }
    }

    /// Check all accounts for liquidation and execute if needed.
    /// Returns the first liquidation result, if any.
    /// Uses BTreeMap iteration for deterministic ordering.
    fn check_and_execute_liquidations(&mut self) -> Option<EventResult> {
        // Collect accounts that need liquidation (deterministic order via BTreeMap)
        let accounts_to_liquidate: Vec<AccountId> = self
            .accounts
            .iter()
            .filter(|(_, account)| {
                account.has_positions()
                    && is_liquidatable(account, &self.markets, &self.correlations)
            })
            .map(|(id, _)| *id)
            .collect();

        for account_id in &accounts_to_liquidate {
            let account = self.accounts.get(account_id).unwrap();
            let mut positions_closed = Vec::new();

            // Collect positions to close (deterministic order via BTreeMap)
            for (market_id, pos) in &account.positions {
                if pos.size != dec!(0) {
                    positions_closed.push((market_id.clone(), pos.size));
                }
            }

            // Execute liquidation: close all positions at mark price
            let account = self.accounts.get_mut(account_id).unwrap();
            for (market_id, _) in &positions_closed {
                if let Some(pos) = account.positions.get_mut(market_id) {
                    let mark_price = self.markets.get(market_id).unwrap().mark_price;
                    let close_size = -pos.size; // Close the entire position
                    let realized_pnl = pos.apply_fill(close_size, mark_price);
                    account.collateral += realized_pnl;
                }
            }
            account.positions.clear();

            let remaining = account.collateral;

            // In a real system, negative remaining collateral would be covered
            // by an insurance fund. Here we clamp to zero.
            if account.collateral < dec!(0) {
                account.collateral = dec!(0);
            }

            return Some(EventResult::AccountLiquidated {
                account_id: *account_id,
                positions_closed,
                remaining_collateral: remaining,
            });
        }

        None
    }

    /// Print a summary of the current state
    pub fn print_summary(&self) {
        println!("\n{}", "=".repeat(80));
        println!(
            "ENGINE STATE (after {} events)  |  Hash: {}",
            self.event_count,
            self.state_hash()
        );
        println!("{}", "=".repeat(80));

        println!("\n  Markets:");
        println!(
            "  {:<12} {:>12} {:>8} {:>8}",
            "Market", "Mark Price", "IM%", "MM%"
        );
        println!("  {}", "-".repeat(44));
        for (id, market) in &self.markets {
            println!(
                "  {:<12} {:>12} {:>7}% {:>7}%",
                id,
                market.mark_price,
                market.im_fraction * dec!(100),
                market.mm_fraction * dec!(100)
            );
        }

        if !self.correlations.is_empty() {
            println!("\n  Correlations:");
            for (key, corr) in &self.correlations {
                println!("    {}/{}: ρ = {}", key.0, key.1, corr);
            }
        }

        println!("\n  Accounts:");
        for (id, account) in &self.accounts {
            let equity = portfolio_equity(account, &self.markets);
            let im_naive = naive_margin_requirement(account, &self.markets, true);
            let im_adjusted = correlation_adjusted_margin(
                account, &self.markets, &self.correlations, true,
            );
            let mm_naive = naive_margin_requirement(account, &self.markets, false);
            let mm_adjusted = correlation_adjusted_margin(
                account, &self.markets, &self.correlations, false,
            );
            let notional = total_notional(account, &self.markets);
            let upnl = total_unrealized_pnl(account, &self.markets);
            let is_liq = is_liquidatable(account, &self.markets, &self.correlations);

            println!("\n    Account #{}:", id);
            println!("      Collateral:       {:>14}", account.collateral);
            println!("      Unrealized PnL:   {:>14}", upnl);
            println!("      Portfolio Equity:  {:>14}", equity);
            println!("      Total Notional:    {:>14}", notional);
            println!("      Initial Margin:    {:>14} (naive: {})", im_adjusted, im_naive);
            println!("      Maint. Margin:     {:>14} (naive: {})", mm_adjusted, mm_naive);
            if im_adjusted != im_naive {
                let discount = (dec!(1) - im_adjusted / im_naive) * dec!(100);
                println!("      Correlation Adj:   {:>13}%", discount.round_dp(1));
            }
            println!(
                "      Liquidatable:      {:>14}",
                if is_liq { "YES" } else { "NO" }
            );

            if !account.positions.is_empty() {
                println!("      Positions:");
                for (market_id, pos) in &account.positions {
                    let mark = self
                        .markets
                        .get(market_id)
                        .map_or(dec!(0), |m| m.mark_price);
                    let direction = if pos.size > dec!(0) { "LONG" } else { "SHORT" };
                    println!(
                        "        {}: {} {} @ entry {} (mark: {}, uPnL: {})",
                        market_id,
                        direction,
                        pos.size.abs(),
                        pos.entry_price,
                        mark,
                        pos.unrealized_pnl(mark)
                    );
                }
            } else {
                println!("      Positions:         (none)");
            }
        }
        println!("\n{}", "=".repeat(80));
    }

    /// Print brief account summary line (used during event processing)
    pub fn print_account_summary(&self, account_id: &AccountId) {
        if let Some(account) = self.accounts.get(account_id) {
            if account.has_positions() || account.collateral > dec!(0) {
                let equity = portfolio_equity(account, &self.markets);
                let mm = correlation_adjusted_margin(
                    account, &self.markets, &self.correlations, false,
                );
                let im = correlation_adjusted_margin(
                    account, &self.markets, &self.correlations, true,
                );
                println!(
                    "          [Acct {}] equity={}, IM={}, MM={}, liq={}",
                    account_id,
                    equity,
                    im,
                    mm,
                    if is_liquidatable(account, &self.markets, &self.correlations) {
                        "YES"
                    } else {
                        "no"
                    }
                );
            }
        }
    }
}
