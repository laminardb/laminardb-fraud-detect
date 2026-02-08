# laminardb-fraud-detect

Real-time fraud detection using LaminarDB embedded streaming engine.

## Read First

- `docs/CONTEXT.md` — architecture decisions and session learnings
- `docs/STEERING.md` — priorities and test matrix
- `docs/DETECTION.md` — fraud detection strategies

## Quick Reference

```bash
cargo run                                           # TUI dashboard
cargo run -- --mode headless --duration 15          # CI mode
cargo run -- --mode web --port 3000                 # Web dashboard
cargo run -- --mode headless --fraud-rate 0.2       # Higher fraud rate
cargo run --release -- --mode stress                # Stress test (7 levels, 60s each)
cargo run --release -- --mode stress --level-duration 10  # Quick stress test
cargo bench                                         # Criterion benchmarks
```

## Key Files

| File | Purpose |
|------|---------|
| `src/detection.rs` | LaminarDB pipeline — 2 sources, 6 detection streams |
| `src/generator.rs` | FraudGenerator — mock data + 4 fraud injection scenarios |
| `src/alerts.rs` | AlertEngine — threshold scoring, severity classification |
| `src/types.rs` | Record/FromRow structs matching SQL column order |
| `src/latency.rs` | Microsecond tracking with percentile computation |
| `src/stress.rs` | Stress test runner — 7 load levels, saturation detection |
| `tests/correctness.rs` | 12 tests — 6 stream correctness + 6 edge cases |
| `benches/throughput.rs` | Criterion benchmarks — push, end-to-end, setup |

## LaminarDB SQL Gotchas

These are critical — learned from laminardb-test:

- Use `tumble()` not `TUMBLE_START()` — that's the registered UDF name
- Use `first_value()`/`last_value()` not `FIRST()`/`LAST()`
- `CAST(tumble(...) AS BIGINT)` for i64 fields — tumble returns Timestamp(Millisecond)
- Numeric time arithmetic only: `ts + 60000`, NOT `ts + INTERVAL '1' MINUTE`
- Source columns need `NOT NULL`
- `#[derive(Record)]` for inputs, `#[derive(FromRow)]` for outputs
- `laminar-core` must be a direct dependency (derive macro references it at compile time)
- FromRow field order must match SQL SELECT column order exactly
- `CREATE SINK` before `db.start()`, then `db.subscribe()` after start

## What Does NOT Work (in published crates v0.1.1)

- ASOF JOIN — SQL parses with `MATCH_CONDITION()` syntax, stream creates, but produces no output rows. Works with local path deps (see [#57](https://github.com/laminardb/laminardb/issues/57)). Code is wired up and will activate automatically once crate is updated.
- INTERVAL arithmetic on BIGINT — use numeric constants
- EMIT ON WINDOW CLOSE — no effect in micro-batch model
- CDC replication — connector stub, no actual I/O

## Known Behavioral Findings

- **Late data NOT dropped** — watermark does not filter late events; events behind watermark are processed into window aggregations ([#65](https://github.com/laminardb/laminardb/issues/65))
- **SESSION emits per-tick** — `rapid_fire` produces ~1:1 output ratio, not one row per session close
- **Engine ceiling ~2,275/sec** — 6-stream pipeline saturates at micro-batch tick rate, not SQL complexity

## Architecture

Single LaminarDB instance with 100ms micro-batch ticks:
1. FraudGenerator produces trades + orders each cycle
2. push_batch() + watermark() feeds both sources
3. Six detection streams run in parallel (5 active + 1 ASOF pending crate fix)
4. poll() retrieves results, AlertEngine scores each output
5. LatencyTracker measures push/processing/alert latency
6. Stress mode: 7 ramp levels with saturation detection (~2,275/sec ceiling)
