use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use serde::Serialize;

use crate::types::*;

#[derive(Debug, Clone, Serialize)]
pub enum AlertSeverity {
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub enum AlertType {
    VolumeAnomaly,
    PriceSpike,
    RapidFire,
    WashTrading,
    SuspiciousMatch,
}

impl AlertType {
    pub fn label(&self) -> &'static str {
        match self {
            AlertType::VolumeAnomaly => "VolumeAnomaly",
            AlertType::PriceSpike => "PriceSpike",
            AlertType::RapidFire => "RapidFire",
            AlertType::WashTrading => "WashTrading",
            AlertType::SuspiciousMatch => "SuspiciousMatch",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: u64,
    pub alert_type: AlertType,
    pub severity: AlertSeverity,
    pub description: String,
    pub latency_us: u64,
    pub timestamp_ms: i64,
}

pub struct AlertEngine {
    next_id: u64,
    alerts: VecDeque<Alert>,
    vol_baselines: HashMap<String, VecDeque<i64>>,
    pub volume_ratio_threshold: f64,
    pub price_range_pct_threshold: f64,
    pub rapid_fire_threshold: i64,
    pub wash_imbalance_threshold: f64,
    pub match_price_diff_threshold: f64,
    counts: HashMap<String, u64>,
}

impl AlertEngine {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            alerts: VecDeque::with_capacity(200),
            vol_baselines: HashMap::new(),
            volume_ratio_threshold: 2.0,
            price_range_pct_threshold: 0.002,
            rapid_fire_threshold: 5,
            wash_imbalance_threshold: 0.3,
            match_price_diff_threshold: 1.0,
            counts: HashMap::new(),
        }
    }

    pub fn recent_alerts(&self) -> &VecDeque<Alert> {
        &self.alerts
    }

    pub fn alert_counts(&self) -> &HashMap<String, u64> {
        &self.counts
    }

    pub fn total_alerts(&self) -> u64 {
        self.counts.values().sum()
    }

    fn push_alert(&mut self, alert: Alert) {
        *self.counts.entry(alert.alert_type.label().to_string()).or_insert(0) += 1;
        if self.alerts.len() >= 200 {
            self.alerts.pop_front();
        }
        self.alerts.push_back(alert);
    }

    pub fn evaluate_volume(&mut self, row: &VolumeBaseline, gen_instant: Instant) -> Option<Alert> {
        let history = self.vol_baselines.entry(row.symbol.clone()).or_insert_with(VecDeque::new);
        let avg = if history.is_empty() {
            row.total_volume
        } else {
            history.iter().sum::<i64>() / history.len() as i64
        };

        if history.len() >= 20 {
            history.pop_front();
        }
        history.push_back(row.total_volume);

        if avg > 0 {
            let ratio = row.total_volume as f64 / avg as f64;
            if ratio > self.volume_ratio_threshold {
                let severity = if ratio > 10.0 {
                    AlertSeverity::Critical
                } else if ratio > 5.0 {
                    AlertSeverity::High
                } else {
                    AlertSeverity::Medium
                };
                self.next_id += 1;
                let alert = Alert {
                    id: self.next_id,
                    alert_type: AlertType::VolumeAnomaly,
                    severity,
                    description: format!("{} vol={} avg={} ({:.1}x)", row.symbol, row.total_volume, avg, ratio),
                    latency_us: gen_instant.elapsed().as_micros() as u64,
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                };
                self.push_alert(alert.clone());
                return Some(alert);
            }
        }
        None
    }

    pub fn evaluate_ohlc(&mut self, row: &OhlcVolatility, gen_instant: Instant) -> Option<Alert> {
        if row.open > 0.0 {
            let range_pct = row.price_range / row.open;
            if range_pct > self.price_range_pct_threshold {
                let severity = if range_pct > 0.05 {
                    AlertSeverity::Critical
                } else if range_pct > 0.01 {
                    AlertSeverity::High
                } else {
                    AlertSeverity::Medium
                };
                self.next_id += 1;
                let alert = Alert {
                    id: self.next_id,
                    alert_type: AlertType::PriceSpike,
                    severity,
                    description: format!("{} range={:.2}% O={:.2} H={:.2} L={:.2}", row.symbol, range_pct * 100.0, row.open, row.high, row.low),
                    latency_us: gen_instant.elapsed().as_micros() as u64,
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                };
                self.push_alert(alert.clone());
                return Some(alert);
            }
        }
        None
    }

    pub fn evaluate_rapid_fire(&mut self, row: &RapidFireBurst, gen_instant: Instant) -> Option<Alert> {
        if row.burst_trades >= self.rapid_fire_threshold {
            let severity = if row.burst_trades > 50 {
                AlertSeverity::Critical
            } else if row.burst_trades > 20 {
                AlertSeverity::High
            } else {
                AlertSeverity::Medium
            };
            self.next_id += 1;
            let alert = Alert {
                id: self.next_id,
                alert_type: AlertType::RapidFire,
                severity,
                description: format!("{} {} trades vol={}", row.account_id, row.burst_trades, row.burst_volume),
                latency_us: gen_instant.elapsed().as_micros() as u64,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            };
            self.push_alert(alert.clone());
            return Some(alert);
        }
        None
    }

    pub fn evaluate_wash(&mut self, row: &WashScore, gen_instant: Instant) -> Option<Alert> {
        let total = row.buy_volume + row.sell_volume;
        if total > 0 && row.buy_count >= 2 && row.sell_count >= 2 {
            let imbalance = (row.buy_volume - row.sell_volume).unsigned_abs() as f64 / total as f64;
            if imbalance < self.wash_imbalance_threshold {
                let severity = if imbalance < 0.02 {
                    AlertSeverity::Critical
                } else if imbalance < 0.05 {
                    AlertSeverity::High
                } else {
                    AlertSeverity::Medium
                };
                self.next_id += 1;
                let alert = Alert {
                    id: self.next_id,
                    alert_type: AlertType::WashTrading,
                    severity,
                    description: format!("{} {} imb={:.3} buy={} sell={}", row.account_id, row.symbol, imbalance, row.buy_volume, row.sell_volume),
                    latency_us: gen_instant.elapsed().as_micros() as u64,
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                };
                self.push_alert(alert.clone());
                return Some(alert);
            }
        }
        None
    }

    pub fn evaluate_match(&mut self, row: &SuspiciousMatch, gen_instant: Instant) -> Option<Alert> {
        if row.price_diff.abs() < self.match_price_diff_threshold {
            let severity = if row.price_diff.abs() < 0.001 {
                AlertSeverity::High
            } else {
                AlertSeverity::Medium
            };
            self.next_id += 1;
            let alert = Alert {
                id: self.next_id,
                alert_type: AlertType::SuspiciousMatch,
                severity,
                description: format!("{} {} order={} diff={:.4}", row.account_id, row.symbol, row.order_id, row.price_diff),
                latency_us: gen_instant.elapsed().as_micros() as u64,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            };
            self.push_alert(alert.clone());
            return Some(alert);
        }
        None
    }
}
