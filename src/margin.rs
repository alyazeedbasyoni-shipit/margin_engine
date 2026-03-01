use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

use crate::types::*;

/// A correlation pair key that is order-independent.
/// We always store (min, max) to ensure deterministic lookup.
/// Serializes as a string "MARKET_A/MARKET_B" for JSON compatibility.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CorrelationKey(pub MarketId, pub MarketId);

impl CorrelationKey {
    pub fn new(a: &str, b: &str) -> Self {
        if a <= b {
            CorrelationKey(a.to_string(), b.to_string())
        } else {
            CorrelationKey(b.to_string(), a.to_string())
        }
    }
}

impl fmt::Display for CorrelationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.0, self.1)
    }
}

impl Serialize for CorrelationKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{}/{}", self.0, self.1))
    }
}

impl<'de> Deserialize<'de> for CorrelationKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let parts: Vec<&str> = s.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(serde::de::Error::custom("expected format MARKET_A/MARKET_B"));
        }
        Ok(CorrelationKey::new(parts[0], parts[1]))
    }
}

/// The correlation matrix, stored as a BTreeMap for deterministic iteration.
/// Keys are ordered pairs of market IDs; values are correlation coefficients in [-1, 1].
/// If a pair is not present, the default correlation is 0 (no netting benefit).
pub type CorrelationMatrix = BTreeMap<CorrelationKey, Decimal>;

// ============================================================================
// Margin Computation
// ============================================================================

/// Compute the **naive** (non-correlation-adjusted) margin requirement for an account.
/// This is the simple sum of per-market margin requirements.
pub fn naive_margin_requirement(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
    use_im: bool,
) -> Decimal {
    account
        .positions
        .iter()
        .map(|(market_id, pos)| {
            let market = match markets.get(market_id) {
                Some(m) => m,
                None => return dec!(0),
            };
            let fraction = if use_im { market.im_fraction } else { market.mm_fraction };
            pos.notional_value(market.mark_price) * fraction
        })
        .sum()
}

/// Compute the **correlation-adjusted** margin requirement for an account.
///
/// The idea: when two positions are in opposite directions (long A / short B)
/// and the assets are positively correlated, the portfolio is hedged and the
/// combined risk is lower than the sum of individual risks. We grant a margin
/// discount proportional to the correlation and the overlapping notional.
///
/// Formula:
///   adjusted_margin = naive_margin - Σ_{i<j} netting_discount(i, j)
///
/// Where for each pair (i, j) of positions:
///   netting_discount(i, j) = correlation(i,j) * min(notional_i, notional_j) * avg_fraction
///                             * direction_factor
///
///   direction_factor:
///     +1 if positions are in opposite directions (hedge → discount)
///     -1 if positions are in the same direction (concentration → surcharge)
///
/// This means:
///   - Long BTC + Short ETH with ρ=0.8 → significant margin REDUCTION
///   - Long BTC + Long ETH with ρ=0.8  → margin INCREASE (concentration penalty)
///   - Long BTC + Short ETH with ρ=0.0 → no adjustment (uncorrelated)
///
/// The result is clamped to a minimum of 0 (margin can never be negative).
pub fn correlation_adjusted_margin(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
    correlations: &CorrelationMatrix,
    use_im: bool,
) -> Decimal {
    let naive = naive_margin_requirement(account, markets, use_im);

    // If fewer than 2 positions, no correlation adjustment possible
    if account.positions.len() < 2 {
        return naive;
    }

    // Collect position info: (market_id, signed_notional, notional_abs, fraction)
    let position_info: Vec<(&MarketId, Decimal, Decimal, Decimal)> = account
        .positions
        .iter()
        .filter_map(|(market_id, pos)| {
            let market = markets.get(market_id)?;
            let fraction = if use_im { market.im_fraction } else { market.mm_fraction };
            let signed_notional = pos.size * market.mark_price;
            let abs_notional = pos.notional_value(market.mark_price);
            Some((market_id, signed_notional, abs_notional, fraction))
        })
        .collect();

    let mut total_adjustment = dec!(0);

    // Iterate over all unique pairs (deterministic order via BTreeMap)
    for i in 0..position_info.len() {
        for j in (i + 1)..position_info.len() {
            let (market_a, signed_a, abs_a, frac_a) = &position_info[i];
            let (market_b, signed_b, abs_b, frac_b) = &position_info[j];

            let key = CorrelationKey::new(market_a, market_b);
            let correlation = correlations.get(&key).copied().unwrap_or(dec!(0));

            if correlation == dec!(0) {
                continue; // No adjustment for uncorrelated pairs
            }

            // Determine direction factor:
            // Opposite directions (one positive, one negative) → hedge → discount (+1)
            // Same direction → concentration → surcharge (-1)
            let opposite_directions =
                (*signed_a > dec!(0) && *signed_b < dec!(0))
                || (*signed_a < dec!(0) && *signed_b > dec!(0));

            let direction_factor = if opposite_directions { dec!(1) } else { dec!(-1) };

            // The netting discount/surcharge is based on the overlapping notional
            let overlapping_notional = (*abs_a).min(*abs_b);
            let avg_fraction = (*frac_a + *frac_b) / dec!(2);

            // discount = correlation * overlap * avg_fraction * direction_factor
            let adjustment = correlation * overlapping_notional * avg_fraction * direction_factor;
            total_adjustment += adjustment;
        }
    }

    // Adjusted margin = naive - total_adjustment (discount is positive, surcharge is negative)
    let adjusted = naive - total_adjustment;

    // Clamp to minimum of 0
    if adjusted < dec!(0) {
        dec!(0)
    } else {
        adjusted
    }
}

/// Portfolio equity = collateral + total unrealized PnL
pub fn portfolio_equity(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
) -> Decimal {
    let unrealized_pnl: Decimal = account
        .positions
        .iter()
        .map(|(market_id, pos)| {
            let mark_price = markets.get(market_id).map_or(dec!(0), |m| m.mark_price);
            pos.unrealized_pnl(mark_price)
        })
        .sum();
    account.collateral + unrealized_pnl
}

/// Total unrealized PnL across all positions
pub fn total_unrealized_pnl(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
) -> Decimal {
    account
        .positions
        .iter()
        .map(|(market_id, pos)| {
            let mark_price = markets.get(market_id).map_or(dec!(0), |m| m.mark_price);
            pos.unrealized_pnl(mark_price)
        })
        .sum()
}

/// Total notional value across all positions
pub fn total_notional(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
) -> Decimal {
    account
        .positions
        .iter()
        .map(|(market_id, pos)| {
            let mark_price = markets.get(market_id).map_or(dec!(0), |m| m.mark_price);
            pos.notional_value(mark_price)
        })
        .sum()
}

/// Check if account is liquidatable: equity < maintenance margin (correlation-adjusted)
pub fn is_liquidatable(
    account: &Account,
    markets: &BTreeMap<MarketId, MarketConfig>,
    correlations: &CorrelationMatrix,
) -> bool {
    let equity = portfolio_equity(account, markets);
    let mm = correlation_adjusted_margin(account, markets, correlations, false);
    equity < mm
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_markets() -> BTreeMap<MarketId, MarketConfig> {
        let mut markets = BTreeMap::new();
        markets.insert(
            "BTC-USD".to_string(),
            MarketConfig {
                market_id: "BTC-USD".to_string(),
                mark_price: dec!(50000),
                im_fraction: dec!(0.05),
                mm_fraction: dec!(0.025),
            },
        );
        markets.insert(
            "ETH-USD".to_string(),
            MarketConfig {
                market_id: "ETH-USD".to_string(),
                mark_price: dec!(3000),
                im_fraction: dec!(0.05),
                mm_fraction: dec!(0.025),
            },
        );
        markets
    }

    fn make_account_hedged() -> Account {
        let mut account = Account::new(1);
        account.collateral = dec!(10000);
        // Long 1 BTC (notional = $50,000)
        account.positions.insert(
            "BTC-USD".to_string(),
            Position { size: dec!(1), entry_price: dec!(50000) },
        );
        // Short ~16.67 ETH (notional = $50,000) — matched notional
        account.positions.insert(
            "ETH-USD".to_string(),
            Position { size: dec!(-16.666666), entry_price: dec!(3000) },
        );
        account
    }

    fn make_account_concentrated() -> Account {
        let mut account = Account::new(2);
        account.collateral = dec!(10000);
        // Long 1 BTC (notional = $50,000)
        account.positions.insert(
            "BTC-USD".to_string(),
            Position { size: dec!(1), entry_price: dec!(50000) },
        );
        // Long ~16.67 ETH (notional = $50,000) — same direction
        account.positions.insert(
            "ETH-USD".to_string(),
            Position { size: dec!(16.666666), entry_price: dec!(3000) },
        );
        account
    }

    #[test]
    fn test_naive_margin_no_correlation() {
        let markets = make_markets();
        let account = make_account_hedged();
        let no_corr = CorrelationMatrix::new();

        let im_naive = naive_margin_requirement(&account, &markets, true);
        let im_adjusted = correlation_adjusted_margin(&account, &markets, &no_corr, true);

        // With no correlations set, adjusted should equal naive
        assert_eq!(im_naive, im_adjusted);

        // Naive IM = 50000*0.05 + 49999.998*0.05 = 2500 + 2499.9999 ≈ 4999.9999
        assert!(im_naive > dec!(4999) && im_naive < dec!(5001));
    }

    #[test]
    fn test_hedged_portfolio_gets_margin_discount() {
        let markets = make_markets();
        let account = make_account_hedged(); // Long BTC, Short ETH
        let mut correlations = CorrelationMatrix::new();
        correlations.insert(
            CorrelationKey::new("BTC-USD", "ETH-USD"),
            dec!(0.8),
        );

        let im_naive = naive_margin_requirement(&account, &markets, true);
        let im_adjusted = correlation_adjusted_margin(&account, &markets, &correlations, true);

        // Hedged portfolio (opposite directions + positive correlation) should get a discount
        assert!(
            im_adjusted < im_naive,
            "Hedged portfolio should have lower margin: adjusted={} vs naive={}",
            im_adjusted, im_naive
        );

        // The discount should be significant with ρ=0.8
        let discount_pct = (dec!(1) - im_adjusted / im_naive) * dec!(100);
        assert!(
            discount_pct > dec!(30),
            "Discount should be >30% for ρ=0.8 hedge, got {}%",
            discount_pct
        );
    }

    #[test]
    fn test_concentrated_portfolio_gets_margin_surcharge() {
        let markets = make_markets();
        let account = make_account_concentrated(); // Long BTC, Long ETH
        let mut correlations = CorrelationMatrix::new();
        correlations.insert(
            CorrelationKey::new("BTC-USD", "ETH-USD"),
            dec!(0.8),
        );

        let im_naive = naive_margin_requirement(&account, &markets, true);
        let im_adjusted = correlation_adjusted_margin(&account, &markets, &correlations, true);

        // Concentrated portfolio (same direction + positive correlation) should get a surcharge
        assert!(
            im_adjusted > im_naive,
            "Concentrated portfolio should have higher margin: adjusted={} vs naive={}",
            im_adjusted, im_naive
        );
    }

    #[test]
    fn test_zero_correlation_no_adjustment() {
        let markets = make_markets();
        let account = make_account_hedged();
        let mut correlations = CorrelationMatrix::new();
        correlations.insert(
            CorrelationKey::new("BTC-USD", "ETH-USD"),
            dec!(0),
        );

        let im_naive = naive_margin_requirement(&account, &markets, true);
        let im_adjusted = correlation_adjusted_margin(&account, &markets, &correlations, true);

        assert_eq!(im_naive, im_adjusted);
    }

    #[test]
    fn test_single_position_no_adjustment() {
        let markets = make_markets();
        let mut account = Account::new(1);
        account.collateral = dec!(10000);
        account.positions.insert(
            "BTC-USD".to_string(),
            Position { size: dec!(1), entry_price: dec!(50000) },
        );

        let mut correlations = CorrelationMatrix::new();
        correlations.insert(
            CorrelationKey::new("BTC-USD", "ETH-USD"),
            dec!(0.8),
        );

        let im_naive = naive_margin_requirement(&account, &markets, true);
        let im_adjusted = correlation_adjusted_margin(&account, &markets, &correlations, true);

        // Single position: no pairs to adjust
        assert_eq!(im_naive, im_adjusted);
    }

    #[test]
    fn test_margin_never_negative() {
        let markets = make_markets();
        let account = make_account_hedged();
        let mut correlations = CorrelationMatrix::new();
        // Even with perfect correlation, margin should never go negative
        correlations.insert(
            CorrelationKey::new("BTC-USD", "ETH-USD"),
            dec!(1.0),
        );

        let im_adjusted = correlation_adjusted_margin(&account, &markets, &correlations, true);
        assert!(im_adjusted >= dec!(0), "Margin should never be negative: {}", im_adjusted);
    }

    #[test]
    fn test_higher_correlation_more_discount_for_hedge() {
        let markets = make_markets();
        let account = make_account_hedged();

        let mut corr_low = CorrelationMatrix::new();
        corr_low.insert(CorrelationKey::new("BTC-USD", "ETH-USD"), dec!(0.3));

        let mut corr_high = CorrelationMatrix::new();
        corr_high.insert(CorrelationKey::new("BTC-USD", "ETH-USD"), dec!(0.9));

        let im_low = correlation_adjusted_margin(&account, &markets, &corr_low, true);
        let im_high = correlation_adjusted_margin(&account, &markets, &corr_high, true);

        assert!(
            im_high < im_low,
            "Higher correlation should give more discount for hedge: ρ=0.9→{} vs ρ=0.3→{}",
            im_high, im_low
        );
    }
}
