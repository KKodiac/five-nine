use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
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
use serde::{Deserialize, Serialize};

// ── Constants ─────────────────────────────────────────────────────────────────

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const UPTIME_WINDOW_DAYS: f64 = 90.0;

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
    providers: Vec<(CustomProvider, Result<ProviderContent, String>)>,
}

// ── Custom provider config ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct CustomProvider {
    name: String,   // display label e.g. "GITHUB"
    source: String, // human-readable domain e.g. "www.githubstatus.com"
    summary_url: String,
    incidents_url: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    providers: Vec<CustomProvider>,
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("five-nine")
        .join("providers.json")
}

fn load_config_from(path: &PathBuf) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config_to(config: &Config, path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

fn default_providers() -> Vec<CustomProvider> {
    vec![
        CustomProvider {
            name: "CLAUDE".to_string(),
            source: "status.claude.com".to_string(),
            summary_url: "https://status.claude.com/api/v2/summary.json".to_string(),
            incidents_url: "https://status.claude.com/api/v2/incidents.json".to_string(),
        },
        CustomProvider {
            name: "OPENAI".to_string(),
            source: "status.openai.com".to_string(),
            summary_url: "https://status.openai.com/api/v2/summary.json".to_string(),
            incidents_url: "https://status.openai.com/api/v2/incidents.json".to_string(),
        },
    ]
}

fn load_config() -> Config {
    let path = config_path();
    if path.exists() {
        return load_config_from(&path);
    }
    // First run: seed defaults and persist so users can edit freely
    let config = Config {
        providers: default_providers(),
    };
    let _ = save_config_to(&config, &path);
    config
}

fn save_config(config: &Config) -> Result<(), String> {
    save_config_to(config, &config_path())
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
        if self.fetching {
            return false;
        }
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
        let Ok(start_dt) = DateTime::parse_from_rfc3339(&inc.created_at) else {
            continue;
        };
        let start = start_dt.with_timezone(&Utc);
        let end = inc
            .resolved_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let eff_start = start.max(window_start);
        let eff_end = end.min(now);
        if eff_end <= eff_start {
            continue;
        }
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

async fn fetch_statuspage(
    summary_url: &str,
    incidents_url: &str,
) -> Result<ProviderContent, String> {
    let (sum_res, inc_res) = tokio::join!(reqwest::get(summary_url), reqwest::get(incidents_url),);

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

async fn fetch_all(providers: &[CustomProvider]) -> FetchState {
    let mut set = tokio::task::JoinSet::new();
    for (i, p) in providers.iter().cloned().enumerate() {
        set.spawn(async move {
            let result = fetch_statuspage(&p.summary_url, &p.incidents_url).await;
            (i, p, result)
        });
    }
    let mut unsorted: Vec<(usize, CustomProvider, Result<ProviderContent, String>)> = Vec::new();
    while let Some(Ok(entry)) = set.join_next().await {
        unsorted.push(entry);
    }
    unsorted.sort_by_key(|(i, _, _)| *i);
    let providers = unsorted.into_iter().map(|(_, p, r)| (p, r)).collect();
    FetchState::Ok(Arc::new(AllProviders { providers }))
}

// ── Rendering helpers ─────────────────────────────────────────────────────────

fn indicator_rank(s: &str) -> u8 {
    match s {
        "critical" | "major_outage" => 4,
        "major" | "partial_outage" => 3,
        "minor" | "degraded_performance" => 2,
        "under_maintenance" => 1,
        _ => 0,
    }
}

/// Returns the (indicator, description) of the worst-off provider.
fn worst_status(
    providers: &[(CustomProvider, Result<ProviderContent, String>)],
) -> (String, String) {
    providers
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok())
        .max_by_key(|c| indicator_rank(&c.indicator))
        .map(|c| (c.indicator.clone(), c.description.clone()))
        .unwrap_or_else(|| ("none".to_string(), "All Systems Operational".to_string()))
}

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
        "operational" => "OPERATIONAL",
        "degraded_performance" => "DEGRADED",
        "partial_outage" => "PARTIAL OUTAGE",
        "major_outage" => "MAJOR OUTAGE",
        "under_maintenance" => "MAINTENANCE",
        _ => "UNKNOWN",
    }
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
        Span::styled(
            prefix,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
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

    // ── Determine overall status (worst across all providers) ─────────────────
    let (overall_indicator, overall_desc) = match &app.fetch_state {
        FetchState::Ok(all) => worst_status(&all.providers),
        FetchState::Loading => ("none".to_string(), "Fetching…".to_string()),
    };

    let overall_color = indicator_color(&overall_indicator);
    let overall_symbol = indicator_symbol(&overall_indicator);

    // ── Header: title + airport animation (sky + runway) + status ────────────
    let scene_width = chunks[0].width as usize;
    let mut header_lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(
            "five-nine",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  ·  AI Service Status",
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    header_lines.extend(airport_animation(app.tick, scene_width, &overall_indicator));
    header_lines.push(Line::from(vec![
        Span::styled(
            overall_symbol,
            Style::default()
                .fg(overall_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            overall_desc.to_uppercase(),
            Style::default()
                .fg(overall_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(
        Paragraph::new(header_lines).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    // ── Services board ─────────────────────────────────────────────────────────
    let board_width = chunks[1].width as usize;
    let mut board_lines: Vec<Line<'static>> = Vec::new();

    match &app.fetch_state {
        FetchState::Ok(all) => {
            if all.providers.is_empty() {
                board_lines.push(Line::from(Span::styled(
                    "  No providers configured. Run: five-nine add <name>",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for (idx, (p, result)) in all.providers.iter().enumerate() {
                if idx > 0 {
                    board_lines.push(Line::from(""));
                }
                board_lines.push(provider_header_line(&p.name, &p.source, board_width));
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
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" refresh  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "↑↓",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" scroll  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "q",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Center),
        chunks[2],
    );
}

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "five-nine",
    version,
    about = "AI service status monitor",
    long_about = "Terminal UI for monitoring Claude, OpenAI, and Apple Developer service status in real time."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the TUI monitor (default)
    Monitor,
    /// Print current service status and exit
    Status {
        /// Output raw JSON instead of formatted text
        #[arg(long)]
        json: bool,
    },
    /// Add a provider to monitor (auto-discovers Statuspage API)
    Add {
        /// Application name to search for (e.g. github, vercel, stripe)
        name: String,
        /// Override with a direct status-page base URL
        #[arg(long)]
        url: Option<String>,
    },
    /// Remove a custom provider
    Remove {
        /// Display name of the provider to remove (case-insensitive)
        name: String,
    },
    /// List all monitored providers
    List,
    /// Check for a newer release and self-update if one is available
    Update,
}

// ── Status command ────────────────────────────────────────────────────────────

fn print_provider_table(name: &str, source: &str, result: &Result<ProviderContent, String>) {
    match result {
        Ok(c) => {
            let sym = indicator_symbol(&c.indicator);
            let color_code = match c.indicator.as_str() {
                "none" | "operational" => "\x1b[32m",
                "minor" | "degraded_performance" => "\x1b[33m",
                _ => "\x1b[31m",
            };
            println!("{color_code}{sym}\x1b[0m  \x1b[1m{name}\x1b[0m  \x1b[2m{source}\x1b[0m");
            for s in &c.services {
                let sym = indicator_symbol(&s.status);
                let label = status_label(&s.status);
                let sc = match s.status.as_str() {
                    "operational" => "\x1b[32m",
                    "degraded_performance" => "\x1b[33m",
                    _ => "\x1b[31m",
                };
                let uc = if s.uptime_pct >= 99.9 {
                    "\x1b[32m"
                } else if s.uptime_pct >= 99.0 {
                    "\x1b[33m"
                } else {
                    "\x1b[31m"
                };
                println!(
                    "  {sc}{sym}\x1b[0m  {:<38} {sc}{:<15}\x1b[0m {uc}{:>7.2}%\x1b[0m",
                    s.name, label, s.uptime_pct
                );
            }
        }
        Err(e) => {
            println!("\x1b[1m{name}\x1b[0m  \x1b[2m{source}\x1b[0m");
            println!("  \x1b[31m✗\x1b[0m  {e}");
        }
    }
}

fn provider_to_json(result: &Result<ProviderContent, String>) -> serde_json::Value {
    match result {
        Ok(c) => serde_json::json!({
            "indicator": c.indicator,
            "description": c.description,
            "services": c.services.iter().map(|s| serde_json::json!({
                "name": s.name,
                "status": s.status,
                "uptime_pct": (s.uptime_pct * 100.0).round() / 100.0,
            })).collect::<Vec<_>>(),
        }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

async fn cmd_status(json: bool) -> Result<(), String> {
    if !json {
        eprintln!("Fetching…");
    }
    let config = load_config();
    let FetchState::Ok(all) = fetch_all(&config.providers).await else {
        unreachable!()
    };

    if json {
        let mut out = serde_json::Map::new();
        for (p, result) in &all.providers {
            out.insert(p.name.to_lowercase(), provider_to_json(result));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap()
        );
    } else {
        for (i, (p, result)) in all.providers.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_provider_table(&p.name, &p.source, result);
        }
    }
    Ok(())
}

// ── Provider management commands ──────────────────────────────────────────────

async fn cmd_add(name: String, url: Option<String>) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("five-nine/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let (summary_url, incidents_url, source) = if let Some(provided) = url {
        // Strip trailing slash and any existing /api/v2/summary.json suffix
        let base = provided
            .trim_end_matches('/')
            .trim_end_matches("/api/v2/summary.json");
        let summary = format!("{base}/api/v2/summary.json");
        let incidents = format!("{base}/api/v2/incidents.json");
        let source = base
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string();

        println!("Verifying {summary}…");
        client
            .get(&summary)
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))?
            .json::<serde_json::Value>()
            .await
            .map_err(|_| format!("'{summary}' does not look like a Statuspage v2 API"))?;

        (summary, incidents, source)
    } else {
        let slug = name.to_lowercase();
        println!("Searching for {name} status page…");

        let candidates = [
            format!("https://status.{slug}.com"),
            format!("https://{slug}status.com"),
            format!("https://{slug}.statuspage.io"),
            format!("https://status.{slug}.io"),
            format!("https://{slug}.status.io"),
        ];

        let mut found = None;
        for base in &candidates {
            let summary = format!("{base}/api/v2/summary.json");
            println!("  Trying {summary}…");
            let ok = if let Ok(resp) = client.get(&summary).send().await {
                resp.status().is_success() && resp.json::<serde_json::Value>().await.is_ok()
            } else {
                false
            };
            if ok {
                let source = base
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .to_string();
                found = Some((summary, format!("{base}/api/v2/incidents.json"), source));
                break;
            }
        }

        found.ok_or_else(|| {
            format!(
                "Could not auto-discover a Statuspage API for '{name}'.\n\
                 Try: five-nine add {name} --url <status-page-url>"
            )
        })?
    };

    let mut config = load_config();
    let display = name.to_uppercase();

    if config.providers.iter().any(|p| p.name == display) {
        return Err(format!("'{display}' is already in your provider list"));
    }

    config.providers.push(CustomProvider {
        name: display.clone(),
        source,
        summary_url,
        incidents_url,
    });
    save_config(&config)?;
    println!("Added {display}.");
    Ok(())
}

fn cmd_remove(name: String) -> Result<(), String> {
    let mut config = load_config();
    let display = name.to_uppercase();
    let before = config.providers.len();
    config.providers.retain(|p| p.name != display);
    if config.providers.len() == before {
        return Err(format!(
            "'{display}' not found. Run `five-nine list` to see custom providers."
        ));
    }
    save_config(&config)?;
    println!("Removed {display}.");
    Ok(())
}

fn cmd_list() {
    let config = load_config();
    if config.providers.is_empty() {
        println!("No providers configured. Add one with: five-nine add <name>");
    } else {
        println!("Monitored providers:");
        for p in &config.providers {
            println!("  {:<20} {}", p.name, p.source);
        }
    }
}

// ── Self-update ───────────────────────────────────────────────────────────────

async fn cmd_update() -> Result<(), String> {
    println!("Checking for updates…");

    let client = reqwest::Client::builder()
        .user_agent(concat!("five-nine/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    let release: serde_json::Value = client
        .get("https://api.github.com/repos/KKodiac/five-nine/releases/latest")
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    let latest = release["tag_name"]
        .as_str()
        .ok_or("Missing tag_name in API response")?
        .trim_start_matches('v');

    let current = env!("CARGO_PKG_VERSION");

    if latest == current {
        println!("Already up to date (v{current}).");
        return Ok(());
    }

    println!("Updating v{current} → v{latest}…");

    let arch = std::env::consts::ARCH; // "aarch64" or "x86_64"
    let asset_name = format!("five-nine-{arch}-apple-darwin");

    let assets = release["assets"].as_array().ok_or("No assets in release")?;
    let asset = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(asset_name.as_str()))
        .ok_or_else(|| format!("Asset '{asset_name}' not found in release"))?;

    let url = asset["browser_download_url"]
        .as_str()
        .ok_or("Missing download URL")?;

    println!("Downloading {asset_name}…");

    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download error: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("Read error: {e}"))?;

    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let tmp_path = current_exe.with_extension("update-tmp");

    std::fs::write(&tmp_path, &bytes).map_err(|e| format!("Write error: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod error: {e}"))?;
    }

    std::fs::rename(&tmp_path, &current_exe)
        .map_err(|e| format!("Replace error (try sudo?): {e}"))?;

    println!("Updated to v{latest}.");
    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Update) => {
            if let Err(e) = cmd_update().await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(Commands::Status { json }) => {
            if let Err(e) = cmd_status(json).await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(Commands::Add { name, url }) => {
            if let Err(e) = cmd_add(name, url).await {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(Commands::Remove { name }) => {
            if let Err(e) = cmd_remove(name) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(Commands::List) => {
            cmd_list();
            return Ok(());
        }
        Some(Commands::Monitor) | None => {}
    }

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
            let custom = load_config().providers;
            tokio::spawn(async move {
                if let FetchState::Ok(p) = fetch_all(&custom).await {
                    let _ = tx.send(p).await;
                }
            });
        }

        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(16))? {
            let evt = event::read()?;
            if let Event::Key(key) = evt {
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "five-nine-test-{}-{}.json",
            tag,
            std::process::id()
        ))
    }

    fn make_provider(name: &str) -> CustomProvider {
        CustomProvider {
            name: name.to_string(),
            source: format!("status.{}.com", name.to_lowercase()),
            summary_url: format!(
                "https://status.{}.com/api/v2/summary.json",
                name.to_lowercase()
            ),
            incidents_url: format!(
                "https://status.{}.com/api/v2/incidents.json",
                name.to_lowercase()
            ),
        }
    }

    // ── Config serialization ──────────────────────────────────────────────────

    #[test]
    fn config_round_trip() {
        let config = Config {
            providers: vec![make_provider("GITHUB")],
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.providers.len(), 1);
        assert_eq!(back.providers[0].name, "GITHUB");
        assert_eq!(back.providers[0].source, "status.github.com");
    }

    #[test]
    fn config_empty_json_gives_default() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.providers.is_empty());
    }

    #[test]
    fn config_missing_file_gives_default() {
        let path = tmp_path("missing");
        // Don't create the file — load_config_from should return default
        let config = load_config_from(&path);
        assert!(config.providers.is_empty());
    }

    // ── Config persistence ────────────────────────────────────────────────────

    #[test]
    fn save_and_load_round_trip() {
        let path = tmp_path("save-load");
        let config = Config {
            providers: vec![make_provider("STRIPE"), make_provider("VERCEL")],
        };
        save_config_to(&config, &path).unwrap();
        let loaded = load_config_from(&path);
        assert_eq!(loaded.providers.len(), 2);
        assert_eq!(loaded.providers[0].name, "STRIPE");
        assert_eq!(loaded.providers[1].name, "VERCEL");
        std::fs::remove_file(&path).ok();
    }

    // ── Duplicate detection ───────────────────────────────────────────────────

    #[test]
    fn duplicate_provider_detected() {
        let config = Config {
            providers: vec![make_provider("GITHUB")],
        };
        assert!(config.providers.iter().any(|p| p.name == "GITHUB"));
    }

    #[test]
    fn non_duplicate_not_detected() {
        let config = Config {
            providers: vec![make_provider("GITHUB")],
        };
        assert!(!config.providers.iter().any(|p| p.name == "STRIPE"));
    }

    // ── Remove logic ──────────────────────────────────────────────────────────

    #[test]
    fn remove_existing_provider() {
        let path = tmp_path("remove-existing");
        let config = Config {
            providers: vec![make_provider("GITHUB"), make_provider("STRIPE")],
        };
        save_config_to(&config, &path).unwrap();

        let mut loaded = load_config_from(&path);
        let before = loaded.providers.len();
        loaded.providers.retain(|p| p.name != "GITHUB");
        assert_eq!(loaded.providers.len(), before - 1);
        assert_eq!(loaded.providers[0].name, "STRIPE");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn remove_nonexistent_provider_is_noop() {
        let config = Config {
            providers: vec![make_provider("GITHUB")],
        };
        let mut providers = config.providers.clone();
        providers.retain(|p| p.name != "NONEXISTENT");
        assert_eq!(providers.len(), 1);
    }

    // ── Uptime calculation ────────────────────────────────────────────────────

    #[test]
    fn uptime_no_incidents_is_empty_map() {
        let result = compute_uptime_from_incidents(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn uptime_old_incident_outside_window_is_ignored() {
        // An incident that resolved 200 days ago is outside the 90-day window
        let start = chrono::Utc::now() - chrono::Duration::days(200);
        let end = start + chrono::Duration::hours(1);
        let incident = Incident {
            created_at: start.to_rfc3339(),
            resolved_at: Some(end.to_rfc3339()),
            components: vec![IncidentComponent {
                name: "API".to_string(),
            }],
        };
        let result = compute_uptime_from_incidents(&[incident]);
        // Either not in map (100% uptime) or very high
        let pct = result.get("API").copied().unwrap_or(100.0);
        assert!(pct > 99.99, "expected ~100% but got {pct}");
    }

    #[test]
    fn uptime_recent_long_incident_lowers_percentage() {
        // A 9-day outage inside the 90-day window lowers uptime by ~10%
        let start = chrono::Utc::now() - chrono::Duration::days(10);
        let end = start + chrono::Duration::days(9);
        let incident = Incident {
            created_at: start.to_rfc3339(),
            resolved_at: Some(end.to_rfc3339()),
            components: vec![IncidentComponent {
                name: "API".to_string(),
            }],
        };
        let result = compute_uptime_from_incidents(&[incident]);
        let pct = result.get("API").copied().unwrap_or(100.0);
        assert!(pct < 95.0, "expected <95% but got {pct}");
        assert!(pct > 80.0, "expected >80% but got {pct}");
    }

    // ── worst_status ──────────────────────────────────────────────────────────

    fn make_content(indicator: &str) -> ProviderContent {
        ProviderContent {
            indicator: indicator.to_string(),
            description: format!("{indicator} desc"),
            services: vec![],
        }
    }

    #[test]
    fn worst_status_empty_returns_all_ok() {
        let (ind, _) = worst_status(&[]);
        assert_eq!(ind, "none");
    }

    #[test]
    fn worst_status_picks_most_severe() {
        let providers = vec![
            (make_provider("A"), Ok(make_content("none"))),
            (make_provider("B"), Ok(make_content("minor"))),
            (make_provider("C"), Ok(make_content("critical"))),
        ];
        let (ind, _) = worst_status(&providers);
        assert_eq!(ind, "critical");
    }

    #[test]
    fn worst_status_ignores_errors() {
        let providers = vec![
            (make_provider("A"), Err("network error".to_string())),
            (make_provider("B"), Ok(make_content("none"))),
        ];
        let (ind, _) = worst_status(&providers);
        assert_eq!(ind, "none");
    }

    #[test]
    fn worst_status_all_errors_returns_default() {
        let providers = vec![(make_provider("A"), Err("err".to_string()))];
        let (ind, _) = worst_status(&providers);
        assert_eq!(ind, "none");
    }

    // ── Network integration tests (skipped by default) ────────────────────────

    #[tokio::test]
    #[ignore]
    async fn integration_fetch_claude_status() {
        let result = fetch_statuspage(
            "https://status.claude.com/api/v2/summary.json",
            "https://status.claude.com/api/v2/incidents.json",
        )
        .await;
        assert!(result.is_ok(), "fetch failed: {:?}", result.err());
        let c = result.unwrap();
        assert!(!c.indicator.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn integration_fetch_github_status() {
        let result = fetch_statuspage(
            "https://status.github.com/api/v2/summary.json",
            "https://status.github.com/api/v2/incidents.json",
        )
        .await;
        assert!(result.is_ok(), "fetch failed: {:?}", result.err());
    }

    #[tokio::test]
    #[ignore]
    async fn integration_fetch_all_with_defaults() {
        let providers = default_providers();
        let FetchState::Ok(all) = fetch_all(&providers).await else {
            panic!("unexpected state")
        };
        assert_eq!(all.providers.len(), providers.len());
    }
}
