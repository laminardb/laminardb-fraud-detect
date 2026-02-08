# laminardb-fraud-detect

Real-time fraud detection system using [LaminarDB](https://laminardb.io) embedded streaming engine. Ingests synthetic market data, runs 6 concurrent detection streams with microsecond latency, and generates alerts for anomalous trading patterns.

## Detection Results

| Detection Stream | Window Type | Alert Type | Status |
|-----------------|-------------|------------|--------|
| Volume Baseline | HOP (2s slide, 10s window) | VolumeAnomaly | **PASS** |
| OHLC + Volatility | TUMBLE (5s) | PriceSpike | **PASS** |
| Rapid-Fire Burst | SESSION (2s gap) | RapidFire | **PASS** |
| Wash Trading Score | TUMBLE (5s) + CASE WHEN | WashTrading | **PASS** |
| Cross-Stream Match | INNER JOIN (2s window) | SuspiciousMatch | **PASS** |
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

# Stress test (7 load levels, 60s each)
cargo run --release -- --mode stress

# Quick stress test (10s per level)
cargo run --release -- --mode stress --level-duration 10

# Criterion benchmarks
cargo bench
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

## Stress Testing & Benchmarks

The `--mode stress` option runs a structured ramp test across 7 load levels (100 to 200K trades/sec target), measuring throughput and latency degradation at each level. It reports:

- Actual vs target throughput per level
- Push and processing latency percentiles (p50/p95/p99)
- Saturation point detection (where throughput drops below 90% of target)
- Peak sustained throughput

### Baseline Results (MacOS, release mode, 6-stream pipeline)

| Metric | Value |
|--------|-------|
| Peak sustained throughput | ~2,275 trades/sec |
| Saturation point | Level 3 (~1,000 target/sec) |
| Engine ceiling | ~2,275/sec (micro-batch tick-bound) |
| Debug mode overhead | ~31% slower (~1,736/sec) |

The engine ceiling is determined by LaminarDB's 100ms micro-batch tick rate, not SQL complexity. Reducing stream output (e.g., tighter JOIN windows) does not increase throughput — the tick loop is the bottleneck.

### Criterion Benchmarks

Criterion benchmarks (`cargo bench`) provide reproducible measurements:

| Benchmark | What it measures |
|-----------|-----------------|
| `push_throughput` | Raw `push_batch()` ingestion (100–5,000 trades) |
| `end_to_end` | Push + watermark + poll + alert evaluation |
| `pipeline_setup` | Time to create full 6-stream pipeline |

## Correctness Tests

12 tests covering all detection streams plus edge cases:

```bash
cargo test -- --nocapture
```

| Test | What it verifies |
|------|-----------------|
| `test_vol_baseline` | HOP window aggregation (total_volume, trade_count, avg_price) |
| `test_ohlc_vol` | TUMBLE OHLC values (open, high, low, close, price_range) |
| `test_rapid_fire` | SESSION burst aggregation (burst_trades, burst_volume) |
| `test_wash_score` | CASE WHEN buy/sell split (buy_volume, sell_volume, counts) |
| `test_suspicious_match` | INNER JOIN + price_diff computation |
| `test_asof_match` | Graceful skip if ASOF unavailable in crate v0.1.1 |
| `test_edge_empty_window_gap` | Pipeline doesn't stall with empty TUMBLE windows |
| `test_edge_late_data_not_dropped` | Documents: LaminarDB processes events behind watermark |
| `test_edge_single_trade_ohlc` | Single trade: open=high=low=close, range=0 |
| `test_edge_join_no_symbol_match` | INNER JOIN 0 rows when symbols differ |
| `test_edge_join_outside_time_window` | INNER JOIN 0 rows when 100s apart |
| `test_edge_wash_only_buys` | CASE WHEN: sell_volume=0, sell_count=0 |

### Known Behavioral Findings

| Finding | Details | Issue |
|---------|---------|-------|
| Late data NOT dropped | Events behind watermark are processed into window aggregations | [#65](https://github.com/laminardb/laminardb/issues/65) |
| SESSION emits per-tick | `rapid_fire` produces ~1:1 output ratio (not one row per session close) | — |
| ASOF JOIN 0 output | Stream creates OK but produces no rows in published crates v0.1.1 | [#57](https://github.com/laminardb/laminardb/issues/57) |
| Engine ceiling | ~2,275/sec regardless of SQL complexity — micro-batch tick-bound | — |

## CI Pipeline

GitHub Actions runs on every push to `master`:

1. **Build** — `cargo build --release`
2. **Correctness tests** — 12 tests (6 stream + 6 edge case)
3. **Headless integration** — 30s at 10% fraud rate, verifies 3+ alert types fire
4. **Stress test** — 7 load levels (10s each), throughput + latency results
5. **Criterion benchmarks** — push, end-to-end, and pipeline setup measurements

All results are published to the GitHub Actions step summary.

## Project Structure

```
src/
  main.rs          # Entry point + headless mode
  types.rs         # Record/FromRow structs (2 inputs, 6 outputs)
  generator.rs     # FraudGenerator with 4 fraud scenarios
  detection.rs     # LaminarDB pipeline (6 detection streams)
  alerts.rs        # AlertEngine with threshold scoring (6 alert types)
  latency.rs       # Microsecond latency tracking (p50/p95/p99)
  stress.rs        # Stress test runner (7 load levels + saturation detection)
  tui.rs           # Ratatui dashboard
  web.rs           # axum + WebSocket + Chart.js dashboard
tests/
  correctness.rs   # 12 correctness + edge case tests
benches/
  throughput.rs    # Criterion benchmarks (push, end-to-end, setup)
docs/
  CONTEXT.md       # Session context and architecture decisions
  STEERING.md      # Priorities and test matrix
  DETECTION.md     # Fraud detection strategies explained
  ARCHITECTURE.md  # System diagrams and data flow
```

## License

[MIT](LICENSE)
