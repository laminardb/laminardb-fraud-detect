# Fraud Detection Strategies

> How each detection stream works, its SQL, and alert logic.

---

## 1. Volume Anomaly Detection

**Stream:** `vol_baseline` | **Window:** HOP (2s slide, 10s size) | **Alert:** VolumeAnomaly

### What It Detects

Sudden spikes in trading volume on a specific symbol. Volume spikes often precede or accompany market manipulation — a trader flooding the book to move the price.

### SQL

```sql
CREATE STREAM vol_baseline AS
SELECT symbol,
       SUM(volume) AS total_volume,
       COUNT(*) AS trade_count,
       AVG(price) AS avg_price
FROM trades
GROUP BY symbol, HOP(ts, INTERVAL '2' SECOND, INTERVAL '10' SECOND)
```

The HOP window produces overlapping 10-second windows that slide every 2 seconds. This gives a smoothed baseline — each output row represents the total volume over the last 10s.

### Alert Logic

The AlertEngine maintains a rolling history of the last 20 `total_volume` values per symbol. When a new value arrives:

```
ratio = current_volume / rolling_average
if ratio > 2.0:  alert
  > 10x → Critical
  > 5x  → High
  > 2x  → Medium
```

### Fraud Injection

`VolumeSpike` scenario: 5-10 trades with volume multiplied by 10-50x on a single symbol from a FRAUD account.

---

## 2. Price Spike / Manipulation Detection

**Stream:** `ohlc_vol` | **Window:** TUMBLE (5s) | **Alert:** PriceSpike

### What It Detects

Abnormal price swings within a short time window. Price manipulation typically involves pushing a price in one direction then reversing — creating a high intra-window range.

### SQL

```sql
CREATE STREAM ohlc_vol AS
SELECT symbol,
       CAST(tumble(ts, INTERVAL '5' SECOND) AS BIGINT) AS bar_start,
       first_value(price) AS open,
       MAX(price) AS high,
       MIN(price) AS low,
       last_value(price) AS close,
       SUM(volume) AS volume,
       MAX(price) - MIN(price) AS price_range
FROM trades
GROUP BY symbol, tumble(ts, INTERVAL '5' SECOND)
```

The `price_range` field gives us the intra-bar volatility directly from SQL.

### Alert Logic

```
range_pct = price_range / open
if range_pct > 0.002:  alert  (0.2% threshold)
  > 5%  → Critical
  > 1%  → High
  > 0.2% → Medium
```

### Fraud Injection

`PriceManipulation` scenario: push price up 2-4% per cycle for 3 consecutive cycles, then a sharp 8% reversal. This creates a high `price_range` in the TUMBLE window containing the reversal.

---

## 3. Rapid-Fire Burst Detection

**Stream:** `rapid_fire` | **Window:** SESSION (2s gap) | **Alert:** RapidFire

### What It Detects

Accounts sending an unusually high number of trades in a short burst. Rapid-fire trading can indicate algorithmic manipulation, spoofing (placing and quickly canceling orders), or unauthorized bot activity.

### SQL

```sql
CREATE STREAM rapid_fire AS
SELECT account_id,
       COUNT(*) AS burst_trades,
       SUM(volume) AS burst_volume,
       MIN(price) AS low,
       MAX(price) AS high
FROM trades
GROUP BY account_id, SESSION(ts, INTERVAL '2' SECOND)
```

SESSION windows are gap-based: they close when no event arrives for 2 seconds. All trades from the same account that arrive within 2 seconds of each other are grouped into a single session.

### Alert Logic

```
if burst_trades >= 5:  alert
  > 50 → Critical
  > 20 → High
  >= 5 → Medium
```

### Fraud Injection

`RapidFire` scenario: 20-30 trades from a single FRAUD account with timestamps spaced 50-100ms apart. All trades fall within the 2-second SESSION gap, creating one large burst.

---

## 4. Wash Trading Detection

**Stream:** `wash_score` | **Window:** TUMBLE (5s) + CASE WHEN | **Alert:** WashTrading

### What It Detects

An account trading with itself — buying and selling roughly equal volumes of the same security. Wash trading is illegal because it creates false liquidity and misleading volume signals.

### SQL

```sql
CREATE STREAM wash_score AS
SELECT account_id,
       symbol,
       SUM(CASE WHEN side = 'buy' THEN volume ELSE CAST(0 AS BIGINT) END) AS buy_volume,
       SUM(CASE WHEN side = 'sell' THEN volume ELSE CAST(0 AS BIGINT) END) AS sell_volume,
       SUM(CASE WHEN side = 'buy' THEN 1 ELSE 0 END) AS buy_count,
       SUM(CASE WHEN side = 'sell' THEN 1 ELSE 0 END) AS sell_count
FROM trades
GROUP BY account_id, symbol, TUMBLE(ts, INTERVAL '5' SECOND)
```

Uses `CASE WHEN` inside `SUM()` to separate buy and sell volumes in a single stream. The `CAST(0 AS BIGINT)` ensures type compatibility.

### Alert Logic

```
imbalance = |buy_volume - sell_volume| / (buy_volume + sell_volume)
if imbalance < 0.3 AND buy_count >= 2 AND sell_count >= 2:  alert
  imbalance < 0.02 → Critical  (near-perfect wash)
  imbalance < 0.05 → High
  imbalance < 0.3  → Medium
```

An imbalance near 0.0 means the account bought and sold almost identical amounts — the signature of wash trading.

### Fraud Injection

`WashTrading` scenario: 3-6 buy/sell pairs from a single FRAUD account on the same symbol with identical volumes.

---

## 5. Suspicious Trade-Order Matching

**Stream:** `suspicious_match` | **Join:** INNER JOIN (10s window) | **Alert:** SuspiciousMatch

### What It Detects

Trades that match orders at suspiciously close prices, suggesting pre-arranged transactions or front-running.

### SQL

```sql
CREATE STREAM suspicious_match AS
SELECT t.symbol,
       t.price AS trade_price,
       t.volume,
       o.order_id,
       o.account_id,
       o.side,
       o.price AS order_price,
       t.price - o.price AS price_diff
FROM trades t
INNER JOIN orders o
ON t.symbol = o.symbol
AND o.ts BETWEEN t.ts - 10000 AND t.ts + 10000
```

Uses numeric `BETWEEN` (not INTERVAL) because `ts` is BIGINT milliseconds. The 10-second window is tighter than typical market matching to reduce false positives.

### Alert Logic

```
if |price_diff| < 1.0:  alert
  < 0.001 → High  (near-exact price match)
  < 1.0   → Medium
```

### Fraud Injection

Normal order generation already creates ~30% matching orders with slight price offsets. During fraud scenarios, the generator creates orders with very tight price matching (offset < 0.2% of price).

---

## Tuning Guide

All thresholds are configurable via the `AlertEngine` struct fields:

| Field | Default | Description |
|-------|---------|-------------|
| `volume_ratio_threshold` | 2.0 | Volume/average ratio to trigger |
| `price_range_pct_threshold` | 0.002 | Price range/open percentage |
| `rapid_fire_threshold` | 5 | Min burst trades to trigger |
| `wash_imbalance_threshold` | 0.3 | Max imbalance (0=perfect wash) |
| `match_price_diff_threshold` | 1.0 | Max |price_diff| for suspicious |

For production use:
- Increase `volume_ratio_threshold` to 5-10x (reduce noise)
- Increase `rapid_fire_threshold` to 20+ (HFT markets have legitimate bursts)
- Decrease `wash_imbalance_threshold` to 0.05 (only flag near-perfect washes)
- Use longer window sizes (TUMBLE 1 minute, HOP 5-minute slide / 30-minute window)
