use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde::Deserialize;

const SUMMARY_API_URL: &str = "https://status.claude.com/api/v2/summary.json";
const INCIDENTS_API_URL: &str = "https://status.claude.com/api/v2/incidents.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const UPTIME_WINDOW_DAYS: f64 = 90.0;

// ── API types ────────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
struct SummaryResponse {
    status: StatusIndicator,
    page: Page,
    components: Vec<Component>,
}

#[derive(Deserialize, Clone)]
struct StatusIndicator {
    indicator: String,
    description: String,
}

#[derive(Deserialize, Clone)]
struct Page {
    updated_at: String,
}

#[derive(Deserialize, Clone)]
struct Component {
    name: String,
    status: String,
    group: bool,
    only_show_if_degraded: bool,
}

#[derive(Deserialize, Clone)]
struct IncidentComponent {
    name: String,
}

#[derive(Deserialize, Clone)]
struct Incident {
    created_at: String,
    resolved_at: Option<String>,
    components: Vec<IncidentComponent>,
}

#[derive(Deserialize)]
struct IncidentsResponse {
    incidents: Vec<Incident>,
}

// ── App state ────────────────────────────────────────────────────────────────

struct StatusData {
    summary: SummaryResponse,
    /// component name → 90-day uptime percentage (0.0–100.0)
    uptime: HashMap<String, f64>,
}

#[derive(Clone)]
enum FetchState {
    Loading,
    Ok(std::sync::Arc<StatusData>),
    Error(String),
}

struct App {
    fetch_state: FetchState,
    last_fetched: Option<Instant>,
    last_checked_at: Option<String>,
    tick: u64,
}

impl App {
    fn new() -> Self {
        Self {
            fetch_state: FetchState::Loading,
            last_fetched: None,
            last_checked_at: None,
            tick: 0,
        }
    }

    fn should_refresh(&self) -> bool {
        match self.last_fetched {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_INTERVAL,
        }
    }

    fn update(&mut self, state: FetchState) {
        self.fetch_state = state;
        self.last_fetched = Some(Instant::now());
        self.last_checked_at = Some(Local::now().format("%H:%M:%S").to_string());
    }
}

// ── Fetch ────────────────────────────────────────────────────────────────────

fn compute_uptime(incidents: &[Incident]) -> HashMap<String, f64> {
    let now = Utc::now();
    let window_start = now - chrono::Duration::days(UPTIME_WINDOW_DAYS as i64);
    let window_secs = UPTIME_WINDOW_DAYS * 86400.0;

    let mut downtime_secs: HashMap<String, f64> = HashMap::new();

    for incident in incidents {
        let Ok(start_dt) = DateTime::parse_from_rfc3339(&incident.created_at) else {
            continue;
        };
        let start = start_dt.with_timezone(&Utc);
        if start > now {
            continue;
        }

        let end = incident
            .resolved_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);

        // Clamp to the 90-day window
        let effective_start = start.max(window_start);
        let effective_end = end.min(now);
        if effective_end <= effective_start {
            continue;
        }

        let duration = (effective_end - effective_start).num_seconds() as f64;

        for comp in &incident.components {
            *downtime_secs.entry(comp.name.clone()).or_insert(0.0) += duration;
        }
    }

    downtime_secs
        .into_iter()
        .map(|(name, down)| {
            let pct = (1.0 - (down / window_secs)).clamp(0.0, 1.0) * 100.0;
            (name, pct)
        })
        .collect()
}

async fn fetch_status() -> FetchState {
    let (summary_res, incidents_res) = tokio::join!(
        reqwest::get(SUMMARY_API_URL),
        reqwest::get(INCIDENTS_API_URL),
    );

    let summary = match summary_res {
        Ok(r) => match r.json::<SummaryResponse>().await {
            Ok(d) => d,
            Err(e) => return FetchState::Error(format!("Parse error: {e}")),
        },
        Err(e) => return FetchState::Error(format!("Network error: {e}")),
    };

    let uptime = match incidents_res {
        Ok(r) => match r.json::<IncidentsResponse>().await {
            Ok(d) => compute_uptime(&d.incidents),
            Err(_) => HashMap::new(),
        },
        Err(_) => HashMap::new(),
    };

    FetchState::Ok(std::sync::Arc::new(StatusData { summary, uptime }))
}

// ── Rendering helpers ────────────────────────────────────────────────────────

fn indicator_color(indicator: &str) -> Color {
    match indicator {
        "none" | "operational" => Color::Green,
        "minor" | "degraded_performance" => Color::Yellow,
        "major" | "partial_outage" => Color::LightRed,
        "critical" | "major_outage" => Color::Red,
        "under_maintenance" => Color::Cyan,
        _ => Color::Gray,
    }
}

fn indicator_symbol(indicator: &str) -> &'static str {
    match indicator {
        "none" | "operational" => "●",
        "minor" | "degraded_performance" => "◐",
        "major" | "partial_outage" => "◑",
        "critical" | "major_outage" => "○",
        "under_maintenance" => "◎",
        _ => "?",
    }
}

fn component_status_label(status: &str) -> &'static str {
    match status {
        "operational" => "Operational",
        "degraded_performance" => "Degraded",
        "partial_outage" => "Partial Outage",
        "major_outage" => "Major Outage",
        "under_maintenance" => "Maintenance",
        _ => "Unknown",
    }
}

fn wave_line(tick: u64, width: usize, color: Color) -> Line<'static> {
    const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    // PQRST cardiac waveform: baseline=0.5, P bump, Q dip, R spike, S dip, T bump
    #[rustfmt::skip]
    const ECG: &[f64] = &[
        // TP — flat baseline
        0.50, 0.50, 0.50, 0.50, 0.50, 0.50, 0.50,
        // P wave — small rounded bump
        0.53, 0.58, 0.62, 0.60, 0.55, 0.50,
        // PQ segment — flat
        0.50, 0.50,
        // Q — small dip
        0.42, 0.30,
        // R — tall sharp spike
        0.55, 0.80, 0.95, 1.00,
        // S — sharp undershoot
        0.60, 0.15, 0.00,
        // return to baseline
        0.25, 0.45, 0.50,
        // T wave — rounded bump
        0.53, 0.58, 0.64, 0.68, 0.70, 0.67, 0.62, 0.56, 0.50,
        // TP — flat tail
        0.50, 0.50, 0.50, 0.50, 0.50,
    ];
    let n = ECG.len();
    // scroll 1 column every 2 ticks — comfortable reading speed
    let offset = (tick / 2) as usize;
    let chars: String = (0..width)
        .map(|i| {
            let val = ECG[(i + offset) % n];
            BLOCKS[(val * 8.0).round() as usize]
        })
        .collect();
    Line::from(Span::styled(chars, Style::default().fg(color)))
}

fn uptime_color(pct: f64) -> Color {
    if pct >= 99.9 {
        Color::Green
    } else if pct >= 99.0 {
        Color::Yellow
    } else {
        Color::LightRed
    }
}

// ── Draw ─────────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    let component_count = match &app.fetch_state {
        FetchState::Ok(data) => data
            .summary
            .components
            .iter()
            .filter(|c| !c.group && (!c.only_show_if_degraded || c.status != "operational"))
            .count(),
        _ => 0,
    };
    let components_height = (2 + component_count).max(3) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(components_height),
            Constraint::Min(0),
        ])
        .split(area);

    // Title
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "five-nine",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  Claude Status"),
    ]))
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(title, chunks[0]);

    // Wave color driven by Claude Code component status
    let inner_width = chunks[1].width.saturating_sub(2) as usize;
    let claude_code_color = match &app.fetch_state {
        FetchState::Ok(data) => data
            .summary
            .components
            .iter()
            .find(|c| !c.group && c.name.to_lowercase().contains("claude code"))
            .map(|c| indicator_color(&c.status))
            .unwrap_or_else(|| indicator_color(&data.summary.status.indicator)),
        FetchState::Error(_) => Color::Red,
        FetchState::Loading => Color::DarkGray,
    };

    // Overall status block
    let overall_content: Vec<Line> = match &app.fetch_state {
        FetchState::Loading => vec![
            wave_line(app.tick, inner_width, claude_code_color),
            Line::from(""),
            Line::from(Span::styled("  Fetching status…", Style::default().fg(Color::DarkGray))),
        ],
        FetchState::Error(e) => vec![
            wave_line(app.tick, inner_width, claude_code_color),
            Line::from(""),
            Line::from(vec![
                Span::styled("  ✗  ", Style::default().fg(Color::Red)),
                Span::styled(e.clone(), Style::default().fg(Color::Red)),
            ]),
        ],
        FetchState::Ok(data) => {
            let color = indicator_color(&data.summary.status.indicator);
            let symbol = indicator_symbol(&data.summary.status.indicator);
            vec![
                wave_line(app.tick, inner_width, claude_code_color),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(symbol, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(
                        data.summary.status.description.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        format!("status.claude.com  ·  updated {}", data.summary.page.updated_at),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
            ]
        }
    };

    f.render_widget(
        Paragraph::new(overall_content)
            .block(Block::default().borders(Borders::ALL).title(" Overall "))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );

    // Services block
    let services_content: Vec<Line> = match &app.fetch_state {
        FetchState::Loading | FetchState::Error(_) => vec![Line::from("")],
        FetchState::Ok(data) => {
            let visible: Vec<&Component> = data
                .summary
                .components
                .iter()
                .filter(|c| !c.group && (!c.only_show_if_degraded || c.status != "operational"))
                .collect();

            if visible.is_empty() {
                vec![Line::from(Span::styled(
                    "  No components reported.",
                    Style::default().fg(Color::DarkGray),
                ))]
            } else {
                let max_name = visible.iter().map(|c| c.name.len()).max().unwrap_or(0);
                const MAX_LABEL: usize = 13; // "Partial Outage" is longest

                visible
                    .iter()
                    .map(|c| {
                        let color = indicator_color(&c.status);
                        let symbol = indicator_symbol(&c.status);
                        let label = component_status_label(&c.status);
                        let name_pad = max_name - c.name.len();
                        let label_pad = MAX_LABEL - label.len();

                        let uptime_span = match data.uptime.get(&c.name) {
                            Some(&pct) => Span::styled(
                                format!("{pct:.2}%"),
                                Style::default().fg(uptime_color(pct)),
                            ),
                            None => Span::styled(
                                "100.00%",
                                Style::default().fg(Color::Green),
                            ),
                        };

                        Line::from(vec![
                            Span::raw("  "),
                            Span::styled(symbol, Style::default().fg(color)),
                            Span::raw("  "),
                            Span::styled(c.name.clone(), Style::default().fg(Color::White)),
                            Span::raw(" ".repeat(name_pad + 2)),
                            Span::styled(label, Style::default().fg(color)),
                            Span::raw(" ".repeat(label_pad + 2)),
                            uptime_span,
                        ])
                    })
                    .collect()
            }
        }
    };

    f.render_widget(
        Paragraph::new(services_content)
            .block(Block::default().borders(Borders::ALL).title(" Services  90d uptime "))
            .wrap(Wrap { trim: false }),
        chunks[2],
    );

    // Footer
    let checked_str = app
        .last_checked_at
        .as_deref()
        .map(|t| format!("last checked {t}  ·  "))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(checked_str, Style::default().fg(Color::DarkGray)),
            Span::styled("r", Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)),
            Span::styled(" refresh  ", Style::default().fg(Color::DarkGray)),
            Span::styled("q", Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)),
            Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Center),
        chunks[3],
    );
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        if app.should_refresh() {
            let state = fetch_status().await;
            app.update(state);
        }

        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| draw(f, &app))?;

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') => {
                        app.fetch_state = FetchState::Loading;
                        app.last_fetched = None;
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
