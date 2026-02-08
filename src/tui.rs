use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Terminal;

use crate::alerts::{Alert, AlertEngine, AlertSeverity};
use crate::detection;
use crate::generator::FraudGenerator;
use crate::latency::LatencyTracker;

struct App {
    alerts: VecDeque<Alert>,
    latency: LatencyTracker,
    alert_engine: AlertEngine,
    stream_counts: [u64; 6],
    total_trades: u64,
    total_orders: u64,
    total_alerts: u64,
    uptime: Instant,
    should_quit: bool,
    scroll_offset: usize,
    prices: std::collections::HashMap<String, f64>,
}

impl App {
    fn new() -> Self {
        Self {
            alerts: VecDeque::with_capacity(200),
            latency: LatencyTracker::new(),
            alert_engine: AlertEngine::new(),
            stream_counts: [0; 6],
            total_trades: 0,
            total_orders: 0,
            total_alerts: 0,
            uptime: Instant::now(),
            should_quit: false,
            scroll_offset: 0,
            prices: std::collections::HashMap::new(),
        }
    }

    fn add_alert(&mut self, alert: Alert) {
        self.total_alerts += 1;
        if self.alerts.len() >= 200 {
            self.alerts.pop_front();
        }
        self.alerts.push_back(alert);
    }
}

pub async fn run(fraud_rate: f64, duration: u64) -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, fraud_rate, duration).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    fraud_rate: f64,
    duration: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pipeline = detection::setup().await?;
    let mut gen = FraudGenerator::new(fraud_rate);
    let mut app = App::new();

    let run_duration = if duration == 0 {
        Duration::from_secs(3600)
    } else {
        Duration::from_secs(duration)
    };

    while !app.should_quit && app.uptime.elapsed() < run_duration {
        terminal.draw(|f| draw(f, &app))?;

        // Handle input
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                        KeyCode::Up => {
                            if app.scroll_offset > 0 {
                                app.scroll_offset -= 1;
                            }
                        }
                        KeyCode::Down => {
                            app.scroll_offset = app.scroll_offset.saturating_add(1);
                        }
                        _ => {}
                    }
                }
            }
        }

        // Generate + push
        let ts = FraudGenerator::now_ms();
        let gen_instant = Instant::now();
        let (trades, orders) = gen.generate_cycle(ts);
        app.total_trades += trades.len() as u64;
        app.total_orders += orders.len() as u64;

        // Update prices from generator
        for (sym, price) in gen.current_prices() {
            app.prices.insert(sym.clone(), *price);
        }

        let push_start = app.latency.record_push_start();
        pipeline.trade_source.push_batch(trades);
        if !orders.is_empty() {
            pipeline.order_source.push_batch(orders);
        }
        pipeline.trade_source.watermark(ts + 10_000);
        pipeline.order_source.watermark(ts + 10_000);
        app.latency.record_push_end(push_start);

        // Poll all streams
        if let Some(ref sub) = pipeline.vol_baseline_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[0] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_volume(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.ohlc_vol_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[1] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_ohlc(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.rapid_fire_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[2] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_rapid_fire(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.wash_score_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[3] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_wash(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.suspicious_match_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[4] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_match(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
        if let Some(ref sub) = pipeline.asof_match_sub {
            while let Some(rows) = sub.poll() {
                app.latency.record_poll();
                for row in &rows {
                    app.stream_counts[5] += 1;
                    if let Some(alert) = app.alert_engine.evaluate_asof(row, gen_instant) {
                        app.latency.record_alert(gen_instant);
                        app.add_alert(alert);
                    }
                }
            }
        }
    }

    let _ = pipeline.db.shutdown().await;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let size = f.area();

    // Top bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(10),   // alert feed
            Constraint::Length(9), // latency + streams
            Constraint::Length(9), // counts + prices
        ])
        .split(size);

    draw_header(f, app, chunks[0]);
    draw_alert_feed(f, app, chunks[1]);
    draw_latency_and_streams(f, app, chunks[2]);
    draw_counts_and_prices(f, app, chunks[3]);
}

fn draw_header(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let elapsed = app.uptime.elapsed().as_secs();
    let header = vec![
        Span::styled(" laminardb-fraud-detect ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" | "),
        Span::styled(format!("Alerts: {}", app.total_alerts), Style::default().fg(Color::Yellow)),
        Span::raw(" | "),
        Span::styled(format!("Trades: {}", app.total_trades), Style::default().fg(Color::Green)),
        Span::raw(" | "),
        Span::styled(format!("Orders: {}", app.total_orders), Style::default().fg(Color::Blue)),
        Span::raw(" | "),
        Span::raw(format!("Uptime: {}s", elapsed)),
        Span::raw(" | "),
        Span::styled("q=quit  Up/Down=scroll", Style::default().fg(Color::DarkGray)),
    ];
    let p = Paragraph::new(Line::from(header))
        .block(Block::default().borders(Borders::ALL).title(" Sentinel "));
    f.render_widget(p, area);
}

fn draw_alert_feed(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let max_visible = (area.height as usize).saturating_sub(2);
    let total = app.alerts.len();
    let _start = if total > max_visible {
        let max_scroll = total - max_visible;
        max_scroll.saturating_sub(app.scroll_offset)
    } else {
        0
    };

    let rows: Vec<Row> = app
        .alerts
        .iter()
        .rev()
        .skip(app.scroll_offset)
        .take(max_visible)
        .map(|alert| {
            let (sev_str, sev_color) = match alert.severity {
                AlertSeverity::Critical => ("CRIT", Color::Red),
                AlertSeverity::High => ("HIGH", Color::Yellow),
                AlertSeverity::Medium => (" MED", Color::Cyan),
            };
            Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(sev_str, Style::default().fg(sev_color).add_modifier(Modifier::BOLD))),
                ratatui::widgets::Cell::from(format!("{:<17}", alert.alert_type.label())),
                ratatui::widgets::Cell::from(alert.description.clone()),
                ratatui::widgets::Cell::from(format!("{}us", alert.latency_us)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(18),
            Constraint::Min(30),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec!["SEV", "TYPE", "DESCRIPTION", "LATENCY"])
            .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::White)),
    )
    .block(Block::default().borders(Borders::ALL).title(format!(" Alert Feed ({}) ", total)));

    f.render_widget(table, area);
}

fn draw_latency_and_streams(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Latency panel
    let push = app.latency.push_stats();
    let proc = app.latency.processing_stats();
    let alert_lat = app.latency.alert_stats();

    let latency_text = vec![
        Line::from(vec![
            Span::styled("  Push:  ", Style::default().fg(Color::Green)),
            Span::raw(format!("p50={:<6} p95={:<6} p99={:<6}", push.p50_us, push.p95_us, push.p99_us)),
        ]),
        Line::from(vec![
            Span::styled("  Proc:  ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("p50={:<6} p95={:<6} p99={:<6}", proc.p50_us, proc.p95_us, proc.p99_us)),
        ]),
        Line::from(vec![
            Span::styled("  Alert: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("p50={:<6} p95={:<6} p99={:<6}", alert_lat.p50_us, alert_lat.p95_us, alert_lat.p99_us)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Min: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}us", push.min_us)),
            Span::raw("  "),
            Span::styled("Max: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}us", alert_lat.max_us)),
        ]),
    ];
    let latency_widget = Paragraph::new(latency_text)
        .block(Block::default().borders(Borders::ALL).title(" Latency (us) "));
    f.render_widget(latency_widget, chunks[0]);

    // Stream counters panel
    let names = ["vol_baseline", "ohlc_vol", "rapid_fire", "wash_score", "suspicious_match", "asof_match"];
    let stream_rows: Vec<Row> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let color = if app.stream_counts[i] > 0 { Color::Green } else { Color::Red };
            Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(
                    if app.stream_counts[i] > 0 { " OK " } else { "WAIT" },
                    Style::default().fg(color),
                )),
                ratatui::widgets::Cell::from(format!("{:<20}", name)),
                ratatui::widgets::Cell::from(format!("{}", app.stream_counts[i])),
            ])
        })
        .collect();

    let stream_table = Table::new(
        stream_rows,
        [Constraint::Length(5), Constraint::Length(21), Constraint::Min(8)],
    )
    .block(Block::default().borders(Borders::ALL).title(" Detection Streams "));
    f.render_widget(stream_table, chunks[1]);
}

fn draw_counts_and_prices(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Alert counts by type
    let counts = app.alert_engine.alert_counts();
    let type_names = ["VolumeAnomaly", "PriceSpike", "RapidFire", "WashTrading", "SuspiciousMatch", "FrontRunning"];
    let count_rows: Vec<Row> = type_names
        .iter()
        .map(|name| {
            let c = counts.get(*name).copied().unwrap_or(0);
            let color = if c > 0 { Color::Yellow } else { Color::DarkGray };
            Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(format!("{:<18}", name), Style::default().fg(color))),
                ratatui::widgets::Cell::from(Span::styled(format!("{}", c), Style::default().fg(color))),
            ])
        })
        .collect();

    let count_table = Table::new(
        count_rows,
        [Constraint::Length(19), Constraint::Min(6)],
    )
    .block(Block::default().borders(Borders::ALL).title(" Alert Counts "));
    f.render_widget(count_table, chunks[0]);

    // Symbol prices
    let mut symbols: Vec<_> = app.prices.iter().collect();
    symbols.sort_by_key(|(s, _)| (*s).clone());
    let price_rows: Vec<Row> = symbols
        .iter()
        .map(|(sym, price)| {
            Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(format!("{:<6}", sym), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
                ratatui::widgets::Cell::from(format!("{:.2}", price)),
            ])
        })
        .collect();

    let price_table = Table::new(
        price_rows,
        [Constraint::Length(7), Constraint::Min(12)],
    )
    .block(Block::default().borders(Borders::ALL).title(" Symbol Prices "));
    f.render_widget(price_table, chunks[1]);
}
