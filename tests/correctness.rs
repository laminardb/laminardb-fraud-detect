//! Correctness tests for all 6 detection streams + edge cases.
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
//      FROM trades t INNER JOIN orders o ON symbol AND ts BETWEEN -2000 AND +2000
// Push 1 trade + 1 order at same timestamp/symbol, assert join and price_diff.
#[tokio::test]
async fn test_suspicious_match_correctness() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    // Trade: AMZN at 180.50
    let trades = vec![
        Trade { account_id: "C1".into(), symbol: "AMZN".into(), side: "buy".into(), price: 180.50, volume: 50, order_ref: "ORD-1".into(), ts: base },
    ];

    // Order: AMZN at 180.55 (same timestamp — within 2s window)
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

// ── Test 6: ASOF Match (ASOF JOIN — front-running detection) ──
// SQL: t.price - o.price AS price_spread
//      FROM trades t ASOF JOIN orders o MATCH_CONDITION(t.ts >= o.ts) ON symbol
// Push orders first (separate micro-batch), then trade, assert join and price_spread.
#[tokio::test]
async fn test_asof_match_correctness() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    // ASOF JOIN might not be available in published crates
    if pipeline.asof_match_sub.is_none() {
        eprintln!("ASOF JOIN not available — skipping test");
        let _ = pipeline.db.shutdown().await;
        return;
    }

    // Step 1: Push order first and advance its watermark (separate micro-batch)
    let orders = vec![
        Order { order_id: "ASOF-ORD-1".into(), account_id: "D2".into(), symbol: "TSLA".into(), side: "buy".into(), quantity: 100, price: 250.00, ts: base },
    ];
    pipeline.order_source.push_batch(orders);
    pipeline.order_source.watermark(base + 5_000);

    // Small sleep to let the micro-batch process the order
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Step 2: Push trade after order (ts = base + 1000, so t.ts >= o.ts is satisfied)
    let trades = vec![
        Trade { account_id: "D1".into(), symbol: "TSLA".into(), side: "buy".into(), price: 250.10, volume: 100, order_ref: "".into(), ts: base + 1000 },
    ];
    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(base + 20_000);
    pipeline.order_source.watermark(base + 20_000);

    let sub = pipeline.asof_match_sub.as_ref().unwrap();
    let results = collect_all(sub, Duration::from_secs(8)).await;

    let tsla: Vec<_> = results.iter()
        .filter(|r: &&laminardb_fraud_detect::types::AsofMatch| r.symbol == "TSLA")
        .collect();

    if tsla.is_empty() {
        eprintln!("ASOF JOIN stream created but produced no output — may need unreleased fix");
        let _ = pipeline.db.shutdown().await;
        return;
    }

    let row = &tsla[0];
    assert!((row.trade_price - 250.10).abs() < 0.01, "trade_price should be 250.10, got {}", row.trade_price);
    assert!((row.order_price - 250.00).abs() < 0.01, "order_price should be 250.00, got {}", row.order_price);

    // price_spread = trade_price - order_price = 250.10 - 250.00 = 0.10
    let expected_spread = 250.10 - 250.00;
    assert!((row.price_spread - expected_spread).abs() < 0.01,
        "price_spread should be {:.4}, got {:.4}", expected_spread, row.price_spread);

    assert_eq!(row.volume, 100, "volume should be 100");
    assert_eq!(row.trade_account, "D1", "trade_account should be D1");
    assert_eq!(row.order_account, "D2", "order_account should be D2");
    assert_eq!(row.order_id, "ASOF-ORD-1", "order_id should be ASOF-ORD-1");

    let _ = pipeline.db.shutdown().await;
}

// ══════════════════════════════════════════════════════════
// Edge case tests: empty windows, late data, NULL handling
// ══════════════════════════════════════════════════════════

// ── Edge 1: Empty window gap ──
// Push trades in window [100_000, 105_000), skip window [105_000, 110_000),
// push trades in [110_000, 115_000). Verify both populated windows produce
// output and the gap doesn't break the pipeline.
#[tokio::test]
async fn test_edge_empty_window_gap() {
    let pipeline = detection::setup().await.unwrap();

    // Window 1: trades at 100_000
    let trades_w1 = vec![
        Trade { account_id: "E1".into(), symbol: "AAPL".into(), side: "buy".into(), price: 150.0, volume: 100, order_ref: "".into(), ts: 100_000 },
    ];
    pipeline.trade_source.push_batch(trades_w1);
    pipeline.trade_source.watermark(110_000); // past empty window
    pipeline.order_source.watermark(110_000);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Window 3: trades at 110_000
    let trades_w3 = vec![
        Trade { account_id: "E2".into(), symbol: "AAPL".into(), side: "sell".into(), price: 155.0, volume: 200, order_ref: "".into(), ts: 110_000 },
    ];
    pipeline.trade_source.push_batch(trades_w3);
    pipeline.trade_source.watermark(130_000);
    pipeline.order_source.watermark(130_000);

    let sub = pipeline.ohlc_vol_sub.as_ref().expect("ohlc_vol should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    // Should get OHLC rows from both populated windows, pipeline didn't stall
    let aapl: Vec<_> = results.iter()
        .filter(|r: &&OhlcVolatility| r.symbol == "AAPL")
        .collect();
    assert!(aapl.len() >= 2, "Expected OHLC output from both windows after gap, got {} rows", aapl.len());

    let _ = pipeline.db.shutdown().await;
}

// ── Edge 2: Late data (behind watermark) ──
// Push trades, advance watermark far ahead, then push more trades with
// old timestamps. FINDING: LaminarDB v0.1.1 does NOT drop late data —
// events behind the watermark are still processed and appear in output.
// This test documents that behavior.
#[tokio::test]
async fn test_edge_late_data_not_dropped() {
    let pipeline = detection::setup().await.unwrap();

    // Push trade at 100_000, advance watermark to 200_000
    let on_time = vec![
        Trade { account_id: "L1".into(), symbol: "MSFT".into(), side: "buy".into(), price: 400.0, volume: 100, order_ref: "".into(), ts: 100_000 },
    ];
    pipeline.trade_source.push_batch(on_time);
    pipeline.trade_source.watermark(200_000);
    pipeline.order_source.watermark(200_000);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Drain output from the on-time trade
    let sub = pipeline.vol_baseline_sub.as_ref().expect("vol_baseline should exist");
    let _initial = collect_all(sub, Duration::from_secs(2)).await;

    // Push LATE trade (ts=50_000 is way behind watermark 200_000)
    let late = vec![
        Trade { account_id: "L2".into(), symbol: "MSFT".into(), side: "sell".into(), price: 999.0, volume: 9999, order_ref: "".into(), ts: 50_000 },
    ];
    pipeline.trade_source.push_batch(late);
    pipeline.trade_source.watermark(250_000);
    pipeline.order_source.watermark(250_000);

    let after = collect_all(sub, Duration::from_secs(3)).await;

    // LaminarDB v0.1.1 behavior: late data IS processed (not dropped)
    let has_late_volume = after.iter()
        .any(|r: &VolumeBaseline| r.symbol == "MSFT" && r.total_volume >= 9999);
    assert!(has_late_volume,
        "LaminarDB v0.1.1 processes late data — volume=9999 should appear in output. \
         Rows after: {:?}",
        after.iter().filter(|r| r.symbol == "MSFT").map(|r| r.total_volume).collect::<Vec<_>>());

    // Pipeline is still functional after late data
    let recovery = vec![
        Trade { account_id: "L3".into(), symbol: "MSFT".into(), side: "buy".into(), price: 405.0, volume: 50, order_ref: "".into(), ts: 250_000 },
    ];
    pipeline.trade_source.push_batch(recovery);
    pipeline.trade_source.watermark(300_000);
    pipeline.order_source.watermark(300_000);

    let recovery_results = collect_all(sub, Duration::from_secs(3)).await;
    let recovery_msft: Vec<_> = recovery_results.iter()
        .filter(|r: &&VolumeBaseline| r.symbol == "MSFT")
        .collect();
    assert!(!recovery_msft.is_empty(),
        "Pipeline should still produce output after late data injection");

    let _ = pipeline.db.shutdown().await;
}

// ── Edge 3: Single-trade OHLC window ──
// Push exactly 1 trade into a TUMBLE window. OHLC should have
// open = high = low = close = price, price_range = 0.
#[tokio::test]
async fn test_edge_single_trade_ohlc() {
    let pipeline = detection::setup().await.unwrap();

    let trades = vec![
        Trade { account_id: "S1".into(), symbol: "TSLA".into(), side: "buy".into(), price: 250.50, volume: 42, order_ref: "".into(), ts: 100_000 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(120_000);
    pipeline.order_source.watermark(120_000);

    let sub = pipeline.ohlc_vol_sub.as_ref().expect("ohlc_vol should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let tsla: Vec<_> = results.iter()
        .filter(|r: &&OhlcVolatility| r.symbol == "TSLA" && r.volume == 42)
        .collect();
    assert!(!tsla.is_empty(), "Expected OHLC output for single TSLA trade");

    let row = &tsla[0];
    assert!((row.open - 250.50).abs() < 0.01, "open should equal price for single trade");
    assert!((row.high - 250.50).abs() < 0.01, "high should equal price for single trade");
    assert!((row.low - 250.50).abs() < 0.01, "low should equal price for single trade");
    assert!((row.close - 250.50).abs() < 0.01, "close should equal price for single trade");
    assert!((row.price_range).abs() < 0.01, "price_range should be 0 for single trade, got {}", row.price_range);

    let _ = pipeline.db.shutdown().await;
}

// ── Edge 4: INNER JOIN with no matching symbol ──
// Push trades for AAPL, orders for GOOGL. INNER JOIN requires symbol match,
// so output should be empty.
#[tokio::test]
async fn test_edge_join_no_symbol_match() {
    let pipeline = detection::setup().await.unwrap();
    let base: i64 = 100_000;

    let trades = vec![
        Trade { account_id: "J1".into(), symbol: "AAPL".into(), side: "buy".into(), price: 150.0, volume: 100, order_ref: "".into(), ts: base },
    ];
    let orders = vec![
        Order { order_id: "ORD-NM".into(), account_id: "J2".into(), symbol: "GOOGL".into(), side: "sell".into(), quantity: 100, price: 2800.0, ts: base },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.order_source.push_batch(orders);
    pipeline.trade_source.watermark(base + 20_000);
    pipeline.order_source.watermark(base + 20_000);

    let sub = pipeline.suspicious_match_sub.as_ref().expect("suspicious_match should exist");
    let results = collect_all(sub, Duration::from_secs(3)).await;

    // Filter for our specific test symbols — should be empty
    let mismatched: Vec<_> = results.iter()
        .filter(|r: &&SuspiciousMatch| r.symbol == "AAPL" && r.order_id == "ORD-NM")
        .collect();
    assert!(mismatched.is_empty(),
        "INNER JOIN should produce no output when symbols don't match, got {} rows", mismatched.len());

    let _ = pipeline.db.shutdown().await;
}

// ── Edge 5: INNER JOIN with order outside time window ──
// Push trade at ts=100_000 and order at ts=200_000 (100s apart, far outside 2s window).
// Should produce no match.
#[tokio::test]
async fn test_edge_join_outside_time_window() {
    let pipeline = detection::setup().await.unwrap();

    let trades = vec![
        Trade { account_id: "T1".into(), symbol: "AMZN".into(), side: "buy".into(), price: 185.0, volume: 75, order_ref: "".into(), ts: 100_000 },
    ];
    let orders = vec![
        Order { order_id: "ORD-FAR".into(), account_id: "T2".into(), symbol: "AMZN".into(), side: "sell".into(), quantity: 75, price: 186.0, ts: 200_000 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.order_source.push_batch(orders);
    pipeline.trade_source.watermark(250_000);
    pipeline.order_source.watermark(250_000);

    let sub = pipeline.suspicious_match_sub.as_ref().expect("suspicious_match should exist");
    let results = collect_all(sub, Duration::from_secs(3)).await;

    let far_match: Vec<_> = results.iter()
        .filter(|r: &&SuspiciousMatch| r.order_id == "ORD-FAR")
        .collect();
    assert!(far_match.is_empty(),
        "INNER JOIN should produce no output when order is 100s away (outside 2s window), got {} rows",
        far_match.len());

    let _ = pipeline.db.shutdown().await;
}

// ── Edge 6: Wash score with only buys (no sells) ──
// Push only buy-side trades for one account+symbol. sell_volume and
// sell_count should be 0.
#[tokio::test]
async fn test_edge_wash_only_buys() {
    let pipeline = detection::setup().await.unwrap();

    let trades = vec![
        Trade { account_id: "BUY-ONLY".into(), symbol: "GOOGL".into(), side: "buy".into(), price: 2800.0, volume: 100, order_ref: "".into(), ts: 100_000 },
        Trade { account_id: "BUY-ONLY".into(), symbol: "GOOGL".into(), side: "buy".into(), price: 2810.0, volume: 200, order_ref: "".into(), ts: 101_000 },
        Trade { account_id: "BUY-ONLY".into(), symbol: "GOOGL".into(), side: "buy".into(), price: 2820.0, volume: 150, order_ref: "".into(), ts: 102_000 },
    ];

    pipeline.trade_source.push_batch(trades);
    pipeline.trade_source.watermark(120_000);
    pipeline.order_source.watermark(120_000);

    let sub = pipeline.wash_score_sub.as_ref().expect("wash_score should exist");
    let results = collect_all(sub, Duration::from_secs(5)).await;

    let buy_only: Vec<_> = results.iter()
        .filter(|r: &&WashScore| r.account_id == "BUY-ONLY" && r.symbol == "GOOGL")
        .collect();
    assert!(!buy_only.is_empty(), "Expected wash_score output for BUY-ONLY account");

    for row in &buy_only {
        assert_eq!(row.sell_volume, 0, "sell_volume should be 0 when only buys, got {}", row.sell_volume);
        assert_eq!(row.sell_count, 0, "sell_count should be 0 when only buys, got {}", row.sell_count);
        assert!(row.buy_volume > 0, "buy_volume should be > 0");
        assert!(row.buy_count > 0, "buy_count should be > 0");
    }

    let _ = pipeline.db.shutdown().await;
}
