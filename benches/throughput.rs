use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tokio::runtime::Runtime;

use laminardb_fraud_detect::alerts::AlertEngine;
use laminardb_fraud_detect::detection;
use laminardb_fraud_detect::generator::FraudGenerator;
use laminardb_fraud_detect::latency::LatencyTracker;

fn push_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let pipeline = rt.block_on(detection::setup()).unwrap();
    let mut gen = FraudGenerator::new(0.0);

    let mut group = c.benchmark_group("push_throughput");
    for size in [100, 500, 1000, 5000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let ts = FraudGenerator::now_ms();
                let (trades, orders) = gen.generate_stress_cycle(ts, size);
                pipeline.trade_source.push_batch(trades);
                if !orders.is_empty() {
                    pipeline.order_source.push_batch(orders);
                }
                pipeline.trade_source.watermark(ts + 10_000);
                pipeline.order_source.watermark(ts + 10_000);
            });
        });
    }
    group.finish();

    rt.block_on(pipeline.db.shutdown()).ok();
}

fn end_to_end(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let pipeline = rt.block_on(detection::setup()).unwrap();
    let mut gen = FraudGenerator::new(0.0);
    let mut alert_engine = AlertEngine::new();
    let mut latency = LatencyTracker::new();

    let mut group = c.benchmark_group("end_to_end");
    for size in [100, 500, 1000, 5000] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let ts = FraudGenerator::now_ms();
                let gen_instant = std::time::Instant::now();

                let (trades, orders) = gen.generate_stress_cycle(ts, size);
                pipeline.trade_source.push_batch(trades);
                if !orders.is_empty() {
                    pipeline.order_source.push_batch(orders);
                }
                pipeline.trade_source.watermark(ts + 10_000);
                pipeline.order_source.watermark(ts + 10_000);

                // Poll all streams
                if let Some(ref sub) = pipeline.vol_baseline_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_volume(row, gen_instant);
                        }
                    }
                }
                if let Some(ref sub) = pipeline.ohlc_vol_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_ohlc(row, gen_instant);
                        }
                    }
                }
                if let Some(ref sub) = pipeline.rapid_fire_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_rapid_fire(row, gen_instant);
                        }
                    }
                }
                if let Some(ref sub) = pipeline.wash_score_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_wash(row, gen_instant);
                        }
                    }
                }
                if let Some(ref sub) = pipeline.suspicious_match_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_match(row, gen_instant);
                        }
                    }
                }
                if let Some(ref sub) = pipeline.asof_match_sub {
                    while let Some(rows) = sub.poll() {
                        for row in &rows {
                            alert_engine.evaluate_asof(row, gen_instant);
                        }
                    }
                }

                latency.reset();
            });
        });
    }
    group.finish();

    rt.block_on(pipeline.db.shutdown()).ok();
}

fn pipeline_setup(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("pipeline_setup", |b| {
        b.iter(|| {
            let pipeline = rt.block_on(detection::setup()).unwrap();
            rt.block_on(pipeline.db.shutdown()).ok();
        });
    });
}

criterion_group!(benches, push_throughput, end_to_end, pipeline_setup);
criterion_main!(benches);
