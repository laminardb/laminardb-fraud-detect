# Steering

> Priorities, test matrix, and next steps.

## Current Priority

1. ~~Get all 6 detection streams producing alerts~~ DONE
2. ~~TUI dashboard with alert feed + latency display~~ DONE
3. ~~Web dashboard (axum + Chart.js + WebSocket)~~ DONE
4. ~~CI workflow~~ DONE
5. ~~Stress test & benchmarking~~ DONE
6. ~~Edge case tests (empty windows, late data, NULL handling)~~ DONE (12 tests)
7. Compare laminardb-test (path deps) results with published crate baseline

## Test Matrix

| Component | Status | Notes |
|-----------|--------|-------|
| **Detection Streams** | | |
| vol_baseline (HOP) | PASS | Rolling 10s window, 2s slide |
| ohlc_vol (TUMBLE) | PASS | 5s OHLC bars with price_range |
| rapid_fire (SESSION) | PASS | 2s gap-based burst detection |
| wash_score (TUMBLE + CASE WHEN) | PASS | Buy/sell imbalance per account |
| suspicious_match (INNER JOIN) | PASS | Trade-order correlation, 2s window |
| asof_match (ASOF JOIN) | PENDING | Stream creates OK, 0 output (awaiting crate v0.1.2, [#57](https://github.com/laminardb/laminardb/issues/57)) |
| **Alert Types** | | |
| VolumeAnomaly | PASS | Triggers on 2x+ rolling average |
| PriceSpike | PASS | Threshold at 0.2% range/open |
| RapidFire | PASS | Triggers on >= 5 trades per session |
| WashTrading | PASS | Triggers on imbalance < 0.3 |
| SuspiciousMatch | PASS | Triggers on \|price_diff\| < 1.0 |
| FrontRunning | PENDING | Depends on ASOF JOIN producing output |
| **Fraud Injection** | | |
| VolumeSpike | PASS | 5-10 trades at 10-50x volume |
| PriceManipulation | PASS | 2-4% push × 3 cycles + 8% reversal |
| RapidFire | PASS | 20-30 trades spaced 50-100ms |
| WashTrading | PASS | 3-6 equal buy/sell pairs |
| **Modes** | | |
| Headless | PASS | stdout summary with latency stats |
| TUI | PASS | Ratatui dashboard with alert feed + latency |
| Web | PASS | axum + Chart.js + WebSocket |
| Stress | PASS | 7 ramp levels, saturation detection |
| **Infrastructure** | | |
| Published crate deps | PASS | laminar-db 0.1, laminar-derive 0.1, laminar-core 0.1 |
| CI workflow | PASS | GitHub Actions (Linux), build + test + headless + stress + bench |
| Criterion benchmarks | PASS | push, end-to-end, pipeline setup |
| **Correctness Tests** | | |
| vol_baseline | PASS | Deterministic HOP aggregation |
| ohlc_vol | PASS | OHLC values + price_range |
| rapid_fire | PASS | SESSION burst aggregation |
| wash_score | PASS | CASE WHEN buy/sell split |
| suspicious_match | PASS | INNER JOIN + price_diff |
| asof_match | PASS* | Gracefully skips if ASOF unavailable |
| **Edge Case Tests** | | |
| empty_window_gap | PASS | Pipeline doesn't stall with empty TUMBLE windows |
| late_data_not_dropped | PASS | Documents: events behind watermark processed ([#65](https://github.com/laminardb/laminardb/issues/65)) |
| single_trade_ohlc | PASS | Single trade: open=high=low=close, range=0 |
| join_no_symbol_match | PASS | INNER JOIN 0 rows when symbols differ |
| join_outside_time_window | PASS | INNER JOIN 0 rows when 100s apart |
| wash_only_buys | PASS | CASE WHEN: sell_volume=0, sell_count=0 |

## Stress Test Baseline (MacOS, release mode)

- **Peak sustained throughput**: ~2,275 trades/sec (6-stream pipeline)
- **Saturation point**: Level 3 (~1,000 target/sec)
- **Engine ceiling**: ~2,275/sec regardless of load — micro-batch tick-bound
- **Debug mode**: ~1,736/sec (~31% slower than release)

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| LaminarDB dependency | Published crates (crates.io) | Self-contained, no sibling clone needed |
| Web framework | axum + Chart.js | Simpler than WASM framework, same real-time value |
| Window types | HOP + SESSION + TUMBLE | All confirmed working in laminardb-test Phase 6 |
| Join types | INNER JOIN + ASOF JOIN | ASOF creates OK but 0 output in v0.1.1 ([#57](https://github.com/laminardb/laminardb/issues/57)) |
| INNER JOIN window | 2s (was 10s) | Reduced from 10s to control fan-out in stress test |
| Timestamp step | Constant 50ms | Fair benchmarking — same JOIN fan-out at all load levels |
| Fraud accounts | Separate FRAUD-XX accounts | Makes wash trading + rapid-fire detection cleaner |
| Watermark advance | event_ts + cycle_span + 10_000 | Sequential per-cycle, no cross-cycle overlap |

## Behavioral Findings

| Finding | Details |
|---------|---------|
| Late data NOT dropped | v0.1.1 processes events behind watermark (test: `test_edge_late_data_not_dropped`, [#65](https://github.com/laminardb/laminardb/issues/65)) |
| SESSION emits per-tick | rapid_fire produces ~1:1 output (76K rows from 78K trades in stress test) |
| ASOF JOIN 0 output | Stream creates OK but poll() returns nothing ([#57](https://github.com/laminardb/laminardb/issues/57)) |
| Engine ceiling ~2,275/sec | 6-stream pipeline saturates at micro-batch tick rate, not SQL complexity |

## Next Steps

- [x] Edge case tests: empty windows, late data, NULL handling (12 tests passing)
- [x] Compare laminardb-test (path deps) vs published crate throughput (+1% — negligible)
- [ ] Compare Mac vs Ubuntu CI throughput numbers (awaiting CI run with stress + bench)
- [x] Update README with benchmark baseline numbers and correctness test table
