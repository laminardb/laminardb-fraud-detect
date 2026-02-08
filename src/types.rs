use laminar_derive::{FromRow, Record};

// ── Input Types (pushed into sources) ──

#[derive(Debug, Clone, Record)]
pub struct Trade {
    pub account_id: String,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub volume: i64,
    pub order_ref: String,
    #[event_time]
    pub ts: i64,
}

#[derive(Debug, Clone, Record)]
pub struct Order {
    pub order_id: String,
    pub account_id: String,
    pub symbol: String,
    pub side: String,
    pub quantity: i64,
    pub price: f64,
    #[event_time]
    pub ts: i64,
}

// ── Output Types (polled from subscriptions) ──

#[derive(Debug, Clone, FromRow)]
pub struct VolumeBaseline {
    pub symbol: String,
    pub total_volume: i64,
    pub trade_count: i64,
    pub avg_price: f64,
}

#[derive(Debug, Clone, FromRow)]
pub struct OhlcVolatility {
    pub symbol: String,
    pub bar_start: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub price_range: f64,
}

#[derive(Debug, Clone, FromRow)]
pub struct RapidFireBurst {
    pub account_id: String,
    pub burst_trades: i64,
    pub burst_volume: i64,
    pub low: f64,
    pub high: f64,
}

#[derive(Debug, Clone, FromRow)]
pub struct WashScore {
    pub account_id: String,
    pub symbol: String,
    pub buy_volume: i64,
    pub sell_volume: i64,
    pub buy_count: i64,
    pub sell_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct SuspiciousMatch {
    pub symbol: String,
    pub trade_price: f64,
    pub volume: i64,
    pub order_id: String,
    pub account_id: String,
    pub side: String,
    pub order_price: f64,
    pub price_diff: f64,
}
