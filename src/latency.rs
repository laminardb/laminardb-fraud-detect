use std::collections::VecDeque;
use std::time::Instant;

use serde::Serialize;

const WINDOW_SIZE: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub count: usize,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self { p50_us: 0, p95_us: 0, p99_us: 0, min_us: 0, max_us: 0, count: 0 }
    }
}

pub struct LatencyTracker {
    push_latencies: VecDeque<u64>,
    processing_latencies: VecDeque<u64>,
    alert_latencies: VecDeque<u64>,
    last_push_instant: Option<Instant>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            push_latencies: VecDeque::with_capacity(WINDOW_SIZE),
            processing_latencies: VecDeque::with_capacity(WINDOW_SIZE),
            alert_latencies: VecDeque::with_capacity(WINDOW_SIZE),
            last_push_instant: None,
        }
    }

    pub fn reset(&mut self) {
        self.push_latencies.clear();
        self.processing_latencies.clear();
        self.alert_latencies.clear();
        self.last_push_instant = None;
    }

    pub fn record_push_start(&self) -> Instant {
        Instant::now()
    }

    pub fn record_push_end(&mut self, start: Instant) {
        let us = start.elapsed().as_micros() as u64;
        push_capped(&mut self.push_latencies, us);
        self.last_push_instant = Some(Instant::now());
    }

    pub fn record_poll(&mut self) {
        if let Some(push_time) = self.last_push_instant {
            let us = push_time.elapsed().as_micros() as u64;
            push_capped(&mut self.processing_latencies, us);
        }
    }

    pub fn record_alert(&mut self, gen_instant: Instant) {
        let us = gen_instant.elapsed().as_micros() as u64;
        push_capped(&mut self.alert_latencies, us);
    }

    pub fn push_stats(&self) -> LatencyStats {
        compute_stats(&self.push_latencies)
    }

    pub fn processing_stats(&self) -> LatencyStats {
        compute_stats(&self.processing_latencies)
    }

    pub fn alert_stats(&self) -> LatencyStats {
        compute_stats(&self.alert_latencies)
    }
}

fn push_capped(q: &mut VecDeque<u64>, val: u64) {
    if q.len() >= WINDOW_SIZE {
        q.pop_front();
    }
    q.push_back(val);
}

fn compute_stats(q: &VecDeque<u64>) -> LatencyStats {
    if q.is_empty() {
        return LatencyStats::default();
    }
    let mut sorted: Vec<u64> = q.iter().copied().collect();
    sorted.sort_unstable();
    let n = sorted.len();
    LatencyStats {
        p50_us: sorted[n * 50 / 100],
        p95_us: sorted[n * 95 / 100],
        p99_us: sorted[(n * 99 / 100).min(n - 1)],
        min_us: sorted[0],
        max_us: sorted[n - 1],
        count: n,
    }
}
