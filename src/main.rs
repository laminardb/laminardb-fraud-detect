use std::time::{Duration, Instant};

use clap::Parser;

use laminardb_fraud_detect::alerts::AlertEngine;
use laminardb_fraud_detect::detection;
use laminardb_fraud_detect::generator::FraudGenerator;
use laminardb_fraud_detect::latency::LatencyTracker;
use laminardb_fraud_detect::tui;
use laminardb_fraud_detect::web;

#[derive(Parser)]
#[command(name = "laminardb-fraud-detect", about = "Real-time fraud detection with LaminarDB")]
struct Cli {
    /// Run mode: tui, web, or headless
    #[arg(long, default_value = "tui")]
    mode: String,

    /// Web server port (web mode only)
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Fraud injection rate (0.0-1.0)
    #[arg(long, default_value = "0.05")]
    fraud_rate: f64,

    /// Run duration in seconds (0 = infinite)
    #[arg(long, default_value = "0")]
    duration: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.mode.as_str() {
        "tui" => tui::run(cli.fraud_rate, cli.duration).await?,
        "web" => web::run(cli.port, cli.fraud_rate, cli.duration).await?,
        "headless" => run_headless(cli.fraud_rate, cli.duration).await?,
        other => eprintln!("Unknown mode: {other}. Use --mode tui|web|headless"),
    }

    Ok(())
}

async fn run_headless(fraud_rate: f64, duration_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== laminardb-fraud-detect (headless) ===");
    println!("Fraud rate: {:.0}%, Duration: {}s", fraud_rate * 100.0, if duration_secs == 0 { "infinite".to_string() } else { duration_secs.to_string() });
    println!();

    let pipeline = detection::setup().await?;
    println!();

    let mut gen = FraudGenerator::new(fraud_rate);
    let mut alert_engine = AlertEngine::new();
    let mut latency = LatencyTracker::new();
    let mut total_trades = 0u64;
    let mut total_orders = 0u64;
    let mut stream_counts: [u64; 6] = [0; 6];

    let run_duration = if duration_secs == 0 { Duration::from_secs(3600) } else { Duration::from_secs(duration_secs) };
    let start = Instant::now();

    while start.elapsed() < run_duration {
        let ts = FraudGenerator::now_ms();
        let gen_instant = Instant::now();

        let (trades, orders) = gen.generate_cycle(ts);
        total_trades += trades.len() as u64;
        total_orders += orders.len() as u64;

        let push_start = latency.record_push_start();
        pipeline.trade_source.push_batch(trades);
        if !orders.is_empty() {
            pipeline.order_source.push_batch(orders);
        }
        pipeline.trade_source.watermark(ts + 10_000);
        pipeline.order_source.watermark(ts + 10_000);
        latency.record_push_end(push_start);

        // Poll all streams
        if let Some(ref sub) = pipeline.vol_baseline_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[0] += 1;
                    if let Some(alert) = alert_engine.evaluate_volume(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
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
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
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
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
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
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
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
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
                    }
                }
            }
        }

        if let Some(ref sub) = pipeline.asof_match_sub {
            while let Some(rows) = sub.poll() {
                latency.record_poll();
                for row in &rows {
                    stream_counts[5] += 1;
                    if let Some(alert) = alert_engine.evaluate_asof(row, gen_instant) {
                        latency.record_alert(gen_instant);
                        println!("  ALERT | {:?} | {} | {}us", alert.severity, alert.description, alert.latency_us);
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Summary
    println!();
    println!("=== Results ===");
    println!("  Trades pushed:      {}", total_trades);
    println!("  Orders pushed:      {}", total_orders);
    println!("  Alerts generated:   {}", alert_engine.total_alerts());
    println!();
    println!("  Stream outputs:");
    let names = ["vol_baseline", "ohlc_vol", "rapid_fire", "wash_score", "suspicious_match", "asof_match"];
    for (i, name) in names.iter().enumerate() {
        println!("    {:<20} {}", name, stream_counts[i]);
    }
    println!();
    let push = latency.push_stats();
    let proc = latency.processing_stats();
    let alert_lat = latency.alert_stats();
    println!("  Latency (microseconds):");
    println!("    Push:       p50={} p95={} p99={} min={} max={}", push.p50_us, push.p95_us, push.p99_us, push.min_us, push.max_us);
    println!("    Processing: p50={} p95={} p99={} min={} max={}", proc.p50_us, proc.p95_us, proc.p99_us, proc.min_us, proc.max_us);
    println!("    Alert:      p50={} p95={} p99={} min={} max={}", alert_lat.p50_us, alert_lat.p95_us, alert_lat.p99_us, alert_lat.min_us, alert_lat.max_us);
    println!();

    for (name, count) in alert_engine.alert_counts() {
        println!("  {}: {}", name, count);
    }

    let _ = pipeline.db.shutdown().await;
    Ok(())
}
