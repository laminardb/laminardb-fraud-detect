//! Correctness tests for all 5 detection streams.
//!
//! Pushes known deterministic data, advances watermarks, and asserts
//! exact output values from each stream.

use std::time::{Duration, Instant};

use laminardb_fraud_detect::detection;
use laminardb_fraud_detect::types::*;

/// Poll a subscription until deadline, collecting all results.
async fn collect_all<T: Clone + laminar_db::FromBatch>(
    sub: &laminar_db::TypedSubscription<T>,
    timeout: Duration,
) -> Vec<T> {
    let deadline = Instant::now() + timeout;
    let mut results = Vec::new();
    while Instant::now() < deadline {
        while let Some(rows) = sub.poll() {
            results.extend(rows);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Final drain
    while let Some(rows) = sub.poll() {
        results.extend(rows);
    }
    results
}

// ── Test 1: Volume Baseline (HOP window) ──
// SQL: SUM(volume), COUNT(*), AVG(price) GROUP BY symbol, HOP(ts, 2s, 10s)
// Push 4 AAPL trades with known volumes/prices, assert aggregates.
#[tokio::test]
async fn test_vol_baseline_correctness() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    // 4 trades for AAPL, all within 1.5s (fits in any single HOP window)
    // Expected: total_volume=700, trade_count=4, avg_price=150.5
    let trades = vec![
        Trade { account_id: "A1".into(), symbol: "AAPL".into(), side: "buy".into(), price: 150.0, volume: 100, order_ref: "".into(), ts: base },
        Trade { account_id: "A2".into(), symbol: "AAPL".into(), side: "buy".into(), price: 155.0, volume: 200, order_ref: "".into(), ts: base + 500 },
        Trade { account_id: "A3".into(), symbol: "AAPL".into(), side: "sell".into(), price: 145.0, volume: 150, order_ref: "".into(), ts: base + 1000 },
        Trade { account_id: "A4".into(), symbol: "AAPL".into(), side: "buy".into(), price: 152.0, volume: 250, order_ref: "".into(), ts: base + 1500 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(base + 20_000);
    pipeline.order_source.watermark(base + 20_000);

    let sub = pipeline.vol_baseline_sub.as_ref().expect("vol_baseline stream should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    // HOP produces overlapping windows — find any window containing all 4 trades
    let matching: Vec<_> = results.iter()
        .filter(|r: &&VolumeBaseline| r.symbol == "AAPL" && r.total_volume == 700)
        .collect();

    assert!(!matching.is_empty(), "Expected at least one HOP window with AAPL total_volume=700, got {} rows: {:?}",
        results.iter().filter(|r| r.symbol == "AAPL").count(),
        results.iter().filter(|r| r.symbol == "AAPL").map(|r| r.total_volume).collect::<Vec<_>>());

    for row in &matching {
        assert_eq!(row.trade_count, 4, "trade_count should be 4");
        assert!((row.avg_price - 150.5).abs() < 0.01, "avg_price should be 150.5, got {}", row.avg_price);
    }

    let _ = pipeline.db.shutdown().await;
}

// ── Test 2: OHLC + Volatility (TUMBLE window) ──
// SQL: first_value(price), MAX(price), MIN(price), last_value(price),
//      SUM(volume), MAX(price)-MIN(price) GROUP BY symbol, TUMBLE(ts, 5s)
// Push 4 MSFT trades in one 5s window, assert OHLC values.
#[tokio::test]
async fn test_ohlc_vol_correctness() {
    let pipeline = detection::setup().await.unwrap();

    // Align to a 5s TUMBLE boundary: 100_000 % 5000 = 0
    let base: i64 = 100_000;

    // 4 MSFT trades in window [100000, 105000)
    // Prices: 300, 310, 290, 305 → open=300, high=310, low=290, close=305, range=20
    // Volumes: 50+100+75+125 = 350
    let trades = vec![
        Trade { account_id: "B1".into(), symbol: "MSFT".into(), side: "buy".into(), price: 300.0, volume: 50, order_ref: "".into(), ts: base },
        Trade { account_id: "B2".into(), symbol: "MSFT".into(), side: "buy".into(), price: 310.0, volume: 100, order_ref: "".into(), ts: base + 1000 },
        Trade { account_id: "B3".into(), symbol: "MSFT".into(), side: "sell".into(), price: 290.0, volume: 75, order_ref: "".into(), ts: base + 2000 },
        Trade { account_id: "B4".into(), symbol: "MSFT".into(), side: "buy".into(), price: 305.0, volume: 125, order_ref: "".into(), ts: base + 3000 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(base + 15_000);
    pipeline.order_source.watermark(base + 15_000);

    let sub = pipeline.ohlc_vol_sub.as_ref().expect("ohlc_vol stream should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let msft: Vec<_> = results.iter().filter(|r: &&OhlcVolatility| r.symbol == "MSFT").collect();
    assert!(!msft.is_empty(), "Expected OHLC output for MSFT, got none");

    // Find the window containing our 4 trades
    let row = msft.iter()
        .find(|r| r.volume == 350)
        .expect("Expected OHLC row with volume=350");

    assert!((row.open - 300.0).abs() < 0.01, "open should be 300.0, got {}", row.open);
    assert!((row.high - 310.0).abs() < 0.01, "high should be 310.0, got {}", row.high);
    assert!((row.low - 290.0).abs() < 0.01, "low should be 290.0, got {}", row.low);
    assert!((row.close - 305.0).abs() < 0.01, "close should be 305.0, got {}", row.close);
    assert!((row.price_range - 20.0).abs() < 0.01, "price_range should be 20.0, got {}", row.price_range);

    let _ = pipeline.db.shutdown().await;
}

// ── Test 3: Rapid-Fire Burst (SESSION window) ──
// SQL: COUNT(*), SUM(volume), MIN(price), MAX(price)
//      GROUP BY account_id, SESSION(ts, 2s)
// Push 5 trades from one account within 1s, assert session aggregates.
#[tokio::test]
async fn test_rapid_fire_correctness() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    // 5 trades from TEST-RF, spaced 200ms apart (all within 2s session gap)
    // Volumes: 10+20+30+40+50 = 150
    // Prices: 200, 205, 195, 210, 198 → low=195, high=210
    let trades = vec![
        Trade { account_id: "TEST-RF".into(), symbol: "TSLA".into(), side: "buy".into(), price: 200.0, volume: 10, order_ref: "".into(), ts: base },
        Trade { account_id: "TEST-RF".into(), symbol: "TSLA".into(), side: "buy".into(), price: 205.0, volume: 20, order_ref: "".into(), ts: base + 200 },
        Trade { account_id: "TEST-RF".into(), symbol: "TSLA".into(), side: "sell".into(), price: 195.0, volume: 30, order_ref: "".into(), ts: base + 400 },
        Trade { account_id: "TEST-RF".into(), symbol: "TSLA".into(), side: "buy".into(), price: 210.0, volume: 40, order_ref: "".into(), ts: base + 600 },
        Trade { account_id: "TEST-RF".into(), symbol: "TSLA".into(), side: "sell".into(), price: 198.0, volume: 50, order_ref: "".into(), ts: base + 800 },
    ];

    pipeline.trade_source.push_batch(trades);
    // Advance watermark past session gap (last_ts + 2s = base+800+2000 = base+2800)
    pipeline.trade_source.watermark(base + 10_000);
    pipeline.order_source.watermark(base + 10_000);

    let sub = pipeline.rapid_fire_sub.as_ref().expect("rapid_fire stream should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let test_rf: Vec<_> = results.iter()
        .filter(|r: &&RapidFireBurst| r.account_id == "TEST-RF")
        .collect();
    assert!(!test_rf.is_empty(), "Expected rapid_fire output for TEST-RF, got none");

    // SESSION may emit partial results across micro-batches.
    // Sum all burst_trades for TEST-RF to verify total count.
    let total_trades: i64 = test_rf.iter().map(|r| r.burst_trades).sum();
    let total_volume: i64 = test_rf.iter().map(|r| r.burst_volume).sum();
    let min_low = test_rf.iter().map(|r| r.low).fold(f64::INFINITY, f64::min);
    let max_high = test_rf.iter().map(|r| r.high).fold(f64::NEG_INFINITY, f64::max);

    assert_eq!(total_trades, 5,
        "total burst_trades should be 5, got {} across {} rows: {:?}",
        total_trades, test_rf.len(),
        test_rf.iter().map(|r| (r.burst_trades, r.burst_volume)).collect::<Vec<_>>());
    assert_eq!(total_volume, 150, "total burst_volume should be 150, got {}", total_volume);
    assert!((min_low - 195.0).abs() < 0.01, "low should be 195.0, got {}", min_low);
    assert!((max_high - 210.0).abs() < 0.01, "high should be 210.0, got {}", max_high);

    let _ = pipeline.db.shutdown().await;
}

// ── Test 4: Wash Score (TUMBLE + CASE WHEN) ──
// SQL: SUM(CASE WHEN side='buy' THEN volume ELSE 0 END) AS buy_volume, ...
//      GROUP BY account_id, symbol, TUMBLE(ts, 5s)
// Push equal buy/sell pairs from one account, assert volumes split correctly.
#[tokio::test]
async fn test_wash_score_correctness() {
    let pipeline = detection::setup().await.unwrap();

    // Align to TUMBLE(5s) boundary
    let base: i64 = 100_000;

    // 2 buys (vol 100 each) + 2 sells (vol 100 each) from TEST-WS on GOOGL
    // Expected: buy_volume=200, sell_volume=200, buy_count=2, sell_count=2
    let trades = vec![
        Trade { account_id: "TEST-WS".into(), symbol: "GOOGL".into(), side: "buy".into(), price: 2800.0, volume: 100, order_ref: "".into(), ts: base },
        Trade { account_id: "TEST-WS".into(), symbol: "GOOGL".into(), side: "sell".into(), price: 2801.0, volume: 100, order_ref: "".into(), ts: base + 500 },
        Trade { account_id: "TEST-WS".into(), symbol: "GOOGL".into(), side: "buy".into(), price: 2799.0, volume: 100, order_ref: "".into(), ts: base + 1000 },
        Trade { account_id: "TEST-WS".into(), symbol: "GOOGL".into(), side: "sell".into(), price: 2800.0, volume: 100, order_ref: "".into(), ts: base + 1500 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(base + 15_000);
    pipeline.order_source.watermark(base + 15_000);

    let sub = pipeline.wash_score_sub.as_ref().expect("wash_score stream should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let test_ws: Vec<_> = results.iter()
        .filter(|r: &&WashScore| r.account_id == "TEST-WS" && r.symbol == "GOOGL")
        .collect();
    assert!(!test_ws.is_empty(), "Expected wash_score output for TEST-WS/GOOGL, got none");

    // Find the window with all 4 trades
    let row = test_ws.iter()
        .find(|r| r.buy_count == 2 && r.sell_count == 2)
        .expect("Expected window with buy_count=2, sell_count=2");

    assert_eq!(row.buy_volume, 200, "buy_volume should be 200, got {}", row.buy_volume);
    assert_eq!(row.sell_volume, 200, "sell_volume should be 200, got {}", row.sell_volume);

    let _ = pipeline.db.shutdown().await;
}

// ── Test 5: Suspicious Match (INNER JOIN) ──
// SQL: t.price - o.price AS price_diff
//      FROM trades t INNER JOIN orders o ON symbol AND ts BETWEEN -10000 AND +10000
// Push 1 trade + 1 order at same timestamp/symbol, assert join and price_diff.
#[tokio::test]
async fn test_suspicious_match_correctness() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    // Trade: AMZN at 180.50
    let trades = vec![
        Trade { account_id: "C1".into(), symbol: "AMZN".into(), side: "buy".into(), price: 180.50, volume: 50, order_ref: "ORD-1".into(), ts: base },
    ];

    // Order: AMZN at 180.55 (same timestamp — within 10s window)
    let orders = vec![
        Order { order_id: "ORD-1".into(), account_id: "C2".into(), symbol: "AMZN".into(), side: "sell".into(), quantity: 50, price: 180.55, ts: base },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.order_source.push_batch(orders);
    pipeline.trade_source.watermark(base + 20_000);
    pipeline.order_source.watermark(base + 20_000);

    let sub = pipeline.suspicious_match_sub.as_ref().expect("suspicious_match stream should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let amzn: Vec<_> = results.iter()
        .filter(|r: &&SuspiciousMatch| r.symbol == "AMZN")
        .collect();
    assert!(!amzn.is_empty(), "Expected suspicious_match output for AMZN, got none");

    let row = &amzn[0];
    assert!((row.trade_price - 180.50).abs() < 0.01, "trade_price should be 180.50, got {}", row.trade_price);
    assert!((row.order_price - 180.55).abs() < 0.01, "order_price should be 180.55, got {}", row.order_price);

    // price_diff = trade_price - order_price = 180.50 - 180.55 = -0.05
    let expected_diff = 180.50 - 180.55;
    assert!((row.price_diff - expected_diff).abs() < 0.01,
        "price_diff should be {:.4}, got {:.4}", expected_diff, row.price_diff);

    assert_eq!(row.volume, 50, "volume should be 50");
    assert_eq!(row.order_id, "ORD-1", "order_id should be ORD-1");

    let _ = pipeline.db.shutdown().await;
}
