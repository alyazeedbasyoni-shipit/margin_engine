mod types;
mod margin;
mod engine;

use rust_decimal_macros::dec;

use types::*;
use engine::*;

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

        // ── Set correlation: BTC/ETH are highly correlated (ρ=0.85) ──────
        Event::SetCorrelation {
            market_a: "BTC-USD".to_string(),
            market_b: "ETH-USD".to_string(),
            correlation: dec!(0.85),
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
        // Same direction + positive correlation → HIGHER margin (concentration penalty)
        // Naive IM = $3,250, Adjusted IM > $3,250
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
        // Total uPnL = -$10,500, equity = -$500 → LIQUIDATION
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
        // Try long 10 ETH at $2,500 → additional notional=$25k
        // Same direction as BTC → concentration penalty applies
        // Adjusted IM > naive $5,450 → even harder to pass
        // Equity=$5,000 < adjusted IM → REJECTED
        Event::Trade {
            account_id: 2,
            market_id: "ETH-USD".to_string(),
            size: dec!(10),
            price: dec!(2500),
        },

        // ── Scenario 3: Cross-margin hedging with correlation benefit ─────
        //
        // Account 3 holds a hedged portfolio (long BTC, short ETH).
        // Because BTC and ETH are correlated, opposite directions = hedge.
        // The correlation-adjusted margin is LOWER than naive margin.
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
        // Short 15 ETH at $2,500 → hedged against BTC
        // Naive IM = 42000*0.05 + 37500*0.05 = 2100 + 1875 = $3,975
        // Adjusted IM < $3,975 (discount for hedge)
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

        // ── Scenario 4: Demonstrate correlation change ────────────────────
        //
        // Update correlation to show how margin requirements change dynamically.
        Event::SetCorrelation {
            market_a: "BTC-USD".to_string(),
            market_b: "ETH-USD".to_string(),
            correlation: dec!(0.5), // Reduced correlation
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
    println!("  (with correlation-adjusted margin netting)");
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
            | Event::FundingPayment { .. }
            | Event::SetCorrelation { .. } => {
                for id in engine.accounts.keys().cloned().collect::<Vec<_>>() {
                    engine.print_account_summary(&id);
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
// Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::margin::*;

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
        // Set default correlation
        engine.process_event(&Event::SetCorrelation {
            market_a: "BTC-USD".to_string(),
            market_b: "ETH-USD".to_string(),
            correlation: dec!(0.8),
        });
        engine
    }

    fn setup_engine_no_correlation() -> EngineState {
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
        assert_eq!(portfolio_equity(account, &engine.markets), dec!(10000));
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
        let mut engine = setup_engine_no_correlation();
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
    fn test_hedged_portfolio_lower_margin_than_concentrated() {
        // This is the KEY test for correlation-adjusted margin.
        // Long BTC + Short ETH (hedge) should require LESS margin than
        // Long BTC + Long ETH (concentrated) when ρ > 0.
        let mut engine = setup_engine(); // ρ=0.8

        // Account 1: Hedged (long BTC, short ETH)
        engine.process_event(&Event::Deposit { account_id: 1, amount: dec!(100000) });
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

        // Account 2: Concentrated (long BTC, long ETH)
        engine.process_event(&Event::Deposit { account_id: 2, amount: dec!(100000) });
        engine.process_event(&Event::Trade {
            account_id: 2,
            market_id: "BTC-USD".to_string(),
            size: dec!(1),
            price: dec!(50000),
        });
        engine.process_event(&Event::Trade {
            account_id: 2,
            market_id: "ETH-USD".to_string(),
            size: dec!(15),
            price: dec!(3000),
        });

        let acct1 = engine.accounts.get(&1).unwrap();
        let acct2 = engine.accounts.get(&2).unwrap();

        let im_hedged = correlation_adjusted_margin(
            acct1, &engine.markets, &engine.correlations, true,
        );
        let im_concentrated = correlation_adjusted_margin(
            acct2, &engine.markets, &engine.correlations, true,
        );

        assert!(
            im_hedged < im_concentrated,
            "Hedged IM ({}) should be less than concentrated IM ({})",
            im_hedged, im_concentrated
        );

        // Both should have the same naive margin
        let naive1 = naive_margin_requirement(acct1, &engine.markets, true);
        let naive2 = naive_margin_requirement(acct2, &engine.markets, true);
        assert_eq!(naive1, naive2, "Naive margins should be equal");
    }

    #[test]
    fn test_correlation_change_affects_margin() {
        let mut engine = setup_engine(); // ρ=0.8

        engine.process_event(&Event::Deposit { account_id: 1, amount: dec!(100000) });
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

        let acct = engine.accounts.get(&1).unwrap();
        let im_high_corr = correlation_adjusted_margin(
            acct, &engine.markets, &engine.correlations, true,
        );

        // Reduce correlation to 0.3
        engine.process_event(&Event::SetCorrelation {
            market_a: "BTC-USD".to_string(),
            market_b: "ETH-USD".to_string(),
            correlation: dec!(0.3),
        });

        let acct = engine.accounts.get(&1).unwrap();
        let im_low_corr = correlation_adjusted_margin(
            acct, &engine.markets, &engine.correlations, true,
        );

        // Higher correlation → more discount for hedge → lower margin
        assert!(
            im_high_corr < im_low_corr,
            "Higher correlation should give more hedge discount: ρ=0.8→{} vs ρ=0.3→{}",
            im_high_corr, im_low_corr
        );
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
        let equity = portfolio_equity(account, &engine.markets);
        assert_eq!(equity, dec!(4750));
        assert!(!is_liquidatable(account, &engine.markets, &engine.correlations));
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

    #[test]
    fn test_set_correlation_event() {
        let mut engine = setup_engine_no_correlation();
        assert!(engine.correlations.is_empty());

        engine.process_event(&Event::SetCorrelation {
            market_a: "BTC-USD".to_string(),
            market_b: "ETH-USD".to_string(),
            correlation: dec!(0.75),
        });

        let key = CorrelationKey::new("BTC-USD", "ETH-USD");
        assert_eq!(engine.correlations.get(&key), Some(&dec!(0.75)));

        // Update it
        engine.process_event(&Event::SetCorrelation {
            market_a: "ETH-USD".to_string(), // Reversed order should work
            market_b: "BTC-USD".to_string(),
            correlation: dec!(0.9),
        });
        assert_eq!(engine.correlations.get(&key), Some(&dec!(0.9)));
    }
}
