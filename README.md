# Deterministic Cross-Margin Perpetual Risk Engine

A minimal, off-chain cross-margin risk and liquidation engine for multiple perpetual markets sharing a single collateral pool. Built in Rust for determinism, precision, and performance.

## Quick Start

```bash
# Build
cargo build

# Run the demo
cargo run

# Run tests
cargo test
```

## What the Demo Shows

The demo processes a sequence of 16 events and demonstrates three key scenarios:

1. **Liquidation** — Account 1 deposits $10,000, opens long positions in BTC and ETH, then gets liquidated when both prices drop sharply (equity falls below maintenance margin).

2. **Trade Rejection** — Account 2 deposits $5,000, opens a leveraged BTC position, then attempts a second trade that would push total initial margin above equity. The trade is rejected.

3. **Cross-Margin Hedging** — Account 3 holds a hedged portfolio (long BTC, short ETH). When both assets drop, the hedge limits losses and the account remains healthy.

After processing all events, the engine replays the same event log from scratch and verifies that the final state hash is identical — confirming deterministic replay.

## Architecture

- **Event-Sourced**: All state is derived from an ordered event log. No side effects.
- **Fixed-Point Arithmetic**: Uses `rust_decimal` for exact decimal math — no floating-point non-determinism.
- **Deterministic Ordering**: `BTreeMap` used throughout for deterministic iteration order.
- **Portfolio-Level Risk**: Margin is computed across all positions, not per-market.

## Event Types

| Event | Description |
|-------|-------------|
| `CreateMarket` | Register a new perpetual market with IM/MM fractions |
| `Deposit` | Add collateral to an account |
| `Withdrawal` | Remove collateral (rejected if it would breach IM) |
| `Trade` | Execute a fill (rejected if post-trade equity < IM) |
| `MarkPriceUpdate` | Update mark price; triggers liquidation checks |
| `FundingPayment` | Apply funding to an account's collateral |

## Margin Model

- **Initial Margin (IM)**: Sum of `|position_size * mark_price| * im_fraction` across all markets
- **Maintenance Margin (MM)**: Sum of `|position_size * mark_price| * mm_fraction` across all markets
- **Portfolio Equity**: `collateral + total_unrealized_pnl`
- **Liquidation Trigger**: `equity < MM`
- **Trade Gate**: `post_trade_equity >= post_trade_IM`

## Output Files

After running, the engine produces:
- `event_log.json` — The full event log (can be replayed)
- `final_state.json` — Serialized final state snapshot

## Design Document

See [DESIGN.md](./DESIGN.md) for the full design document covering architecture, tradeoffs, and simplifications.
