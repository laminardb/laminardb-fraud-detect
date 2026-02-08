use rand::Rng;
use std::collections::HashMap;

use crate::types::{Order, Trade};

pub const SYMBOLS: &[(&str, f64)] = &[
    ("AAPL", 150.0),
    ("GOOGL", 2800.0),
    ("MSFT", 420.0),
    ("AMZN", 185.0),
    ("TSLA", 250.0),
];

const NORMAL_ACCOUNTS: &[&str] = &["ACCT-001", "ACCT-002", "ACCT-003", "ACCT-004", "ACCT-005"];
const FRAUD_ACCOUNTS: &[&str] = &["FRAUD-01", "FRAUD-02", "FRAUD-03"];

#[derive(Debug, Clone, Copy)]
enum FraudScenario {
    VolumeSpike,
    PriceManipulation,
    RapidFire,
    WashTrading,
}

const ALL_SCENARIOS: &[FraudScenario] = &[
    FraudScenario::VolumeSpike,
    FraudScenario::PriceManipulation,
    FraudScenario::RapidFire,
    FraudScenario::WashTrading,
];

pub struct FraudGenerator {
    prices: HashMap<String, f64>,
    order_seq: u64,
    trade_seq: u64,
    pub fraud_rate: f64,
    manipulation_remaining: u32,
    manipulation_symbol: Option<String>,
}

impl FraudGenerator {
    pub fn new(fraud_rate: f64) -> Self {
        let mut prices = HashMap::new();
        for (sym, base) in SYMBOLS {
            prices.insert(sym.to_string(), *base);
        }
        Self {
            prices,
            order_seq: 0,
            trade_seq: 0,
            fraud_rate,
            manipulation_remaining: 0,
            manipulation_symbol: None,
        }
    }

    pub fn now_ms() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    pub fn current_prices(&self) -> &HashMap<String, f64> {
        &self.prices
    }

    /// Generate trades + optional orders for one cycle. Returns (trades, orders).
    pub fn generate_cycle(&mut self, ts: i64) -> (Vec<Trade>, Vec<Order>) {
        let mut rng = rand::thread_rng();

        // Check if we should inject fraud this cycle
        let inject_fraud = rng.gen_bool(self.fraud_rate.min(1.0));

        if inject_fraud {
            let scenario = ALL_SCENARIOS[rng.gen_range(0..ALL_SCENARIOS.len())];
            match scenario {
                FraudScenario::VolumeSpike => return self.inject_volume_spike(ts),
                FraudScenario::PriceManipulation => {
                    self.manipulation_remaining = 3;
                    let idx = rng.gen_range(0..SYMBOLS.len());
                    self.manipulation_symbol = Some(SYMBOLS[idx].0.to_string());
                }
                FraudScenario::RapidFire => return self.inject_rapid_fire(ts),
                FraudScenario::WashTrading => return self.inject_wash_trading(ts),
            }
        }

        // Normal cycle (or price manipulation continuation)
        self.generate_normal(ts)
    }

    fn generate_normal(&mut self, ts: i64) -> (Vec<Trade>, Vec<Order>) {
        let mut rng = rand::thread_rng();
        let mut trades = Vec::with_capacity(SYMBOLS.len());
        let mut orders = Vec::new();

        for (sym, _) in SYMBOLS {
            let symbol = sym.to_string();
            let price = self.prices.get_mut(&symbol).unwrap();

            // Price manipulation: push price up 2-4% per cycle for 3 cycles
            if self.manipulation_remaining > 0
                && self.manipulation_symbol.as_deref() == Some(sym)
            {
                let push = *price * rng.gen_range(0.02..0.04);
                *price += push;
                self.manipulation_remaining -= 1;
                if self.manipulation_remaining == 0 {
                    // Sharp reversal
                    *price *= 0.92;
                    self.manipulation_symbol = None;
                }
            } else {
                let change = *price * rng.gen_range(-0.005..0.005);
                *price += change;
            }

            let account = NORMAL_ACCOUNTS[rng.gen_range(0..NORMAL_ACCOUNTS.len())];
            let side = if rng.gen_bool(0.5) { "buy" } else { "sell" };
            let volume = rng.gen_range(10..500);

            self.trade_seq += 1;
            let order_ref = format!("T-{:06}", self.trade_seq);

            trades.push(Trade {
                account_id: account.to_string(),
                symbol: symbol.clone(),
                side: side.to_string(),
                price: *price,
                volume,
                order_ref: order_ref.clone(),
                ts,
            });

            // ~30% chance to generate a matching order
            if rng.gen_bool(0.3) {
                self.order_seq += 1;
                let offset = *price * rng.gen_range(-0.002..0.002);
                orders.push(Order {
                    order_id: format!("ORD-{:06}", self.order_seq),
                    account_id: account.to_string(),
                    symbol,
                    side: side.to_string(),
                    quantity: volume,
                    price: *price + offset,
                    ts,
                });
            }
        }

        (trades, orders)
    }

    fn inject_volume_spike(&mut self, ts: i64) -> (Vec<Trade>, Vec<Order>) {
        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..SYMBOLS.len());
        let (sym, _) = SYMBOLS[idx];
        let symbol = sym.to_string();
        let price = *self.prices.get(&symbol).unwrap();
        let fraud_acct = FRAUD_ACCOUNTS[rng.gen_range(0..FRAUD_ACCOUNTS.len())];

        let mut trades = Vec::new();
        // Generate 5-10 trades with 10-50x volume
        let count = rng.gen_range(5..=10);
        for _ in 0..count {
            self.trade_seq += 1;
            let spike_vol = rng.gen_range(10..500) * rng.gen_range(10..50);
            trades.push(Trade {
                account_id: fraud_acct.to_string(),
                symbol: symbol.clone(),
                side: if rng.gen_bool(0.5) { "buy" } else { "sell" }.to_string(),
                price: price + price * rng.gen_range(-0.001..0.001),
                volume: spike_vol,
                order_ref: format!("T-{:06}", self.trade_seq),
                ts,
            });
        }

        // Also include normal trades for other symbols
        let (mut normal, orders) = self.generate_normal(ts);
        trades.append(&mut normal);
        (trades, orders)
    }

    fn inject_rapid_fire(&mut self, ts: i64) -> (Vec<Trade>, Vec<Order>) {
        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..SYMBOLS.len());
        let (sym, _) = SYMBOLS[idx];
        let symbol = sym.to_string();
        let price = *self.prices.get(&symbol).unwrap();
        let fraud_acct = FRAUD_ACCOUNTS[rng.gen_range(0..FRAUD_ACCOUNTS.len())];

        let mut trades = Vec::new();
        // 20-30 trades spaced 50-100ms apart
        let count = rng.gen_range(20..=30);
        for i in 0..count {
            self.trade_seq += 1;
            let t = ts + (i as i64) * rng.gen_range(50..100);
            trades.push(Trade {
                account_id: fraud_acct.to_string(),
                symbol: symbol.clone(),
                side: if rng.gen_bool(0.5) { "buy" } else { "sell" }.to_string(),
                price: price + price * rng.gen_range(-0.001..0.001),
                volume: rng.gen_range(10..100),
                order_ref: format!("T-{:06}", self.trade_seq),
                ts: t,
            });
        }

        let (mut normal, orders) = self.generate_normal(ts);
        trades.append(&mut normal);
        (trades, orders)
    }

    fn inject_wash_trading(&mut self, ts: i64) -> (Vec<Trade>, Vec<Order>) {
        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..SYMBOLS.len());
        let (sym, _) = SYMBOLS[idx];
        let symbol = sym.to_string();
        let price = *self.prices.get(&symbol).unwrap();
        let fraud_acct = FRAUD_ACCOUNTS[rng.gen_range(0..FRAUD_ACCOUNTS.len())];

        let mut trades = Vec::new();
        // Generate equal buy/sell pairs from same account
        let pairs = rng.gen_range(3..=6);
        for _ in 0..pairs {
            let vol = rng.gen_range(50..200);
            self.trade_seq += 1;
            trades.push(Trade {
                account_id: fraud_acct.to_string(),
                symbol: symbol.clone(),
                side: "buy".to_string(),
                price,
                volume: vol,
                order_ref: format!("T-{:06}", self.trade_seq),
                ts,
            });
            self.trade_seq += 1;
            trades.push(Trade {
                account_id: fraud_acct.to_string(),
                symbol: symbol.clone(),
                side: "sell".to_string(),
                price: price + rng.gen_range(-0.01..0.01),
                volume: vol,
                order_ref: format!("T-{:06}", self.trade_seq),
                ts,
            });
        }

        let (mut normal, orders) = self.generate_normal(ts);
        trades.append(&mut normal);
        (trades, orders)
    }
}
