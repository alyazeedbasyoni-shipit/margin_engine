use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

// ============================================================================
// Types
// ============================================================================

type AccountId = u64;
type MarketId = String;

// ============================================================================
// Market Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    pub market_id: MarketId,
    pub mark_price: Decimal,
    /// Initial margin fraction (e.g., 0.05 = 5%)
    pub im_fraction: Decimal,
    /// Maintenance margin fraction (e.g., 0.025 = 2.5%)
    pub mm_fraction: Decimal,
}

// ============================================================================
// Position
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    /// Signed size: positive = long, negative = short
    pub size: Decimal,
    /// Volume-weighted average entry price
    pub entry_price: Decimal,
}

impl Position {
    pub fn new() -> Self {
        Self {
            size: dec!(0),
            entry_price: dec!(0),
        }
    }

    /// Compute unrealized PnL at a given mark price
    pub fn unrealized_pnl(&self, mark_price: Decimal) -> Decimal {
        self.size * (mark_price - self.entry_price)
    }

    /// Compute the notional value (absolute) at a given mark price
    pub fn notional_value(&self, mark_price: Decimal) -> Decimal {
        self.size.abs() * mark_price
    }

    /// Apply a trade fill: update size and entry price using VWAP for increases,
    /// or realize PnL for decreases/flips.
    /// Returns the realized PnL from this fill.
    pub fn apply_fill(&mut self, fill_size: Decimal, fill_price: Decimal) -> Decimal {
        if self.size == dec!(0) {
            // Opening a new position
            self.size = fill_size;
            self.entry_price = fill_price;
            return dec!(0);
        }

        let same_direction = (self.size > dec!(0) && fill_size > dec!(0))
            || (self.size < dec!(0) && fill_size < dec!(0));

        if same_direction {
            // Increasing position: VWAP entry price
            let total_cost =
                self.size.abs() * self.entry_price + fill_size.abs() * fill_price;
            let new_size = self.size + fill_size;
            self.entry_price = total_cost / new_size.abs();
            self.size = new_size;
            dec!(0)
        } else {
            // Reducing or flipping position
            let close_size = fill_size.abs().min(self.size.abs());
            let realized_pnl = if self.size > dec!(0) {
                // Long position being reduced
                close_size * (fill_price - self.entry_price)
            } else {
                // Short position being reduced
                close_size * (self.entry_price - fill_price)
            };

            let remaining = self.size + fill_size;
            if remaining == dec!(0) {
                // Position fully closed
                self.size = dec!(0);
                self.entry_price = dec!(0);
            } else if (remaining > dec!(0)) != (self.size > dec!(0)) {
                // Position flipped
                self.size = remaining;
                self.entry_price = fill_price;
            } else {
                // Position reduced but not closed
                self.size = remaining;
                // entry_price stays the same
            }
            realized_pnl
        }
    }
}

// ============================================================================
// Account
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub account_id: AccountId,
    /// Collateral balance (USDC). Includes realized PnL.
    pub collateral: Decimal,
    /// Positions keyed by market_id
    pub positions: BTreeMap<MarketId, Position>,
}

impl Account {
    pub fn new(account_id: AccountId) -> Self {
        Self {
            account_id,
            collateral: dec!(0),
            positions: BTreeMap::new(),
        }
    }

    /// Total unrealized PnL across all positions
    pub fn total_unrealized_pnl(&self, markets: &BTreeMap<MarketId, MarketConfig>) -> Decimal {
        self.positions
            .iter()
            .map(|(market_id, pos)| {
                let mark_price = markets.get(market_id).map_or(dec!(0), |m| m.mark_price);
                pos.unrealized_pnl(mark_price)
            })
            .sum()
    }

    /// Portfolio equity = collateral + total unrealized PnL
    pub fn equity(&self, markets: &BTreeMap<MarketId, MarketConfig>) -> Decimal {
        self.collateral + self.total_unrealized_pnl(markets)
    }

    /// Total notional value across all positions (sum of |size * mark_price|)
    pub fn total_notional(&self, markets: &BTreeMap<MarketId, MarketConfig>) -> Decimal {
        self.positions
            .iter()
            .map(|(market_id, pos)| {
                let mark_price = markets.get(market_id).map_or(dec!(0), |m| m.mark_price);
                pos.notional_value(mark_price)
            })
            .sum()
    }

    /// Initial margin requirement at portfolio level
    pub fn initial_margin_requirement(
        &self,
        markets: &BTreeMap<MarketId, MarketConfig>,
    ) -> Decimal {
        self.positions
            .iter()
            .map(|(market_id, pos)| {
                let market = markets.get(market_id).unwrap();
                pos.notional_value(market.mark_price) * market.im_fraction
            })
            .sum()
    }

    /// Maintenance margin requirement at portfolio level
    pub fn maintenance_margin_requirement(
        &self,
        markets: &BTreeMap<MarketId, MarketConfig>,
    ) -> Decimal {
        self.positions
            .iter()
            .map(|(market_id, pos)| {
                let market = markets.get(market_id).unwrap();
                pos.notional_value(market.mark_price) * market.mm_fraction
            })
            .sum()
    }

    /// Check if account is liquidatable: equity < maintenance margin
    pub fn is_liquidatable(&self, markets: &BTreeMap<MarketId, MarketConfig>) -> bool {
        let equity = self.equity(markets);
        let mm = self.maintenance_margin_requirement(markets);
        equity < mm
    }

    /// Check if account has any open positions
    pub fn has_positions(&self) -> bool {
        self.positions.values().any(|p| p.size != dec!(0))
    }
}

// ============================================================================
// Events
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Create a new market with given configuration
    CreateMarket {
        market_id: MarketId,
        initial_price: Decimal,
        im_fraction: Decimal,
        mm_fraction: Decimal,
    },
    /// Deposit collateral into an account
    Deposit {
        account_id: AccountId,
        amount: Decimal,
    },
    /// Withdraw collateral from an account
    Withdrawal {
        account_id: AccountId,
        amount: Decimal,
    },
    /// Execute a trade fill
    Trade {
        account_id: AccountId,
        market_id: MarketId,
        /// Signed size: positive = buy/long, negative = sell/short
        size: Decimal,
        price: Decimal,
    },
    /// Update the mark price for a market
    MarkPriceUpdate {
        market_id: MarketId,
        price: Decimal,
    },
    /// Apply funding payment to an account's position
    FundingPayment {
        account_id: AccountId,
        market_id: MarketId,
        /// Positive = account receives, negative = account pays
        amount: Decimal,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::CreateMarket { market_id, initial_price, im_fraction, mm_fraction } => {
                write!(f, "CreateMarket({}, price={}, IM={}%, MM={}%)",
                    market_id, initial_price, im_fraction * dec!(100), mm_fraction * dec!(100))
            }
            Event::Deposit { account_id, amount } => {
                write!(f, "Deposit(account={}, amount={})", account_id, amount)
            }
            Event::Withdrawal { account_id, amount } => {
                write!(f, "Withdrawal(account={}, amount={})", account_id, amount)
            }
            Event::Trade { account_id, market_id, size, price } => {
                let direction = if *size > dec!(0) { "BUY" } else { "SELL" };
                write!(f, "Trade(account={}, market={}, {} {} @ {})",
                    account_id, market_id, direction, size.abs(), price)
            }
            Event::MarkPriceUpdate { market_id, price } => {
                write!(f, "MarkPriceUpdate({}, price={})", market_id, price)
            }
            Event::FundingPayment { account_id, market_id, amount } => {
                write!(f, "FundingPayment(account={}, market={}, amount={})",
                    account_id, market_id, amount)
            }
        }
    }
}

// ============================================================================
// Event Processing Result
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventResult {
    Ok,
    TradeRejected { reason: String },
    WithdrawalRejected { reason: String },
    AccountLiquidated {
        account_id: AccountId,
        positions_closed: Vec<(MarketId, Decimal)>,
        remaining_collateral: Decimal,
    },
    AccountNotFound { account_id: AccountId },
    MarketNotFound { market_id: MarketId },
}

impl fmt::Display for EventResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventResult::Ok => write!(f, "OK"),
            EventResult::TradeRejected { reason } => write!(f, "TRADE REJECTED: {}", reason),
            EventResult::WithdrawalRejected { reason } => {
                write!(f, "WITHDRAWAL REJECTED: {}", reason)
            }
            EventResult::AccountLiquidated {
                account_id,
                positions_closed,
                remaining_collateral,
            } => {
                write!(
                    f,
                    "LIQUIDATED account={}, closed={:?}, remaining_collateral={}",
                    account_id, positions_closed, remaining_collateral
                )
            }
            EventResult::AccountNotFound { account_id } => {
                write!(f, "ACCOUNT NOT FOUND: {}", account_id)
            }
            EventResult::MarketNotFound { market_id } => {
                write!(f, "MARKET NOT FOUND: {}", market_id)
            }
        }
    }
}

// ============================================================================
// Engine State
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineState {
    pub accounts: BTreeMap<AccountId, Account>,
    pub markets: BTreeMap<MarketId, MarketConfig>,
    pub event_count: u64,
}

impl EngineState {
    pub fn new() -> Self {
        Self {
            accounts: BTreeMap::new(),
            markets: BTreeMap::new(),
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
                let equity = simulated.equity(&self.markets);
                let im = simulated.initial_margin_requirement(&self.markets);

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

                // Check post-trade margin requirement
                let equity = simulated.equity(&self.markets);
                let im = simulated.initial_margin_requirement(&self.markets);

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
                account.has_positions() && account.is_liquidatable(&self.markets)
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

        println!("\n  Accounts:");
        for (id, account) in &self.accounts {
            let equity = account.equity(&self.markets);
            let im = account.initial_margin_requirement(&self.markets);
            let mm = account.maintenance_margin_requirement(&self.markets);
            let total_notional = account.total_notional(&self.markets);
            let unrealized_pnl = account.total_unrealized_pnl(&self.markets);
            let is_liq = account.is_liquidatable(&self.markets);

            println!("\n    Account #{}:", id);
            println!("      Collateral:       {:>14}", account.collateral);
            println!("      Unrealized PnL:   {:>14}", unrealized_pnl);
            println!("      Portfolio Equity:  {:>14}", equity);
            println!("      Total Notional:    {:>14}", total_notional);
            println!("      Initial Margin:    {:>14}", im);
            println!("      Maint. Margin:     {:>14}", mm);
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
}

// ============================================================================
// Demo Event Log
// ============================================================================

fn build_demo_event_log() -> Vec<Event> {
    vec![
        // ── Setup: Create markets ──────────────────────────────────────────
        Event::CreateMarket {
            market_id: "BTC-USD".to_string(),
            initial_price: dec!(50000),
            im_fraction: dec!(0.05),  // 5% initial margin
            mm_fraction: dec!(0.025), // 2.5% maintenance margin
        },
        Event::CreateMarket {
            market_id: "ETH-USD".to_string(),
            initial_price: dec!(3000),
            im_fraction: dec!(0.05),
            mm_fraction: dec!(0.025),
        },
        // ── Scenario 1: Healthy portfolio becomes liquidatable ─────────────
        //
        // Account 1 deposits $10,000 and opens positions in BTC and ETH.
        // After adverse price movement, equity drops below maintenance margin.
        Event::Deposit {
            account_id: 1,
            amount: dec!(10000),
        },
        // Long 1 BTC at $50,000 (notional=$50k, IM=$2,500)
        Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        },
        // Long 5 ETH at $3,000 (notional=$15k, IM=$750)
        // Total IM = $3,250, equity = $10,000 → healthy
        Event::Trade {
            account_id: 1,
            market_id: "ETH-USD".to_string(),
            size: dec!(5),
            price: dec!(3000),
        },
        // BTC drops to $42,000 → uPnL(BTC) = 1*(42000-50000) = -$8,000
        Event::MarkPriceUpdate {
            market_id: "BTC-USD".to_string(),
            price: dec!(42000),
        },
        // ETH drops to $2,500 → uPnL(ETH) = 5*(2500-3000) = -$2,500
        // Total uPnL = -$10,500, equity = -$500
        // MM = (42000+12500)*0.025 = $1,362.50
        // equity (-$500) < MM ($1,362.50) → LIQUIDATION
        Event::MarkPriceUpdate {
            market_id: "ETH-USD".to_string(),
            price: dec!(2500),
        },
        // ── Scenario 2: Trade rejected due to margin constraints ──────────
        //
        // Account 2 deposits $5,000. First trade succeeds, second is rejected
        // because total IM would exceed equity.
        Event::Deposit {
            account_id: 2,
            amount: dec!(5000),
        },
        // Long 2 BTC at $42,000 → notional=$84k, IM=$4,200
        // Equity=$5,000 > IM=$4,200 → allowed
        Event::Trade {
            account_id: 2,
            market_id: "BTC-USD".to_string(),
            size: dec!(2),
            price: dec!(42000),
        },
        // Try long 10 ETH at $2,500 → additional notional=$25k, additional IM=$1,250
        // Total IM would be $4,200+$1,250=$5,450
        // Equity=$5,000 < $5,450 → REJECTED
        Event::Trade {
            account_id: 2,
            market_id: "ETH-USD".to_string(),
            size: dec!(10),
            price: dec!(2500),
        },
        // ── Scenario 3: Cross-margin hedging benefit ──────────────────────
        //
        // Account 3 holds a hedged portfolio (long BTC, short ETH).
        // Both assets drop, but the hedge limits losses.
        Event::Deposit {
            account_id: 3,
            amount: dec!(8000),
        },
        // Long 1 BTC at $42,000
        Event::Trade {
            account_id: 3,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(42000),
        },
        // Short 15 ETH at $2,500
        Event::Trade {
            account_id: 3,
            market_id: "ETH-USD".to_string(),
            size: dec!(-15),
            price: dec!(2500),
        },
        // BTC drops to $40,000 → uPnL(BTC) = 1*(40000-42000) = -$2,000
        Event::MarkPriceUpdate {
            market_id: "BTC-USD".to_string(),
            price: dec!(40000),
        },
        // ETH drops to $2,300 → uPnL(ETH) = -15*(2300-2500) = +$3,000
        // Net uPnL = +$1,000, equity = $9,000 → healthy thanks to hedge
        Event::MarkPriceUpdate {
            market_id: "ETH-USD".to_string(),
            price: dec!(2300),
        },
        // ── Funding payment example ───────────────────────────────────────
        Event::FundingPayment {
            account_id: 3,
            market_id: "BTC-USD".to_string(),
            amount: dec!(-50), // Account 3 pays $50 in funding
        },
    ]
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    println!("{}", "=".repeat(70));
    println!("  Deterministic Cross-Margin Perpetual Risk Engine — Demo");
    println!("{}", "=".repeat(70));

    let event_log = build_demo_event_log();

    // ──── First Run ────────────────────────────────────────────────────────
    println!("\n>>> FIRST RUN: Processing {} events...\n", event_log.len());

    let mut engine = EngineState::new();

    for (i, event) in event_log.iter().enumerate() {
        println!("  [{:>2}] {}", i + 1, event);
        let result = engine.process_event(event);
        match &result {
            EventResult::Ok => println!("       -> {}", result),
            _ => println!("       -> *** {} ***", result),
        }

        // Print brief account summaries after state-changing events
        match event {
            Event::Trade { .. }
            | Event::MarkPriceUpdate { .. }
            | Event::FundingPayment { .. } => {
                for (id, account) in &engine.accounts {
                    if account.has_positions() || account.collateral > dec!(0) {
                        let equity = account.equity(&engine.markets);
                        let mm = account.maintenance_margin_requirement(&engine.markets);
                        let im = account.initial_margin_requirement(&engine.markets);
                        println!(
                            "          [Acct {}] equity={}, IM={}, MM={}, liq={}",
                            id,
                            equity,
                            im,
                            mm,
                            if account.is_liquidatable(&engine.markets) {
                                "YES"
                            } else {
                                "no"
                            }
                        );
                    }
                }
            }
            _ => {}
        }
    }

    engine.print_summary();
    let first_hash = engine.state_hash();

    // ──── Replay Run ───────────────────────────────────────────────────────
    println!("\n>>> REPLAY RUN: Processing same {} events...\n", event_log.len());

    let mut engine2 = EngineState::new();
    for event in &event_log {
        engine2.process_event(event);
    }

    engine2.print_summary();
    let second_hash = engine2.state_hash();

    // ──── Determinism Verification ─────────────────────────────────────────
    println!("\n>>> DETERMINISM VERIFICATION");
    println!("{}", "-".repeat(70));
    println!("  First run hash:  {}", first_hash);
    println!("  Replay run hash: {}", second_hash);
    if first_hash == second_hash {
        println!("  RESULT: PASS — Both runs produced identical state.");
    } else {
        println!("  RESULT: FAIL — States diverged!");
    }
    println!("{}", "-".repeat(70));

    // ──── Save artifacts ───────────────────────────────────────────────────
    let event_log_json = serde_json::to_string_pretty(&event_log).unwrap();
    std::fs::write("event_log.json", &event_log_json).unwrap();
    println!("\n  Event log saved to event_log.json ({} events)", event_log.len());

    let state_json = serde_json::to_string_pretty(&engine).unwrap();
    std::fs::write("final_state.json", &state_json).unwrap();
    println!("  Final state saved to final_state.json\n");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_engine() -> EngineState {
        let mut engine = EngineState::new();
        engine.process_event(&Event::CreateMarket {
            market_id: "BTC-USD".to_string(),
            initial_price: dec!(50000),
            im_fraction: dec!(0.05),
            mm_fraction: dec!(0.025),
        });
        engine.process_event(&Event::CreateMarket {
            market_id: "ETH-USD".to_string(),
            initial_price: dec!(3000),
            im_fraction: dec!(0.05),
            mm_fraction: dec!(0.025),
        });
        engine
    }

    #[test]
    fn test_deposit_and_equity() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(10000),
        });
        let account = engine.accounts.get(&1).unwrap();
        assert_eq!(account.collateral, dec!(10000));
        assert_eq!(account.equity(&engine.markets), dec!(10000));
    }

    #[test]
    fn test_trade_opens_position() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(10000),
        });
        let result = engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        assert!(matches!(result, EventResult::Ok));
        let account = engine.accounts.get(&1).unwrap();
        let pos = account.positions.get("BTC-USD").unwrap();
        assert_eq!(pos.size, dec!(1));
        assert_eq!(pos.entry_price, dec!(50000));
    }

    #[test]
    fn test_trade_rejected_insufficient_margin() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(1000),
        });
        // Try to buy 1 BTC at $50,000 — notional $50,000, IM = $2,500
        // Equity = $1,000 < IM = $2,500 → rejected
        let result = engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        assert!(matches!(result, EventResult::TradeRejected { .. }));
    }

    #[test]
    fn test_liquidation_on_price_drop() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(3000),
        });
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        // equity = 3000, IM = 2500, MM = 1250 — healthy

        // Price drops to $48,000 → uPnL = -$2,000, equity = $1,000
        // MM = 48000 * 0.025 = $1,200
        // equity ($1,000) < MM ($1,200) → liquidation
        let result = engine.process_event(&Event::MarkPriceUpdate {
            market_id: "BTC-USD".to_string(),
            price: dec!(48000),
        });
        assert!(matches!(result, EventResult::AccountLiquidated { .. }));
        let account = engine.accounts.get(&1).unwrap();
        assert!(account.positions.is_empty());
    }

    #[test]
    fn test_deterministic_replay() {
        let events = build_demo_event_log();

        let mut engine1 = EngineState::new();
        for event in &events {
            engine1.process_event(event);
        }

        let mut engine2 = EngineState::new();
        for event in &events {
            engine2.process_event(event);
        }

        assert_eq!(engine1.state_hash(), engine2.state_hash());
    }

    #[test]
    fn test_cross_margin_benefit() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(5000),
        });

        // Long 1 BTC, Short 15 ETH (hedged)
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "ETH-USD".to_string(),
            size: dec!(-15),
            price: dec!(3000),
        });

        // Both drop proportionally
        engine.process_event(&Event::MarkPriceUpdate {
            market_id: "BTC-USD".to_string(),
            price: dec!(47500),
        });
        engine.process_event(&Event::MarkPriceUpdate {
            market_id: "ETH-USD".to_string(),
            price: dec!(2850),
        });

        // BTC PnL = 1 * (47500 - 50000) = -2500
        // ETH PnL = -15 * (2850 - 3000) = +2250
        // Net PnL = -250
        // Equity = 5000 - 250 = 4750
        let account = engine.accounts.get(&1).unwrap();
        let equity = account.equity(&engine.markets);
        assert_eq!(equity, dec!(4750));
        assert!(!account.is_liquidatable(&engine.markets));
    }

    #[test]
    fn test_withdrawal_rejected_if_margin_insufficient() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(5000),
        });
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        // IM = 2500, equity = 5000
        // Withdrawing 3000 would leave equity = 2000 < IM = 2500
        let result = engine.process_event(&Event::Withdrawal {
            account_id: 1,
            amount: dec!(3000),
        });
        assert!(matches!(result, EventResult::WithdrawalRejected { .. }));
    }

    #[test]
    fn test_funding_payment() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(10000),
        });
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });

        let collateral_before = engine.accounts.get(&1).unwrap().collateral;
        engine.process_event(&Event::FundingPayment {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            amount: dec!(-100),
        });
        let collateral_after = engine.accounts.get(&1).unwrap().collateral;
        assert_eq!(collateral_after, collateral_before - dec!(100));
    }

    #[test]
    fn test_position_increase_vwap() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(100000),
        });

        // Buy 1 BTC at $50,000
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });

        // Buy 1 more BTC at $52,000 → VWAP entry = (50000+52000)/2 = $51,000
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(52000),
        });

        let pos = engine
            .accounts
            .get(&1)
            .unwrap()
            .positions
            .get("BTC-USD")
            .unwrap();
        assert_eq!(pos.size, dec!(2));
        assert_eq!(pos.entry_price, dec!(51000));
    }

    #[test]
    fn test_position_partial_close_realized_pnl() {
        let mut engine = setup_engine();
        engine.process_event(&Event::Deposit {
            account_id: 1,
            amount: dec!(100000),
        });

        // Buy 2 BTC at $50,000
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(2),
            price: dec!(50000),
        });

        let collateral_before = engine.accounts.get(&1).unwrap().collateral;

        // Sell 1 BTC at $55,000 → realized PnL = 1 * (55000 - 50000) = $5,000
        engine.process_event(&Event::Trade {
            account_id: 1,
            market_id: "BTC-USD".to_string(),
            size: dec!(-1),
            price: dec!(55000),
        });

        let account = engine.accounts.get(&1).unwrap();
        assert_eq!(account.collateral, collateral_before + dec!(5000));
        let pos = account.positions.get("BTC-USD").unwrap();
        assert_eq!(pos.size, dec!(1));
        assert_eq!(pos.entry_price, dec!(50000));
    }
}
