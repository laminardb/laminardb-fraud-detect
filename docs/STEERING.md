# Steering

> Priorities, test matrix, and next steps.

## Current Priority

1. Get all 5 detection streams producing alerts in headless mode
2. TUI dashboard with alert feed + latency display
3. Web dashboard (axum + Chart.js + WebSocket)
4. CI workflow
5. Documentation

## Test Matrix

| Component | Status | Notes |
|-----------|--------|-------|
| **Detection Streams** | | |
| vol_baseline (HOP) | PASS | Rolling 10s window, 2s slide |
| ohlc_vol (TUMBLE) | PASS | 5s OHLC bars with price_range |
| rapid_fire (SESSION) | PASS | 2s gap-based burst detection |
| wash_score (TUMBLE + CASE WHEN) | PASS | Buy/sell imbalance per account |
| suspicious_match (INNER JOIN) | PASS | Trade-order correlation, 10s window |
| **Alert Types** | | |
| VolumeAnomaly | PASS | Triggers on 2x+ rolling average |
| PriceSpike | TUNING | Threshold at 0.2% range/open |
| RapidFire | PASS | Triggers on >= 5 trades per session |
| WashTrading | PASS | Triggers on imbalance < 0.3 |
| SuspiciousMatch | PASS | Triggers on \|price_diff\| < 1.0 |
| **Fraud Injection** | | |
| VolumeSpike | PASS | 5-10 trades at 10-50x volume |
| PriceManipulation | PASS | 2-4% push × 3 cycles + 8% reversal |
| RapidFire | PASS | 20-30 trades spaced 50-100ms |
| WashTrading | PASS | 3-6 equal buy/sell pairs |
| **Modes** | | |
| Headless | PASS | stdout summary with latency stats |
| TUI | PENDING | Ratatui dashboard |
| Web | PENDING | axum + Chart.js + WebSocket |
| **Infrastructure** | | |
| Published crate deps | PASS | laminar-db 0.1, laminar-derive 0.1, laminar-core 0.1 |
| CI workflow | PENDING | GitHub Actions |

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| LaminarDB dependency | Published crates (crates.io) | Self-contained, no sibling clone needed |
| Web framework | axum + Chart.js | Simpler than WASM framework, same real-time value |
| Window types | HOP + SESSION + TUMBLE | All confirmed working in laminardb-test Phase 6 |
| Join type | INNER JOIN only | ASOF JOIN doesn't work in embedded mode |
| Fraud accounts | Separate FRAUD-XX accounts | Makes wash trading + rapid-fire detection cleaner |
| Watermark advance | ts + 10_000 (10s) | Covers longest window (HOP 10s) |

## Next Steps

- [ ] Verify PriceSpike alerts fire with tuned threshold
- [ ] Implement TUI dashboard (modeled on laminardb-test tui.rs)
- [ ] Implement web dashboard (axum + WebSocket + Chart.js)
- [ ] Add CI workflow (.github/workflows/ci.yml)
- [ ] Create GitHub repo under laminardb org
