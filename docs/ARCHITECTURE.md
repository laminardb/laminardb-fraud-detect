# Architecture

> How laminardb-fraud-detect works end-to-end.

---

## System Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        laminardb-fraud-detect                           │
│                                                                         │
│  ┌──────────────┐     ┌──────────────────────────────────────────────┐  │
│  │              │     │         LaminarDB Embedded Engine             │  │
│  │  Fraud       │     │         (100ms micro-batch ticks)             │  │
│  │  Generator   │     │                                              │  │
│  │              │     │  ┌────────────┐     ┌─────────────────────┐  │  │
│  │  5 symbols   │────►│  │  SOURCE:   │────►│ Stream 1: HOP       │  │  │
│  │  8 accounts  │     │  │  trades    │  │  │ vol_baseline        │──┼──┼──►  VolumeAnomaly
│  │  4 fraud     │     │  │            │  │  └─────────────────────┘  │  │
│  │  scenarios   │     │  │  Fields:   │  │  ┌─────────────────────┐  │  │
│  │              │     │  │  account_id│  ├─►│ Stream 2: TUMBLE    │  │  │
│  │  Cycle:      │     │  │  symbol    │  │  │ ohlc_vol            │──┼──┼──►  PriceSpike
│  │  200ms       │     │  │  side      │  │  └─────────────────────┘  │  │
│  │              │     │  │  price     │  │  ┌─────────────────────┐  │  │
│  │              │     │  │  volume    │  ├─►│ Stream 3: SESSION   │  │  │
│  │              │     │  │  order_ref │  │  │ rapid_fire          │──┼──┼──►  RapidFire
│  │              │     │  │  ts        │  │  └─────────────────────┘  │  │
│  │              │     │  └────────────┘  │  ┌─────────────────────┐  │  │
│  │              │     │                  └─►│ Stream 4: TUMBLE    │  │  │
│  │              │     │                     │ wash_score           │──┼──┼──►  WashTrading
│  │              │     │                     │ (CASE WHEN)          │  │  │
│  │              │     │                     └─────────────────────┘  │  │
│  │              │     │                                              │  │
│  │              │     │  ┌────────────┐     ┌─────────────────────┐  │  │
│  │              │────►│  │  SOURCE:   │────►│ Stream 5: INNER JOIN│  │  │
│  │              │     │  │  orders    │     │ suspicious_match    │──┼──┼──►  SuspiciousMatch
│  │              │     │  └────────────┘     │ (trades × orders)   │  │  │
│  └──────────────┘     │                     └─────────────────────┘  │  │
│                       └──────────────────────────────────────────────┘  │
│                                        │                                │
│                                        ▼                                │
│                       ┌──────────────────────────────────────────────┐  │
│                       │              AlertEngine                     │  │
│                       │                                              │  │
│                       │  Evaluates each stream output against        │  │
│                       │  configurable thresholds:                    │  │
│                       │                                              │  │
│                       │  vol_baseline  → ratio vs rolling avg        │  │
│                       │  ohlc_vol      → price_range / open          │  │
│                       │  rapid_fire    → burst_trades count          │  │
│                       │  wash_score    → buy/sell imbalance          │  │
│                       │  susp_match    → |price_diff|                │  │
│                       │                                              │  │
│                       │  Severity: Medium → High → Critical          │  │
│                       └──────────────────┬───────────────────────────┘  │
│                                          │                              │
│                       ┌──────────────────┴───────────────────────────┐  │
│                       │           LatencyTracker                     │  │
│                       │                                              │  │
│                       │  Push:       push_batch() duration           │  │
│                       │  Processing: push → first poll()             │  │
│                       │  Alert:      generation → alert creation     │  │
│                       │                                              │  │
│                       │  Rolling 1000-sample window                  │  │
│                       │  Computes: p50, p95, p99, min, max           │  │
│                       └──────────────────┬───────────────────────────┘  │
│                                          │                              │
│                    ┌─────────────────────┼─────────────────────┐        │
│                    ▼                     ▼                     ▼        │
│            ┌──────────────┐   ┌───────────────────┐   ┌────────────┐   │
│            │  TUI Mode    │   │   Web Mode         │   │  Headless  │   │
│            │  (ratatui)   │   │   (axum+Chart.js)  │   │  (stdout)  │   │
│            │              │   │                     │   │            │   │
│            │  Alert feed  │   │  WebSocket server   │   │  Summary   │   │
│            │  Latency     │   │  Real-time charts   │   │  report    │   │
│            │  Prices      │   │  Alert table        │   │  at exit   │   │
│            │  Counts      │   │  localhost:3000      │   │            │   │
│            └──────────────┘   └───────────────────────┘   └────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow (One Cycle)

```
Time ──────────────────────────────────────────────────────────────►

  t=0ms          t=0ms              t~100ms            t~100ms
  ┌─────┐        ┌─────┐           ┌─────┐            ┌─────┐
  │ Gen │──────► │Push │─────────► │Tick │──────────► │Poll │
  └─────┘        └─────┘           └─────┘            └─────┘
     │              │                  │                  │
     │  generate    │  push_batch()    │  LaminarDB       │  sub.poll()
     │  _cycle(ts)  │  watermark()     │  drains buffers  │  for each
     │              │                  │  runs 5 SQL      │  stream
     │  Returns:    │  Feeds both      │  queries via     │
     │  - trades    │  SOURCE:trades   │  ctx.sql()       │  Returns:
     │  - orders    │  SOURCE:orders   │                  │  - VolumeBaseline
     │              │                  │  Registers       │  - OhlcVolatility
     │              │                  │  MemTables,      │  - RapidFireBurst
     │              │                  │  executes SQL,   │  - WashScore
     │              │                  │  pushes to       │  - SuspiciousMatch
     │              │                  │  subscriber      │
     │              │                  │  buffers         │
     │              │                  │                  │
     ▼              ▼                  ▼                  ▼
  LatencyTracker                                     AlertEngine
  records          records                           evaluates
  gen_instant      push start/end                    each row
                                                     against
                                                     thresholds
```

---

## Module Dependency Graph

```
main.rs
  ├── detection.rs ── laminar_db::{LaminarDB, SourceHandle, TypedSubscription}
  │     └── types.rs ── laminar_derive::{Record, FromRow}
  ├── generator.rs
  │     └── types.rs
  ├── alerts.rs
  │     └── types.rs
  ├── latency.rs
  ├── tui.rs ── ratatui, crossterm
  │     ├── alerts.rs
  │     ├── latency.rs
  │     ├── detection.rs
  │     └── generator.rs
  └── web.rs ── axum, tower_http
        ├── alerts.rs
        ├── latency.rs
        ├── detection.rs
        └── generator.rs
```

---

## LaminarDB Embedded Pipeline Internals

```
  Your App                        LaminarDB (100ms tick)
  ─────────                       ─────────────────────────────
  push_batch(trades) ──────► Source buffer (trades)
  push_batch(orders) ──────► Source buffer (orders)
  watermark(ts+10000) ─────► │
                              ▼  (every 100ms)
                         ┌── Drain ALL source buffers
                         ├── Register as Arrow MemTables in DataFusion
                         ├── Execute 5 stream queries via ctx.sql():
                         │     1. vol_baseline   (HOP)
                         │     2. ohlc_vol       (TUMBLE)
                         │     3. rapid_fire     (SESSION)
                         │     4. wash_score     (TUMBLE)
                         │     5. suspicious_match (INNER JOIN)
                         ├── Push results to subscriber buffers
                         └── Clean up MemTables
                              │
  poll()  ◄─────────────── Subscriber buffers (one per stream)
```

**Key properties:**
- All 5 queries run within the same tick — total SQL execution is serialized
- MemTables are ephemeral — created fresh each tick, discarded after
- Watermark at `ts + 10_000` ensures the 10s HOP window closes correctly
- Both sources share the same tick boundary

---

## Window Types Visualized

### HOP Window (vol_baseline)

```
  slide=2s    window=10s
  ├──────────────────────┤
  │  Window 1: [0s-10s]  │
  │  ├──────────────────────┤
  │  │  Window 2: [2s-12s]  │
  │  │  ├──────────────────────┤
  │  │  │  Window 3: [4s-14s]  │
  ▼  ▼  ▼
─────────────────────────────────► time
  A single trade at t=5s appears in Windows 1, 2, and 3.
  Overlapping windows smooth out noise for baseline detection.
```

### SESSION Window (rapid_fire)

```
  gap=2s
                 ◄─2s─►
  ─── trade ── trade ── trade ─────────── trade ── trade ──
  │          Session 1          │        │  Session 2    │
  └─────────────────────────────┘        └───────────────┘
  burst_trades=3                          burst_trades=2

  A 2s gap between events closes the session.
  Rapid-fire: 20+ trades with <100ms spacing = 1 huge session.
```

### TUMBLE Window (ohlc_vol, wash_score)

```
  size=5s
  ├──────┤├──────┤├──────┤
  │ 0-5s ││ 5-10s││10-15s│
  └──────┘└──────┘└──────┘
  Non-overlapping. Each trade belongs to exactly one window.
  Output emitted when window closes (watermark advances past boundary).
```

---

## Alert Severity Pipeline

```
  Stream Output
       │
       ▼
  ┌─────────────────────────────────────┐
  │  Threshold Check                    │
  │  (configurable per alert type)      │
  │                                     │
  │  PASS threshold? ──► No  ──► skip   │
  │       │                             │
  │      Yes                            │
  │       │                             │
  │  ┌────▼────────────────────────┐    │
  │  │  Severity Classification    │    │
  │  │                             │    │
  │  │  Critical ──► extreme val   │    │
  │  │  High     ──► significant   │    │
  │  │  Medium   ──► above thresh  │    │
  │  └────┬────────────────────────┘    │
  │       │                             │
  │  ┌────▼────────────────────────┐    │
  │  │  Alert Created              │    │
  │  │  - id, type, severity       │    │
  │  │  - description              │    │
  │  │  - latency_us               │    │
  │  │  - timestamp_ms             │    │
  │  └────┬────────────────────────┘    │
  └───────┼─────────────────────────────┘
          │
          ├──► AlertEngine.alerts (VecDeque, last 200)
          ├──► AlertEngine.counts (HashMap by type)
          └──► LatencyTracker.record_alert()
```
