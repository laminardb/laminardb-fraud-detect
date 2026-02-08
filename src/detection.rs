use laminar_db::LaminarDB;

use crate::types::*;

pub struct DetectionPipeline {
    pub db: LaminarDB,
    pub trade_source: laminar_db::SourceHandle<Trade>,
    pub order_source: laminar_db::SourceHandle<Order>,
    pub vol_baseline_sub: Option<laminar_db::TypedSubscription<VolumeBaseline>>,
    pub ohlc_vol_sub: Option<laminar_db::TypedSubscription<OhlcVolatility>>,
    pub rapid_fire_sub: Option<laminar_db::TypedSubscription<RapidFireBurst>>,
    pub wash_score_sub: Option<laminar_db::TypedSubscription<WashScore>>,
    pub suspicious_match_sub: Option<laminar_db::TypedSubscription<SuspiciousMatch>>,
    pub streams_created: Vec<(String, bool)>,
}

pub async fn setup() -> Result<DetectionPipeline, Box<dyn std::error::Error>> {
    let db = LaminarDB::builder()
        .buffer_size(65536)
        .build()
        .await?;

    // ── Sources ──
    db.execute(
        "CREATE SOURCE trades (
            account_id VARCHAR NOT NULL,
            symbol     VARCHAR NOT NULL,
            side       VARCHAR NOT NULL,
            price      DOUBLE NOT NULL,
            volume     BIGINT NOT NULL,
            order_ref  VARCHAR NOT NULL,
            ts         BIGINT NOT NULL
        )",
    )
    .await?;

    db.execute(
        "CREATE SOURCE orders (
            order_id   VARCHAR NOT NULL,
            account_id VARCHAR NOT NULL,
            symbol     VARCHAR NOT NULL,
            side       VARCHAR NOT NULL,
            quantity   BIGINT NOT NULL,
            price      DOUBLE NOT NULL,
            ts         BIGINT NOT NULL
        )",
    )
    .await?;

    let mut streams_created = Vec::new();

    // ── Stream 1: Volume Baseline (HOP window) ──
    let vol_ok = try_create(&db, "vol_baseline",
        "CREATE STREAM vol_baseline AS
         SELECT symbol,
                SUM(volume) AS total_volume,
                COUNT(*) AS trade_count,
                AVG(price) AS avg_price
         FROM trades
         GROUP BY symbol, HOP(ts, INTERVAL '2' SECOND, INTERVAL '10' SECOND)"
    ).await;
    streams_created.push(("vol_baseline".into(), vol_ok));

    // ── Stream 2: OHLC + Volatility (TUMBLE window) ──
    let ohlc_ok = try_create(&db, "ohlc_vol",
        "CREATE STREAM ohlc_vol AS
         SELECT symbol,
                CAST(tumble(ts, INTERVAL '5' SECOND) AS BIGINT) AS bar_start,
                first_value(price) AS open,
                MAX(price) AS high,
                MIN(price) AS low,
                last_value(price) AS close,
                SUM(volume) AS volume,
                MAX(price) - MIN(price) AS price_range
         FROM trades
         GROUP BY symbol, tumble(ts, INTERVAL '5' SECOND)"
    ).await;
    streams_created.push(("ohlc_vol".into(), ohlc_ok));

    // ── Stream 3: Rapid-Fire Burst (SESSION window) ──
    let rapid_ok = try_create(&db, "rapid_fire",
        "CREATE STREAM rapid_fire AS
         SELECT account_id,
                COUNT(*) AS burst_trades,
                SUM(volume) AS burst_volume,
                MIN(price) AS low,
                MAX(price) AS high
         FROM trades
         GROUP BY account_id, SESSION(ts, INTERVAL '2' SECOND)"
    ).await;
    streams_created.push(("rapid_fire".into(), rapid_ok));

    // ── Stream 4: Wash Score (TUMBLE + CASE WHEN) ──
    let wash_ok = try_create(&db, "wash_score",
        "CREATE STREAM wash_score AS
         SELECT account_id,
                symbol,
                SUM(CASE WHEN side = 'buy' THEN volume ELSE CAST(0 AS BIGINT) END) AS buy_volume,
                SUM(CASE WHEN side = 'sell' THEN volume ELSE CAST(0 AS BIGINT) END) AS sell_volume,
                SUM(CASE WHEN side = 'buy' THEN 1 ELSE 0 END) AS buy_count,
                SUM(CASE WHEN side = 'sell' THEN 1 ELSE 0 END) AS sell_count
         FROM trades
         GROUP BY account_id, symbol, TUMBLE(ts, INTERVAL '5' SECOND)"
    ).await;
    streams_created.push(("wash_score".into(), wash_ok));

    // ── Stream 5: Suspicious Match (INNER JOIN) ──
    let match_ok = try_create(&db, "suspicious_match",
        "CREATE STREAM suspicious_match AS
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
         AND o.ts BETWEEN t.ts - 10000 AND t.ts + 10000"
    ).await;
    streams_created.push(("suspicious_match".into(), match_ok));

    // ── Create sinks + subscribe ──
    macro_rules! setup_sub {
        ($db:expr, $name:expr, $ok:expr, $ty:ty) => {
            if $ok {
                let _ = $db.execute(&format!("CREATE SINK {}_sink FROM {}", $name, $name)).await;
                match $db.subscribe::<$ty>($name) {
                    Ok(sub) => Some(sub),
                    Err(e) => {
                        eprintln!("  [WARN] Subscribe to {} failed: {e}", $name);
                        None
                    }
                }
            } else {
                None
            }
        };
    }

    let vol_baseline_sub = setup_sub!(db, "vol_baseline", vol_ok, VolumeBaseline);
    let ohlc_vol_sub = setup_sub!(db, "ohlc_vol", ohlc_ok, OhlcVolatility);
    let rapid_fire_sub = setup_sub!(db, "rapid_fire", rapid_ok, RapidFireBurst);
    let wash_score_sub = setup_sub!(db, "wash_score", wash_ok, WashScore);
    let suspicious_match_sub = setup_sub!(db, "suspicious_match", match_ok, SuspiciousMatch);

    db.start().await?;

    let trade_source = db.source::<Trade>("trades")?;
    let order_source = db.source::<Order>("orders")?;

    Ok(DetectionPipeline {
        db,
        trade_source,
        order_source,
        vol_baseline_sub,
        ohlc_vol_sub,
        rapid_fire_sub,
        wash_score_sub,
        suspicious_match_sub,
        streams_created,
    })
}

async fn try_create(db: &LaminarDB, name: &str, sql: &str) -> bool {
    match db.execute(sql).await {
        Ok(_) => {
            eprintln!("  [OK] {} created", name);
            true
        }
        Err(e) => {
            eprintln!("  [WARN] {} failed: {e}", name);
            false
        }
    }
}
