use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

// ============================================================================
// Type Aliases
// ============================================================================

pub type AccountId = u64;
pub type MarketId = String;

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
    /// Update the correlation between two markets
    SetCorrelation {
        market_a: MarketId,
        market_b: MarketId,
        /// Correlation coefficient in [-1, 1]
        correlation: Decimal,
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
            Event::SetCorrelation { market_a, market_b, correlation } => {
                write!(f, "SetCorrelation({}/{}, ρ={})",
                    market_a, market_b, correlation)
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
            EventResult::TradeRejected { reason } => {
                write!(f, "TRADE REJECTED: {}", reason)
            }
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
