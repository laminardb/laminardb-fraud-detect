# Context

> Architecture decisions, session learnings, and design rationale.

## Why This Project Exists

laminardb-test validates that each LaminarDB feature works in isolation. This project demonstrates a **real-world use case** — combining multiple proven features into a single fraud detection pipeline that processes market data with microsecond latency.

## Architecture Decisions

### Single LaminarDB Instance, Multiple Streams

We run all 5 detection streams on one `LaminarDB` instance rather than separate instances per detector. This means:
- All streams share the same 100ms micro-batch tick
- One `push_batch()` call feeds all streams simultaneously
- Memory is shared (single Arrow MemTable per source per tick)

Trade-off: if one stream's SQL is slow, it delays all others within the same tick.

### Two Sources, Not One

We use separate `trades` and `orders` sources because the INNER JOIN requires two distinct MemTables. The join condition uses `o.ts BETWEEN t.ts - 10000 AND t.ts + 10000` (numeric, not INTERVAL) because ts is BIGINT.

### CASE WHEN for Wash Trading

DataFusion supports `CASE WHEN` inside aggregate functions, which we use to compute buy/sell volumes in a single stream:
```sql
SUM(CASE WHEN side = 'buy' THEN volume ELSE CAST(0 AS BIGINT) END) AS buy_volume
```
The fallback plan was two separate streams, but CASE WHEN works correctly.

### Alert Thresholds Are Tuned for Demo

The default thresholds are set to trigger all 5 alert types during a 15-second demo run at 10% fraud rate. Production systems would need:
- Longer baseline windows (minutes, not seconds)
- Dynamic thresholds based on historical patterns
- Multi-factor scoring combining multiple signals

### Published Crates, Not Path Dependencies

Unlike laminardb-test (which uses path deps to `../laminardb/crates/`), this project uses published crates from crates.io:
```toml
laminar-db = "0.1"
laminar-derive = "0.1"
laminar-core = "0.1"
```
This makes the project fully self-contained — no sibling directory needed.

## Key Learnings

### From laminardb-test

- HOP, SESSION, TUMBLE all work in embedded mode (confirmed Phase 6)
- INNER JOIN works with numeric BETWEEN (confirmed Phase 4)
- Cascading MVs work (confirmed Phase 2, fixed in laminardb#35)
- ASOF JOIN does NOT work in embedded mode (DataFusion limitation, laminardb#37)
- `laminar-core` version must match what `laminar-db` depends on internally

### From This Project

- 5 streams from 2 sources works without throughput issues at 200ms cycle time
- CASE WHEN inside SUM() works in DataFusion via laminardb
- Alert latency (generation → alert creation) is consistently under 2ms
- Push latency is dominated by Arrow RecordBatch construction, not network I/O

## Session History

### Session 1 (2026-02-08)

- Created project scaffold with published crate dependencies
- Implemented all 5 detection streams + alert engine + latency tracker
- Headless mode working — all 5 streams producing output
- Tuned thresholds: VolumeAnomaly, SuspiciousMatch, WashTrading, RapidFire confirmed
- PriceSpike threshold lowered to 0.2% to trigger on manipulation scenarios
