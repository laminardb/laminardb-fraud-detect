use std::time::{Duration, Instant};

use crate::alerts::AlertEngine;
use crate::detection;
use crate::generator::FraudGenerator;
use crate::latency::LatencyTracker;

struct StressLevel {
    trades_per_cycle: usize,
    sleep_ms: u64,
    target_tps: u64,
}

const LEVELS: &[StressLevel] = &[
    StressLevel { trades_per_cycle: 10,   sleep_ms: 100, target_tps: 100 },
    StressLevel { trades_per_cycle: 25,   sleep_ms: 100, target_tps: 250 },
    StressLevel { trades_per_cycle: 50,   sleep_ms: 50,  target_tps: 1_000 },
    StressLevel { trades_per_cycle: 100,  sleep_ms: 50,  target_tps: 2_000 },
    StressLevel { trades_per_cycle: 200,  sleep_ms: 20,  target_tps: 10_000 },
    StressLevel { trades_per_cycle: 500,  sleep_ms: 10,  target_tps: 50_000 },
    StressLevel { trades_per_cycle: 1000, sleep_ms: 5,   target_tps: 200_000 },
];

struct LevelResult {
    level: usize,
    target_tps: u64,
    actual_tps: u64,
    total_trades: u64,
    total_orders: u64,
    total_alerts: u64,
    push_p50: u64,
    push_p95: u64,
    push_p99: u64,
    proc_p50: u64,
    proc_p95: u64,
    proc_p99: u64,
    stream_counts: [u64; 6],
    duration_secs: f64,
}

pub async fn run(level_duration: u64) -> Result<(), Box<dyn std::error::Error>> {
    let total_time = LEVELS.len() as u64 * level_duration;
    println!("=== STRESS TEST ===");
    println!("Levels: {}, Duration per level: {}s, Total estimated: {}s",
        LEVELS.len(), level_duration, total_time);
    println!();

    let pipeline = detection::setup().await?;
    let mut gen = FraudGenerator::new(0.0); // no fraud — pure throughput
    let mut alert_engine = AlertEngine::new();
    let mut latency = LatencyTracker::new();
    let mut results: Vec<LevelResult> = Vec::new();

    let level_dur = Duration::from_secs(level_duration);

    for (idx, level) in LEVELS.iter().enumerate() {
        let level_num = idx + 1;
        print!("Level {}/{}: target ~{} trades/sec, {} trades/cycle, {}ms sleep ... ",
            level_num, LEVELS.len(), level.target_tps, level.trades_per_cycle, level.sleep_ms);

        latency.reset();
        let mut total_trades = 0u64;
        let mut total_orders = 0u64;
        let mut total_alerts = 0u64;
        let mut stream_counts: [u64; 6] = [0; 6];

        // Sequential event timestamps: each cycle starts where the previous ended.
        // This prevents cross-cycle JOIN fan-out from overlapping time ranges.
        let mut event_ts: i64 = FraudGenerator::now_ms();
        let cycle_span = FraudGenerator::stress_cycle_span_ms(level.trades_per_cycle);

        let level_start = Instant::now();

        while level_start.elapsed() < level_dur {
            let gen_instant = Instant::now();

            let (trades, orders) = gen.generate_stress_cycle(event_ts, level.trades_per_cycle);
            total_trades += trades.len() as u64;
            total_orders += orders.len() as u64;

            let push_start = latency.record_push_start();
            pipeline.trade_source.push_batch(trades);
            if !orders.is_empty() {
                pipeline.order_source.push_batch(orders);
            }
            // Watermark ahead of the latest event in this cycle
            pipeline.trade_source.watermark(event_ts + cycle_span + 10_000);
            pipeline.order_source.watermark(event_ts + cycle_span + 10_000);
            latency.record_push_end(push_start);

            // Advance event_ts past this cycle so the next cycle doesn't overlap
            event_ts += cycle_span;

            // Poll all streams
            macro_rules! poll_stream {
                ($sub:expr, $idx:expr, $eval:ident) => {
                    if let Some(ref sub) = $sub {
                        while let Some(rows) = sub.poll() {
                            latency.record_poll();
                            for row in &rows {
                                stream_counts[$idx] += 1;
                                if let Some(_alert) = alert_engine.$eval(row, gen_instant) {
                                    latency.record_alert(gen_instant);
                                    total_alerts += 1;
                                }
                            }
                        }
                    }
                };
            }

            poll_stream!(pipeline.vol_baseline_sub, 0, evaluate_volume);
            poll_stream!(pipeline.ohlc_vol_sub, 1, evaluate_ohlc);
            poll_stream!(pipeline.rapid_fire_sub, 2, evaluate_rapid_fire);
            poll_stream!(pipeline.wash_score_sub, 3, evaluate_wash);
            poll_stream!(pipeline.suspicious_match_sub, 4, evaluate_match);
            poll_stream!(pipeline.asof_match_sub, 5, evaluate_asof);

            tokio::time::sleep(Duration::from_millis(level.sleep_ms)).await;
        }

        let elapsed = level_start.elapsed().as_secs_f64();
        let actual_tps = (total_trades as f64 / elapsed) as u64;

        let push = latency.push_stats();
        let proc = latency.processing_stats();

        println!("{} trades/sec (push p99={}us)", actual_tps, push.p99_us);

        results.push(LevelResult {
            level: level_num,
            target_tps: level.target_tps,
            actual_tps,
            total_trades,
            total_orders,
            total_alerts,
            push_p50: push.p50_us,
            push_p95: push.p95_us,
            push_p99: push.p99_us,
            proc_p50: proc.p50_us,
            proc_p95: proc.p95_us,
            proc_p99: proc.p99_us,
            stream_counts,
            duration_secs: elapsed,
        });
    }

    // Print summary table
    println!();
    print_results_table(&results);

    // Detect saturation point
    print_saturation_analysis(&results);

    // Detailed latency breakdown
    println!();
    print_latency_detail(&results);

    // Stream breakdown
    println!();
    println!("Stream output totals:");
    let names = ["vol_baseline", "ohlc_vol", "rapid_fire", "wash_score", "suspicious_match", "asof_match"];
    for (i, name) in names.iter().enumerate() {
        let total: u64 = results.iter().map(|r| r.stream_counts[i]).sum();
        println!("  {:<20} {}", name, total);
    }

    let _ = pipeline.db.shutdown().await;
    Ok(())
}

fn format_latency(us: u64) -> String {
    if us >= 1_000_000 {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    } else if us >= 1_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{}us", us)
    }
}

fn print_results_table(results: &[LevelResult]) {
    println!("{}", "=".repeat(90));
    println!("{:^90}", "STRESS TEST RESULTS");
    println!("{}", "=".repeat(90));
    println!(
        " {:<5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8}",
        "Level", "Target/s", "Actual/s", "Push p50", "Push p99", "Proc p99", "Alerts", "Time"
    );
    println!("{}", "-".repeat(90));

    for r in results {
        println!(
            " {:<5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8} {:>7.1}s",
            r.level,
            r.target_tps,
            r.actual_tps,
            format_latency(r.push_p50),
            format_latency(r.push_p99),
            format_latency(r.proc_p99),
            r.total_alerts,
            r.duration_secs,
        );
    }

    println!("{}", "=".repeat(90));

    // Totals
    let total_trades: u64 = results.iter().map(|r| r.total_trades).sum();
    let total_orders: u64 = results.iter().map(|r| r.total_orders).sum();
    let total_alerts: u64 = results.iter().map(|r| r.total_alerts).sum();
    let total_time: f64 = results.iter().map(|r| r.duration_secs).sum();
    println!(
        "Totals: {} trades, {} orders, {} alerts in {:.1}s",
        total_trades, total_orders, total_alerts, total_time
    );
}

fn print_latency_detail(results: &[LevelResult]) {
    println!("Latency detail (microseconds):");
    println!(
        " {:<5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Level", "Push p50", "Push p95", "Push p99", "Proc p50", "Proc p95", "Proc p99"
    );
    println!("{}", "-".repeat(75));
    for r in results {
        println!(
            " {:<5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            r.level,
            format_latency(r.push_p50),
            format_latency(r.push_p95),
            format_latency(r.push_p99),
            format_latency(r.proc_p50),
            format_latency(r.proc_p95),
            format_latency(r.proc_p99),
        );
    }
}

fn print_saturation_analysis(results: &[LevelResult]) {
    println!();

    // Find saturation: where actual < 90% of target
    let saturation = results.iter().find(|r| {
        r.actual_tps < (r.target_tps * 90 / 100)
    });

    if let Some(sat) = saturation {
        let pct = (sat.actual_tps as f64 / sat.target_tps as f64) * 100.0;
        println!(
            "Saturation point: Level {} (~{} trades/sec target)",
            sat.level, sat.target_tps
        );
        println!(
            "  Actual throughput: {}/sec ({:.0}% of target)",
            sat.actual_tps, pct
        );
        println!("  Push p99: {}", format_latency(sat.push_p99));
    } else {
        println!("No saturation detected - pipeline handled all load levels!");
    }

    // Find peak sustained throughput
    let peak = results.iter().max_by_key(|r| r.actual_tps);
    if let Some(p) = peak {
        println!(
            "Peak sustained throughput: ~{} trades/sec (Level {})",
            p.actual_tps, p.level
        );
    }
}
