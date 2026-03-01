# Design Document: Deterministic Cross-Margin Risk Engine

**Author:** Alyazeed Basyoni
**Date:** 2026-03-01

## 1. Overview

This document outlines the architectural design for a deterministic, off-chain, cross-margin risk engine for a perpetuals exchange, as required by the take-home assignment. The system is designed to be minimal, robust, and verifiable, with a primary focus on correctness and deterministic replay.

The chosen implementation language is **Rust**. Its strong type system, memory safety guarantees without a garbage collector, and focus on performance make it an excellent choice for building a financial engine where precision, predictability, and resource control are paramount. Determinism is achieved through an event-sourcing architecture and the exclusive use of fixed-point arithmetic.

A key design feature is **correlation-adjusted margin netting**: the engine recognizes that a portfolio long BTC and short ETH (a hedge) carries less risk than a portfolio long both BTC and ETH (a concentration), and adjusts margin requirements accordingly using a configurable correlation parameter.

## 2. System Architecture

The engine is built on an **event-sourcing** pattern. The state of the entire system (all accounts, positions, and markets) is derived solely by applying a strictly ordered sequence of events from an event log. This design inherently guarantees determinism: given the same initial state and the same event log, the final state will always be identical.

### 2.1. Module Structure

The codebase is organized into four modules:

| Module       | Responsibility                                                                 |
|-------------|--------------------------------------------------------------------------------|
| `types.rs`  | Core data types: `Position`, `Account`, `MarketConfig`, `Event`, `EventResult` |
| `margin.rs` | All margin computation logic, including correlation-adjusted netting            |
| `engine.rs` | Event processing, state management, liquidation execution                      |
| `main.rs`   | Demo scenarios, event log construction, integration tests                      |

### 2.2. Core Components

-   **Event Log:** An ordered, append-only list of all actions that can mutate the system state (e.g., `Deposit`, `Trade`, `MarkPriceUpdate`, `SetCorrelation`). This is the single source of truth.
-   **Engine:** The central processing unit. It consumes events one by one and applies them to the current state, producing a new state. It contains all the business logic for margin calculation, risk checks, and liquidation.
-   **State:** A snapshot of all accounts, collateral, positions, market data, and the correlation matrix at a specific point in time. It is entirely reconstructible from the event log.

### 2.3. Data Flow

The data flow is unidirectional and simple, ensuring predictability.

```
Event Log ──stream of events──▶ Risk Engine ──applies event──▶ State
                                     ▲                           │
                                     └───────new state───────────┘

State contains:
  ├── Accounts (BTreeMap<AccountId, Account>)
  ├── Markets  (BTreeMap<MarketId, MarketConfig>)
  └── Correlations (BTreeMap<CorrelationKey, Decimal>)
```

## 3. State Model

The state is modeled using a set of core data structures. All monetary and price values are represented using a fixed-point decimal type (`rust_decimal::Decimal`) to prevent floating-point inaccuracies and ensure determinism.

-   **`EngineState`**: The top-level container for the entire system state.
    -   `accounts`: A `BTreeMap` from `AccountId` to `Account`. `BTreeMap` is used over `HashMap` to guarantee deterministic iteration order.
    -   `markets`: A `BTreeMap` from `MarketId` to `MarketConfig`.
    -   `correlations`: A `BTreeMap` from `CorrelationKey` to `Decimal`, representing the pairwise correlation matrix between markets.

-   **`Account`**: Represents a single trader's portfolio.
    -   `collateral`: The amount of collateral (e.g., USDC) held by the user. This balance includes all realized PnL from closed positions or partial closes.
    -   `positions`: A `BTreeMap` from `MarketId` to `Position`, representing the user's holdings in each market.

-   **`Position`**: A user's position in a single market.
    -   `size`: The quantity of contracts. Positive for a long position, negative for a short.
    -   `entry_price`: The volume-weighted average price (VWAP) of the current open position.

-   **`MarketConfig`**: Represents a single perpetual market's parameters.
    -   `mark_price`: The current price of the underlying asset, used for all PnL and margin calculations.
    -   `im_fraction`: The Initial Margin fraction (e.g., 5%).
    -   `mm_fraction`: The Maintenance Margin fraction (e.g., 2.5%).

-   **`CorrelationKey`**: An order-independent pair of market IDs (always stored as `(min, max)` lexicographically). Serializes as a string `"MARKET_A/MARKET_B"` for JSON compatibility.

-   **`Event`**: An enum representing all possible state mutations.
    -   `CreateMarket { ... }`
    -   `Deposit { ... }`
    -   `Withdrawal { ... }`
    -   `Trade { ... }`
    -   `MarkPriceUpdate { ... }`
    -   `FundingPayment { ... }`
    -   `SetCorrelation { market_a, market_b, correlation }` — dynamically update the correlation between two markets

## 4. Core Logic & Computations

All risk is calculated at the portfolio level, allowing positions to collateralize each other.

### 4.1. Portfolio Equity

-   **Unrealized PnL**: For each position, `PnL = size × (mark_price - entry_price)`. The total Unrealized PnL is the sum across all of an account's positions.
-   **Portfolio Equity**: `collateral + Σ unrealized_pnl(position_i)`.

### 4.2. Naive Margin (Per-Market Sum)

The naive margin requirement is the simple additive sum of per-market requirements:

```
naive_IM = Σ |size_i × mark_price_i| × im_fraction_i
naive_MM = Σ |size_i × mark_price_i| × mm_fraction_i
```

This treats every position independently. It does **not** account for the fact that correlated assets in opposite directions partially offset each other's risk.

### 4.3. Correlation-Adjusted Margin (The Key Innovation)

The problem with naive margin: a portfolio that is long BTC and short ETH (a hedge, since BTC and ETH are ~85% correlated) is assigned the **same margin** as a portfolio long BTC and long ETH (a concentrated directional bet). This is economically wrong — the hedge carries far less risk.

The correlation-adjusted model fixes this by computing a **netting discount** (or **concentration surcharge**) for each pair of positions:

```
For each pair of positions (i, j) where i < j:

  overlapping_notional = min(|notional_i|, |notional_j|)
  avg_fraction = (fraction_i + fraction_j) / 2

  direction_factor:
    +1  if positions are in opposite directions (hedge)
    -1  if positions are in the same direction (concentration)

  adjustment(i,j) = ρ(i,j) × overlapping_notional × avg_fraction × direction_factor

adjusted_margin = naive_margin - Σ adjustment(i,j)
                = naive_margin - Σ ρ(i,j) × min(|N_i|, |N_j|) × avg_frac × dir_factor

Clamped to: max(adjusted_margin, 0)
```

**Intuition:**

| Portfolio | Direction Factor | Correlation | Effect |
|-----------|:---:|:---:|--------|
| Long BTC + Short ETH | +1 (hedge) | ρ = 0.85 | **Margin discount** (~34%) |
| Long BTC + Long ETH | -1 (concentrated) | ρ = 0.85 | **Margin surcharge** (~34%) |
| Long BTC + Short SOL | +1 (hedge) | ρ = 0.0 | No adjustment |

**Concrete example from the demo (Account 3, ρ=0.5):**

```
Positions: Long 1 BTC @ $40,000, Short 15 ETH @ $2,300
Notional:  BTC = $40,000, ETH = $34,500

Naive IM = $40,000 × 0.05 + $34,500 × 0.05 = $2,000 + $1,725 = $3,725

Overlap = min($40,000, $34,500) = $34,500
Avg fraction = (0.05 + 0.05) / 2 = 0.05
Direction factor = +1 (opposite directions → hedge)

Adjustment = 0.5 × $34,500 × 0.05 × 1 = $862.50

Adjusted IM = $3,725 - $862.50 = $2,862.50 (23.2% discount)
```

The correlation parameter `ρ` is fully configurable per market pair via the `SetCorrelation` event, and can be updated dynamically during the event stream.

### 4.4. Risk Checks for New Trades

To ensure the system remains solvent, a risk check is performed before executing any new trade. The trade is only permitted if the account will have sufficient collateral to meet the **correlation-adjusted** initial margin requirement *after* the trade.

1.  A temporary, post-trade `Account` state is created in memory (clone + simulate).
2.  The `Portfolio Equity` and `Correlation-Adjusted IM` are calculated based on this hypothetical state.
3.  The trade is **allowed** if and only if `Post-Trade Equity >= Post-Trade Adjusted IM`.

This means a hedge trade (which reduces adjusted IM) is easier to enter than a concentration trade (which increases adjusted IM), correctly reflecting the risk economics.

### 4.5. Liquidation Logic

An account is flagged for liquidation if its equity falls below the **correlation-adjusted** maintenance margin requirement.

-   **Liquidation Trigger**: `Portfolio Equity < Adjusted MM`.
-   **Process**: This check is performed after any `MarkPriceUpdate` event.
-   **Simplified Execution**: All of the account's positions are closed at the current mark prices, and the realized PnL is added to the account's collateral. In a real system, this would involve a more complex process with liquidation engines, insurance funds, and fees.

## 5. Determinism Guarantee

Determinism is the most critical requirement of the system. It is guaranteed by:

1.  **Event-Sourced Architecture**: The state is a pure function of the event log. There are no side effects in the state transition logic.
2.  **Fixed-Point Arithmetic**: The `rust_decimal::Decimal` type is used for all calculations. This avoids the non-deterministic nature of floating-point numbers (IEEE 754) and ensures that calculations produce the exact same result every time, regardless of the underlying hardware.
3.  **Deterministic Iteration**: `BTreeMap` is used for all key-value stores (accounts, positions, markets, correlations). This ensures that operations that iterate over these maps (e.g., calculating total notional, iterating over position pairs for correlation adjustment, checking for liquidations) do so in the same order every time.
4.  **No External Dependencies**: The core risk engine logic has no I/O (network, disk, etc.) or other external dependencies that could introduce non-determinism.

## 6. How to Run

### 6.1. Build and Run

```bash
# Build the project
cargo build

# Run the demo simulation
cargo run

# Run the full test suite (20 tests)
cargo test
```

### 6.2. Demo Scenarios

The `main` function executes a hardcoded event log that demonstrates:

1.  **Liquidation**: Account 1 (long BTC + long ETH, concentrated) becomes liquidatable after adverse price movement. The concentration surcharge makes this happen sooner.
2.  **Trade Rejection**: Account 2's second trade is rejected because the correlation-adjusted IM (with concentration penalty) exceeds equity.
3.  **Correlation Benefit**: Account 3 (long BTC + short ETH, hedged) survives the same price drop with a 23% margin discount.
4.  **Dynamic Correlation Update**: The correlation is changed from ρ=0.85 to ρ=0.5 mid-stream, and the margin adjusts accordingly.
5.  **Replay Determinism**: The entire event log is processed twice, and the final state hashes are compared to ensure they are identical.

## 7. How AI Was Used

AI (Manus) was used as a development accelerator throughout this project. Specifically:

-   **Architecture & Design**: I outlined the high-level architecture (event-sourcing, fixed-point arithmetic, cross-margin model) and used AI to help flesh out the detailed state model and formalize the margin computation logic.
-   **Code Generation**: AI assisted with scaffolding the Rust implementation — the data structures, event processing loop, and margin calculations. I reviewed, refined, and tested the generated code to ensure correctness.
-   **Test Cases**: AI helped generate the initial test suite, which I extended and validated against hand-calculated expected values.
-   **Documentation**: AI drafted the README and this design document based on my architectural decisions, which I then reviewed and edited.

All architectural decisions, tradeoff reasoning, and the overall design direction are my own. AI served as a productivity tool to accelerate implementation within the time constraint.

## 8. Simplifications and Tradeoffs

To meet the time constraints of the assignment, several simplifications were made:

-   **Pairwise Correlation Only**: The correlation model considers pairwise relationships between markets. A production system might use a full covariance matrix or scenario-based margining (like SPAN or CME's approach) for more accurate portfolio risk assessment.
-   **Static Correlation Input**: Correlations are set via events and assumed to be externally computed. A production system would compute rolling correlations from historical price data.
-   **No Funding Rates**: The model includes a `FundingPayment` event, but does not implement the logic to calculate funding rates. This would typically involve tracking a premium between the perpetual's mark price and the underlying's index price.
-   **Simplified Liquidation**: The liquidation process simply closes positions at the current mark price without simulating slippage, fees, or an insurance fund. A real system would have a more robust liquidation mechanism.
-   **Single Collateral Type**: The engine assumes a single asset (e.g., USDC) is used for collateral.
-   **No Order Book**: Trades are modeled as `Trade` events with a given size and price, abstracting away the order book and matching engine.
-   **Simplified State Hashing**: The deterministic state hash is a simple byte-level calculation for demo purposes. A production system would use a cryptographically secure hash like SHA-256.

## 9. Future Extensions

If this were to evolve into a production system, the following enhancements would be prioritized:

1.  **Scenario-Based Margining (SPAN-like)**: Run the portfolio through a matrix of stress scenarios (e.g., BTC ±10%, ETH ±10%, correlation shock) and set margin to the worst-case loss. This is more robust than pairwise netting.
2.  **Multi-Asset Collateral**: Support collateral in multiple currencies/tokens with haircuts and dynamic pricing.
3.  **Partial Liquidation**: Instead of closing all positions, partially reduce the largest position until the account is above maintenance margin.
4.  **Insurance Fund & Socialized Loss**: Handle underwater accounts after liquidation.
5.  **Dynamic Correlation Estimation**: Compute rolling correlations from a price feed, with configurable lookback windows and decay factors.
