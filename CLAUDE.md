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
```

## Key Files

| File | Purpose |
|------|---------|
| `src/detection.rs` | LaminarDB pipeline — 2 sources, 5 detection streams |
| `src/generator.rs` | FraudGenerator — mock data + 4 fraud injection scenarios |
| `src/alerts.rs` | AlertEngine — threshold scoring, severity classification |
| `src/types.rs` | Record/FromRow structs matching SQL column order |
| `src/latency.rs` | Microsecond tracking with percentile computation |

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

## What Does NOT Work

- ASOF JOIN — DataFusion limitation in embedded mode
- INTERVAL arithmetic on BIGINT — use numeric constants
- EMIT ON WINDOW CLOSE — no effect in micro-batch model
- CDC replication — connector stub, no actual I/O

## Architecture

Single LaminarDB instance with 100ms micro-batch ticks:
1. FraudGenerator produces trades + orders each cycle
2. push_batch() + watermark() feeds both sources
3. Five detection streams run in parallel via DataFusion ctx.sql()
4. poll() retrieves results, AlertEngine scores each output
5. LatencyTracker measures push/processing/alert latency
