use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

use crate::alerts::{Alert, AlertEngine};
use crate::detection;
use crate::generator::FraudGenerator;
use crate::latency::{LatencyStats, LatencyTracker};

#[derive(Clone, Serialize)]
struct DashboardUpdate {
    alerts: Vec<Alert>,
    latency: LatencyUpdate,
    streams: Vec<StreamStatus>,
    alert_counts: HashMap<String, u64>,
    total_trades: u64,
    total_orders: u64,
    total_alerts: u64,
    uptime_secs: u64,
    prices: HashMap<String, f64>,
}

#[derive(Clone, Serialize)]
struct LatencyUpdate {
    push: LatencyStats,
    processing: LatencyStats,
    alert: LatencyStats,
}

#[derive(Clone, Serialize)]
struct StreamStatus {
    name: String,
    count: u64,
    active: bool,
}

struct AppState {
    tx: broadcast::Sender<String>,
}

pub async fn run(port: u16, fraud_rate: f64, duration: u64) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, _) = broadcast::channel::<String>(256);
    let state = Arc::new(AppState { tx: tx.clone() });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    // Spawn the detection engine
    let engine_tx = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = run_engine(engine_tx, fraud_rate, duration).await {
            eprintln!("Engine error: {e}");
        }
    });

    let addr = format!("0.0.0.0:{port}");
    println!("Dashboard at http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let rx = state.tx.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, rx))
}

async fn handle_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<String>) {
    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg.into())).await.is_err() {
            break;
        }
    }
}

async fn run_engine(
    tx: broadcast::Sender<String>,
    fraud_rate: f64,
    duration: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pipeline = detection::setup().await?;
    let mut gen = FraudGenerator::new(fraud_rate);
    let mut alert_engine = AlertEngine::new();
    let mut latency = LatencyTracker::new();
    let mut total_trades = 0u64;
    let mut total_orders = 0u64;
    let mut stream_counts: [u64; 5] = [0; 5];
    let mut prices: HashMap<String, f64> = HashMap::new();
    let mut recent_alerts: Vec<Alert> = Vec::new();

    let run_duration = if duration == 0 {
        Duration::from_secs(3600)
    } else {
        Duration::from_secs(duration)
    };
    let start = Instant::now();

    while start.elapsed() < run_duration {
        let ts = FraudGenerator::now_ms();
        let gen_instant = Instant::now();

        let (trades, orders) = gen.generate_cycle(ts);
        total_trades += trades.len() as u64;
        total_orders += orders.len() as u64;

        for (sym, price) in gen.current_prices() {
            prices.insert(sym.clone(), *price);
        }

        let push_start = latency.record_push_start();
        pipeline.trade_source.push_batch(trades);
        if !orders.is_empty() {
            pipeline.order_source.push_batch(orders);
        }
        pipeline.trade_source.watermark(ts + 10_000);
        pipeline.order_source.watermark(ts + 10_000);
        latency.record_push_end(push_start);

        recent_alerts.clear();

        // Poll all streams
        if let Some(ref sub) = pipeline.vol_baseline_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[0] += 1;
                    if let Some(alert) = alert_engine.evaluate_volume(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        recent_alerts.push(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.ohlc_vol_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[1] += 1;
                    if let Some(alert) = alert_engine.evaluate_ohlc(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        recent_alerts.push(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.rapid_fire_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[2] += 1;
                    if let Some(alert) = alert_engine.evaluate_rapid_fire(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        recent_alerts.push(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.wash_score_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[3] += 1;
                    if let Some(alert) = alert_engine.evaluate_wash(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        recent_alerts.push(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.suspicious_match_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[4] += 1;
                    if let Some(alert) = alert_engine.evaluate_match(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        recent_alerts.push(alert);
                    }
                }
            }
        }

        // Broadcast update to WebSocket clients
        let names = ["vol_baseline", "ohlc_vol", "rapid_fire", "wash_score", "suspicious_match"];
        let streams: Vec<StreamStatus> = names
            .iter()
            .enumerate()
            .map(|(i, name)| StreamStatus {
                name: name.to_string(),
                count: stream_counts[i],
                active: stream_counts[i] > 0,
            })
            .collect();

        let update = DashboardUpdate {
            alerts: recent_alerts.clone(),
            latency: LatencyUpdate {
                push: latency.push_stats(),
                processing: latency.processing_stats(),
                alert: latency.alert_stats(),
            },
            streams,
            alert_counts: alert_engine.alert_counts().clone(),
            total_trades,
            total_orders,
            total_alerts: alert_engine.total_alerts(),
            uptime_secs: start.elapsed().as_secs(),
            prices: prices.clone(),
        };

        if let Ok(json) = serde_json::to_string(&update) {
            let _ = tx.send(json);
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let _ = pipeline.db.shutdown().await;
    Ok(())
}
