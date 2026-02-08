# laminardb-fraud-detect

Real-time fraud detection system using [LaminarDB](https://laminardb.io) embedded streaming engine. Ingests synthetic market data, runs 6 concurrent detection streams with microsecond latency, and generates alerts for anomalous trading patterns.

## Detection Results

| Detection Stream | Window Type | Alert Type | Status |
|-----------------|-------------|------------|--------|
| Volume Baseline | HOP (2s slide, 10s window) | VolumeAnomaly | **PASS** |
| OHLC + Volatility | TUMBLE (5s) | PriceSpike | **PASS** |
| Rapid-Fire Burst | SESSION (2s gap) | RapidFire | **PASS** |
| Wash Trading Score | TUMBLE (5s) + CASE WHEN | WashTrading | **PASS** |
| Cross-Stream Match | INNER JOIN (10s window) | SuspiciousMatch | **PASS** |
| Front-Running | ASOF JOIN | FrontRunning | **PENDING** (awaiting crate v0.1.2, see [#57](https://github.com/laminardb/laminardb/issues/57)) |

## Latency (typical headless run, 15s @ 10% fraud rate)

| Stage | p50 | p95 | p99 |
|-------|-----|-----|-----|
| Push (push_batch) | ~150us | ~450us | ~525us |
| Processing (push → poll) | ~75us | ~365us | ~800us |
| Alert (gen → alert) | ~250us | ~1.1ms | ~1.4ms |

## Quick Start

```bash
# Headless mode (CI-friendly)
cargo run -- --mode headless --duration 15 --fraud-rate 0.1

# TUI dashboard
cargo run

# Web dashboard
cargo run -- --mode web --port 3000
```

## How It Works

```
 FraudGenerator (mock market data + fraud injection)
      │
      ├─ generate_cycle(ts) → trades + orders
      │   Normal: 5 trades/cycle (one per symbol), ~30% with matching orders
      │   Fraud:  volume spikes, price manipulation, rapid-fire, wash trading
      │
      ▼
┌─────────────────────────────────────────────────────────────┐
│  LaminarDB Embedded Pipeline (100ms micro-batch ticks)      │
│                                                             │
│  SOURCE: trades ──┬── HOP(2s,10s) ──► vol_baseline          │
│                   ├── TUMBLE(5s)  ──► ohlc_vol               │
│                   ├── SESSION(2s) ──► rapid_fire             │
│                   └── TUMBLE(5s)  ──► wash_score             │
│                                                             │
│  SOURCE: orders ──┐                                         │
│                   ├── INNER JOIN(trades×orders) ──►          │
│                   │   suspicious_match                       │
│                   └── ASOF JOIN(trades×orders) ──►           │
│                       asof_match (pending crate fix)         │
└─────────────────────────────────────────────────────────────┘
      │
      ▼
  AlertEngine (threshold scoring → severity classification)
      │
      ▼
  LatencyTracker (p50/p95/p99 microsecond precision)
      │
      ├─► TUI (ratatui dashboard)
      ├─► Web (axum + Chart.js + WebSocket)
      └─► Headless (stdout summary)
```

## Fraud Scenarios

| Scenario | Injection Method | Detection Stream | What Triggers |
|----------|-----------------|-----------------|---------------|
| Volume Spike | 5-10 trades at 10-50x normal volume | vol_baseline (HOP) | current > 2x rolling average |
| Price Manipulation | 2-4% push for 3 cycles, then 8% reversal | ohlc_vol (TUMBLE) | price_range/open > 0.2% |
| Rapid-Fire | 20-30 trades in <2s from fraud account | rapid_fire (SESSION) | burst_trades >= 5 |
| Wash Trading | Equal buy/sell pairs from same account | wash_score (TUMBLE) | imbalance < 0.3 with both sides >= 2 |
| Suspicious Match | Tight price matching on trade-order pairs | suspicious_match (JOIN) | \|price_diff\| < 1.0 |
| Front-Running | Trade follows order at similar price from different account | asof_match (ASOF JOIN) | \|price_spread\| < 0.5 |

## LaminarDB Features Used

All features are confirmed working from [laminardb-test](https://github.com/laminardb/laminardb-test):

- **Phase 1 (Rust API)**: `builder()`, `execute()`, `start()`, `source::<T>()`, `subscribe::<T>()`, `push_batch()`, `watermark()`, `poll()`
- **Phase 2 (Streaming SQL)**: `TUMBLE` windows, `tumble()` UDF, `first_value()`/`last_value()`, `SUM`, `COUNT`, `AVG`, `CASE WHEN`
- **Phase 4 (INNER JOIN)**: Stream-stream join with numeric `BETWEEN` time bounds
- **Phase 6 (HOP window)**: `HOP(ts, slide, size)` — rolling/sliding windows
- **Phase 6 (SESSION window)**: `SESSION(ts, gap)` — gap-based burst detection
- **ASOF JOIN** *(pending)*: `ASOF JOIN ... MATCH_CONDITION()` — temporal nearest-match (SQL parses, execution awaiting crate v0.1.2)

## Dependencies

Uses published crates from [crates.io](https://crates.io):

```toml
laminar-db = "0.1"       # Embedded streaming database
laminar-derive = "0.1"   # Record/FromRow derive macros
laminar-core = "0.1"     # Core engine (required by derive macro)
```

## Project Structure

```
src/
  main.rs          # Entry point + headless mode
  types.rs         # Record/FromRow structs (2 inputs, 6 outputs)
  generator.rs     # FraudGenerator with 4 fraud scenarios
  detection.rs     # LaminarDB pipeline (6 detection streams)
  alerts.rs        # AlertEngine with threshold scoring (6 alert types)
  latency.rs       # Microsecond latency tracking (p50/p95/p99)
  tui.rs           # Ratatui dashboard
  web.rs           # axum + WebSocket + Chart.js dashboard
docs/
  CONTEXT.md       # Session context and architecture decisions
  STEERING.md      # Priorities and test matrix
  DETECTION.md     # Fraud detection strategies explained
```

## License

[MIT](LICENSE)
