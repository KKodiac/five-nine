use std::collections::HashMap;
use std::sync::Arc;
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
    widgets::{Block, Borders, Paragraph},
};
use serde::Deserialize;

// ── Constants ─────────────────────────────────────────────────────────────────

const CLAUDE_SUMMARY_URL: &str = "https://status.claude.com/api/v2/summary.json";
const CLAUDE_INCIDENTS_URL: &str = "https://status.claude.com/api/v2/incidents.json";
const OPENAI_SUMMARY_URL: &str = "https://status.openai.com/api/v2/summary.json";
const OPENAI_INCIDENTS_URL: &str = "https://status.openai.com/api/v2/incidents.json";
const APPLE_STATUS_URL: &str =
    "https://www.apple.com/support/systemstatus/data/developer/system_status_en_US.js";

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const UPTIME_WINDOW_DAYS: f64 = 90.0;

// Apple developer services to always show (others shown only if degraded)
const APPLE_PINNED: &[&str] = &[
    "App Store Connect",
    "App Store Connect - TestFlight",
    "APNS",
    "CloudKit Database",
    "Certificates, Identifiers & Profiles",
    "Xcode Cloud",
    "Xcode Automatic Configuration",
    "Developer ID Notary Service",
];

// ── Statuspage API types ──────────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
struct SummaryResponse {
    status: StatusIndicator,
    components: Vec<SpComponent>,
}

#[derive(Deserialize, Clone)]
struct StatusIndicator {
    indicator: String,
    description: String,
}

#[derive(Deserialize, Clone)]
struct SpComponent {
    name: String,
    status: String,
    #[serde(default)]
    group: bool,
    #[serde(default)]
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

// ── Apple API types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AppleResponse {
    services: Vec<AppleService>,
}

#[derive(Deserialize, Clone)]
struct AppleService {
    #[serde(rename = "serviceName")]
    service_name: String,
    events: Vec<AppleEvent>,
}

#[derive(Deserialize, Clone)]
struct AppleEvent {
    #[serde(rename = "eventStatus")]
    event_status: String,
    #[serde(rename = "statusType")]
    status_type: String,
    #[serde(rename = "epochStartDate")]
    epoch_start_ms: i64,
    #[serde(rename = "epochEndDate")]
    epoch_end_ms: Option<i64>,
}

// ── Normalized types ──────────────────────────────────────────────────────────

struct ServiceRow {
    name: String,
    status: String,
    uptime_pct: f64,
}

struct ProviderContent {
    indicator: String,
    description: String,
    services: Vec<ServiceRow>,
}

struct AllProviders {
    claude: Result<ProviderContent, String>,
    openai: Result<ProviderContent, String>,
    apple: Result<ProviderContent, String>,
}

// ── App state ─────────────────────────────────────────────────────────────────

enum FetchState {
    Loading,
    Ok(Arc<AllProviders>),
}

struct App {
    fetch_state: FetchState,
    last_fetched: Option<Instant>,
    last_checked_at: Option<String>,
    tick: u64,
    fetching: bool,
    scroll: u16,
    board_line_count: u16,
}

impl App {
    fn new() -> Self {
        Self {
            fetch_state: FetchState::Loading,
            last_fetched: None,
            last_checked_at: None,
            tick: 0,
            fetching: false,
            scroll: 0,
            board_line_count: 0,
        }
    }

    fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn clamp_scroll(&mut self, board_height: u16) {
        let max = self.board_line_count.saturating_sub(board_height);
        self.scroll = self.scroll.min(max);
    }

    fn should_refresh(&self) -> bool {
        if self.fetching { return false; }
        match self.last_fetched {
            None => true,
            Some(t) => t.elapsed() >= REFRESH_INTERVAL,
        }
    }

    fn apply(&mut self, providers: Arc<AllProviders>) {
        self.fetch_state = FetchState::Ok(providers);
        self.last_fetched = Some(Instant::now());
        self.last_checked_at = Some(Local::now().format("%H:%M:%S").to_string());
        self.fetching = false;
    }
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

fn compute_uptime_from_incidents(incidents: &[Incident]) -> HashMap<String, f64> {
    let now = Utc::now();
    let window_start = now - chrono::Duration::days(UPTIME_WINDOW_DAYS as i64);
    let window_secs = UPTIME_WINDOW_DAYS * 86400.0;
    let mut downtime: HashMap<String, f64> = HashMap::new();

    for inc in incidents {
        let Ok(start_dt) = DateTime::parse_from_rfc3339(&inc.created_at) else { continue };
        let start = start_dt.with_timezone(&Utc);
        let end = inc
            .resolved_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let eff_start = start.max(window_start);
        let eff_end = end.min(now);
        if eff_end <= eff_start { continue; }
        let dur = (eff_end - eff_start).num_seconds() as f64;
        for comp in &inc.components {
            *downtime.entry(comp.name.clone()).or_insert(0.0) += dur;
        }
    }

    downtime
        .into_iter()
        .map(|(name, down)| (name, (1.0 - (down / window_secs)).clamp(0.0, 1.0) * 100.0))
        .collect()
}

fn normalize_statuspage(summary: SummaryResponse, uptime: HashMap<String, f64>) -> ProviderContent {
    let services = summary
        .components
        .iter()
        .filter(|c| !c.group && (!c.only_show_if_degraded || c.status != "operational"))
        .map(|c| ServiceRow {
            name: c.name.clone(),
            status: c.status.clone(),
            uptime_pct: *uptime.get(&c.name).unwrap_or(&100.0),
        })
        .collect();

    ProviderContent {
        indicator: summary.status.indicator.clone(),
        description: summary.status.description.clone(),
        services,
    }
}

fn apple_service_status(events: &[AppleEvent]) -> &'static str {
    let now_ms = Utc::now().timestamp_millis();
    let active = events.iter().any(|e| {
        e.event_status != "resolved"
            && e.epoch_start_ms <= now_ms
            && e.epoch_end_ms.unwrap_or(i64::MAX) >= now_ms
    });
    if !active {
        "operational"
    } else {
        let is_outage = events.iter().any(|e| {
            e.event_status != "resolved" && e.status_type.to_lowercase().contains("outage")
        });
        if is_outage { "major_outage" } else { "degraded_performance" }
    }
}

fn apple_uptime(events: &[AppleEvent]) -> f64 {
    let now_ms = Utc::now().timestamp_millis();
    let window_start_ms = now_ms - (UPTIME_WINDOW_DAYS as i64 * 86_400_000);
    let window_ms = UPTIME_WINDOW_DAYS * 86_400_000.0;
    let downtime_ms: i64 = events
        .iter()
        .map(|e| {
            let start = e.epoch_start_ms.max(window_start_ms);
            let end = e.epoch_end_ms.unwrap_or(now_ms).min(now_ms);
            (end - start).max(0)
        })
        .sum();
    (1.0 - downtime_ms as f64 / window_ms).clamp(0.0, 1.0) * 100.0
}

fn normalize_apple(services: Vec<AppleService>) -> ProviderContent {
    let pinned: std::collections::HashSet<&str> = APPLE_PINNED.iter().copied().collect();

    let visible: Vec<&AppleService> = services
        .iter()
        .filter(|s| {
            pinned.contains(s.service_name.as_str())
                || !s.events.is_empty()
        })
        .collect();

    let any_degraded = visible.iter().any(|s| apple_service_status(&s.events) != "operational");
    let indicator = if any_degraded { "minor" } else { "none" }.to_string();
    let description = if any_degraded {
        "Some Services Experiencing Issues"
    } else {
        "All Developer Services Operational"
    }
    .to_string();

    let service_rows = visible
        .iter()
        .map(|s| ServiceRow {
            name: s.service_name.clone(),
            status: apple_service_status(&s.events).to_string(),
            uptime_pct: apple_uptime(&s.events),
        })
        .collect();

    ProviderContent { indicator, description, services: service_rows }
}

async fn fetch_statuspage(
    summary_url: &str,
    incidents_url: &str,
) -> Result<ProviderContent, String> {
    let (sum_res, inc_res) = tokio::join!(
        reqwest::get(summary_url),
        reqwest::get(incidents_url),
    );

    let summary = sum_res
        .map_err(|e| format!("Network error: {e}"))?
        .json::<SummaryResponse>()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    let uptime = match inc_res {
        Ok(r) => match r.json::<IncidentsResponse>().await {
            Ok(d) => compute_uptime_from_incidents(&d.incidents),
            Err(_) => HashMap::new(),
        },
        Err(_) => HashMap::new(),
    };

    Ok(normalize_statuspage(summary, uptime))
}

async fn fetch_apple() -> Result<ProviderContent, String> {
    let body = reqwest::get(APPLE_STATUS_URL)
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Read error: {e}"))?;

    // Strip JSONP wrapper: jsonCallback({...});
    let json = body
        .trim()
        .strip_prefix("jsonCallback(")
        .and_then(|s| s.strip_suffix(");").or_else(|| s.strip_suffix(')')))
        .unwrap_or(&body);

    let parsed: AppleResponse =
        serde_json::from_str(json).map_err(|e| format!("Parse error: {e}"))?;

    Ok(normalize_apple(parsed.services))
}

async fn fetch_all() -> FetchState {
    let (claude, openai, apple) = tokio::join!(
        fetch_statuspage(CLAUDE_SUMMARY_URL, CLAUDE_INCIDENTS_URL),
        fetch_statuspage(OPENAI_SUMMARY_URL, OPENAI_INCIDENTS_URL),
        fetch_apple(),
    );

    FetchState::Ok(Arc::new(AllProviders { claude, openai, apple }))
}

// ── Rendering helpers ─────────────────────────────────────────────────────────

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

fn status_label(status: &str) -> &'static str {
    match status {
        "operational"          => "OPERATIONAL",
        "degraded_performance" => "DEGRADED",
        "partial_outage"       => "PARTIAL OUTAGE",
        "major_outage"         => "MAJOR OUTAGE",
        "under_maintenance"    => "MAINTENANCE",
        _                      => "UNKNOWN",
    }
}

fn uptime_color(pct: f64) -> Color {
    if pct >= 99.9 { Color::Green } else if pct >= 99.0 { Color::Yellow } else { Color::LightRed }
}

/// Airport animation: departing planes cross the sky; an arriving plane rolls
/// down the runway toward the ATC tower. Plane color tracks service status.
fn airport_animation(tick: u64, width: usize, indicator: &str) -> Vec<Line<'static>> {
    let status_color = indicator_color(indicator);
    let w = width;

    // ── Sky row: sparse stars + two departing planes (left → right) ──────────
    let mut sky: Vec<Span<'static>> = (0..w)
        .map(|i| {
            if (i * 7 + 3) % 23 == 0 || (i * 11 + 17) % 41 == 0 {
                Span::styled("·", Style::default().fg(Color::Rgb(45, 45, 70)))
            } else {
                Span::raw(" ")
            }
        })
        .collect();

    let p1 = ((tick / 4) % (w as u64 + 8)) as usize;
    let p2 = ((tick / 7 + w as u64 * 2 / 3) % (w as u64 + 14)) as usize;
    for p in [p1, p2] {
        if p < w {
            sky[p] = Span::styled("✈", Style::default().fg(status_color));
        }
    }

    // ── Ground row: runway markings + ATC tower + arriving plane (right → left)
    let tower = (w * 65 / 100).min(w.saturating_sub(4));
    let mut ground: Vec<Span<'static>> = (0..w)
        .map(|i| {
            if i == tower {
                Span::styled("▐", Style::default().fg(Color::Rgb(200, 170, 80)))
            } else if i == tower + 1 {
                Span::styled("█", Style::default().fg(Color::Rgb(230, 200, 100)))
            } else if i == tower + 2 {
                Span::styled("▌", Style::default().fg(Color::Rgb(200, 170, 80)))
            } else if i % 6 < 4 {
                Span::styled("─", Style::default().fg(Color::Rgb(55, 55, 55)))
            } else {
                Span::raw(" ")
            }
        })
        .collect();

    // Plane enters from right, rolls left, disappears off-screen
    let phase = (tick / 3) % (w as u64 + 10);
    if phase < w as u64 {
        let ax = (w - 1).saturating_sub(phase as usize);
        ground[ax] = Span::styled("✈", Style::default().fg(status_color));
    }

    vec![Line::from(sky), Line::from(ground)]
}

fn provider_lines(content: &ProviderContent, inner_width: usize) -> Vec<Line<'static>> {
    if content.services.is_empty() {
        return vec![Line::from(Span::styled(
            "  No components reported.",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    content
        .services
        .iter()
        .map(|s| {
            let color = indicator_color(&s.status);
            let symbol = indicator_symbol(&s.status);
            let label = status_label(&s.status);
            let pct = format!("{:.2}%", s.uptime_pct);

            let left_len = 2 + 1 + 2 + s.name.len();
            let right_len = label.len() + 2 + pct.len();
            let gap = inner_width.saturating_sub(left_len + right_len);

            Line::from(vec![
                Span::raw("  "),
                Span::styled(symbol, Style::default().fg(color)),
                Span::raw("  "),
                Span::styled(s.name.clone(), Style::default().fg(Color::White)),
                Span::raw(" ".repeat(gap)),
                Span::styled(label, Style::default().fg(color)),
                Span::raw("  "),
                Span::styled(pct, Style::default().fg(uptime_color(s.uptime_pct))),
            ])
        })
        .collect()
}

fn provider_header_line(name: &str, source: &str, width: usize) -> Line<'static> {
    let prefix = format!("  {} ", name);
    let suffix = format!(" {source}");
    let dashes = width.saturating_sub(prefix.len() + suffix.len());
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("─".repeat(dashes), Style::default().fg(Color::DarkGray)),
        Span::styled(suffix, Style::default().fg(Color::DarkGray)),
    ])
}

// ── Draw ──────────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(5), // title + sky + runway + status line + bottom border
            Constraint::Min(1),    // services board
            Constraint::Length(1), // footer
        ])
        .split(area);

    // ── Determine wave / overall status ───────────────────────────────────────
    let (wave_indicator, overall_indicator, overall_desc) = match &app.fetch_state {
        FetchState::Ok(all) => {
            let claude_code_indicator = all
                .claude
                .as_ref()
                .ok()
                .and_then(|c| {
                    c.services
                        .iter()
                        .find(|s| s.name.to_lowercase().contains("claude code"))
                        .map(|s| s.status.clone())
                })
                .unwrap_or_else(|| {
                    all.claude
                        .as_ref()
                        .map(|c| c.indicator.clone())
                        .unwrap_or_else(|_| "critical".into())
                });
            let (ind, desc) = all
                .claude
                .as_ref()
                .map(|c| (c.indicator.clone(), c.description.clone()))
                .unwrap_or_else(|e| ("critical".into(), e.clone()));
            (claude_code_indicator, ind, desc)
        }
        FetchState::Loading => ("none".into(), "none".into(), "Fetching…".into()),
    };

    let overall_color = indicator_color(&overall_indicator);
    let overall_symbol = indicator_symbol(&overall_indicator);

    // ── Header: title + airport animation (sky + runway) + status ────────────
    let scene_width = chunks[0].width as usize;
    let mut header_lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(
            "five-nine",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  ·  AI Service Status",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    header_lines.extend(airport_animation(app.tick, scene_width, &wave_indicator));
    header_lines.push(Line::from(vec![
        Span::styled(
            overall_symbol,
            Style::default().fg(overall_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            overall_desc.to_uppercase(),
            Style::default().fg(overall_color).add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(
        Paragraph::new(header_lines).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    // ── Services board ─────────────────────────────────────────────────────────
    let board_width = chunks[1].width as usize;
    let mut board_lines: Vec<Line<'static>> = Vec::new();

    let provider_defs: [(&str, &str); 3] = [
        ("CLAUDE", "status.claude.com"),
        ("OPENAI", "status.openai.com"),
        ("APPLE DEVELOPER", "developer.apple.com/system-status"),
    ];

    match &app.fetch_state {
        FetchState::Ok(all) => {
            let results: [&Result<ProviderContent, String>; 3] =
                [&all.claude, &all.openai, &all.apple];

            for (idx, ((name, source), result)) in
                provider_defs.iter().zip(results.iter()).enumerate()
            {
                if idx > 0 {
                    board_lines.push(Line::from(""));
                }
                board_lines.push(provider_header_line(name, source, board_width));

                match result {
                    Ok(content) => {
                        if content.services.is_empty() {
                            board_lines.push(Line::from(Span::styled(
                                "  No components reported.",
                                Style::default().fg(Color::DarkGray),
                            )));
                        } else {
                            board_lines.extend(provider_lines(content, board_width));
                        }
                    }
                    Err(e) => {
                        board_lines.push(Line::from(Span::styled(
                            format!("  ✗  {e}"),
                            Style::default().fg(Color::Red),
                        )));
                    }
                }
            }
        }
        FetchState::Loading => {
            board_lines.push(Line::from(Span::styled(
                "  Loading…",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    app.board_line_count = board_lines.len() as u16;
    app.clamp_scroll(chunks[1].height);
    f.render_widget(
        Paragraph::new(board_lines).scroll((app.scroll, 0)),
        chunks[1],
    );

    // ── Footer ────────────────────────────────────────────────────────────────
    let checked_str = app
        .last_checked_at
        .as_deref()
        .map(|t| format!("last checked {t}  ·  "))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(checked_str, Style::default().fg(Color::DarkGray)),
            Span::styled(
                "r",
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" refresh  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "↑↓",
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" scroll  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "q",
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Center),
        chunks[2],
    );
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Arc<AllProviders>>(1);

    loop {
        // Receive completed fetch without blocking
        if let Ok(providers) = rx.try_recv() {
            app.apply(providers);
        }

        // Kick off a background fetch if due
        if app.should_refresh() {
            app.fetching = true;
            let tx = tx.clone();
            tokio::spawn(async move {
                if let FetchState::Ok(p) = fetch_all().await {
                    let _ = tx.send(p).await;
                }
            });
        }

        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') => {
                        app.fetch_state = FetchState::Loading;
                        app.last_fetched = None;
                        app.fetching = false;
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.scroll_down(1),
                    KeyCode::Up | KeyCode::Char('k') => app.scroll_up(1),
                    KeyCode::PageDown | KeyCode::Char('d') => app.scroll_down(10),
                    KeyCode::PageUp | KeyCode::Char('u') => app.scroll_up(10),
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}
