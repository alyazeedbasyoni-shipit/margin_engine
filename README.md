# Deterministic Cross-Margin Perpetual Risk Engine

A lightweight, deterministic risk engine for cross-margin perpetual futures with **correlation-adjusted margin netting**. Built in Rust for precision, performance, and reproducibility.

## Key Features

- **Event-sourced architecture** — state is a pure function of the event log, guaranteeing deterministic replay
- **Correlation-adjusted margin** — hedged portfolios (long BTC / short ETH) get margin discounts; concentrated portfolios get surcharges
- **Configurable correlation parameter** — set and update ρ per market pair via `SetCorrelation` events
- **Fixed-point arithmetic** — `rust_decimal` eliminates floating-point non-determinism
- **Deterministic iteration** — `BTreeMap` throughout for reproducible ordering

## Quick Start

```bash
# Build
cargo build

# Run the demo (4 scenarios + determinism verification)
cargo run

# Run the test suite (20 tests)
cargo test
```

## Project Structure

```
src/
├── types.rs    # Core data types: Position, Account, MarketConfig, Event, EventResult
├── margin.rs   # Margin computation: naive + correlation-adjusted netting
├── engine.rs   # Event processing, state management, liquidation
└── main.rs     # Demo scenarios and integration tests
```

## Demo Scenarios

The demo processes 18 events and demonstrates:

1. **Liquidation** — Account with concentrated long BTC + long ETH positions is liquidated after price drop (concentration surcharge makes this happen sooner)
2. **Trade Rejection** — Second trade rejected because correlation-adjusted IM exceeds equity
3. **Hedge Benefit** — Account with long BTC + short ETH survives same price drop (23% margin discount at ρ=0.5)
4. **Dynamic Correlation** — Correlation updated mid-stream from ρ=0.85 to ρ=0.5
5. **Replay Determinism** — Both runs produce identical state hash

## Correlation-Adjusted Margin

The engine recognizes that correlated assets in opposite directions offset risk:

| Portfolio | ρ(BTC,ETH) | Margin Effect |
|-----------|:---:|---|
| Long BTC + Short ETH | 0.85 | ~34% discount (hedge) |
| Long BTC + Long ETH | 0.85 | ~34% surcharge (concentration) |
| Long BTC + Short SOL | 0.00 | No adjustment |

See [DESIGN.md](DESIGN.md) for the full formula and worked examples.

## Event Types

| Event | Description |
|-------|-------------|
| `CreateMarket` | Register a new perpetual market with IM/MM fractions |
| `Deposit` | Add collateral to an account |
| `Withdrawal` | Remove collateral (rejected if it would breach IM) |
| `Trade` | Execute a fill (rejected if post-trade equity < adjusted IM) |
| `MarkPriceUpdate` | Update mark price; triggers liquidation checks |
| `FundingPayment` | Apply funding to an account's collateral |
| `SetCorrelation` | Set/update the correlation coefficient between two markets |

## Output Files

After running, the engine produces:
- `event_log.json` — The full event log (can be replayed)
- `final_state.json` — Serialized final state snapshot

## Design Document

See [DESIGN.md](DESIGN.md) for the comprehensive design document covering architecture, correlation-adjusted margin formula, tradeoffs, and future extensions.
