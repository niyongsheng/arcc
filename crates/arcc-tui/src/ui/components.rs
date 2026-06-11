use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        BarChart, Block, Borders, Cell, Clear, LineGauge, List, ListItem, Paragraph, Row,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    },
    Frame,
};
use ratatui_markdown::markdown::{MarkdownRenderer, RenderHooks};
use ratatui_markdown::theme::ThemeConfig;
use ratatui_markdown::highlight::{HighlightHooks, TreeSitterHighlighter};
use ratatui_markdown::tree::{CollapsibleTree, KeyStyle};
use std::sync::{Arc, LazyLock};
use tui_spinner::FluxFrames;
use crate::commands;

// ── Dashboard data types ─────────────────────────────────────────────

/// System information collected for the dashboard.
#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub cpu_count: usize,
    pub uptime: String,
    pub version: String,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
}

/// All data backing the dashboard view.
#[derive(Debug, Clone)]
pub struct DashboardData {
    pub system: SystemInfo,
    pub sessions: Vec<String>,             // formatted session rows (rendered as table)
    pub session_ids: Vec<String>,           // full session UUIDs (for selection lookup)
    pub session_count: usize,
    pub msg_count: usize,
    pub token_daily: Vec<(String, u64)>,   // (date_label, total_tokens)
    pub total_input: i64,
    pub total_output: i64,
    pub audit_items: Vec<(String, String, bool)>, // (timestamp, label, ok)
}

/// Live system metrics updated every ~2 s.
#[derive(Debug, Clone, Default)]
pub struct LiveMetrics {
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

// Warm-tone palette (aligned with logo #FF5722)
const CLR_USER: Color = Color::White;
const CLR_TOOL: Color = Color::Rgb(140, 140, 140);
const CLR_INPUT_ACCENT: Color = Color::Rgb(255, 138, 101);
const CLR_STATUS_IDLE: Color = Color::Rgb(160, 200, 120);
const CLR_STATUS_BUSY: Color = Color::Rgb(255, 200, 100);
const CLR_STATUS_STREAM: Color = Color::Rgb(255, 210, 180);
const CLR_STATUS_ERR: Color = Color::Rgb(255, 100, 100);

// ---------------------------------------------------------------------------
// TreeAwareHooks — custom RenderHooks that renders JSON/TOML code blocks
// as collapsible trees and delegates everything else to HighlightHooks.
// ---------------------------------------------------------------------------

struct TreeAwareHooks {
    highlight: HighlightHooks,
    max_width: usize,
}

impl TreeAwareHooks {
    fn new(highlighter: Arc<TreeSitterHighlighter>, max_width: usize) -> Self {
        Self {
            highlight: HighlightHooks::new(highlighter, max_width),
            max_width,
        }
    }
}

impl RenderHooks for TreeAwareHooks {
    fn render_code_block(&self, lang: &str, code: &str) -> Option<Vec<Line<'static>>> {
        // JSON/TOML → collapsible tree
        if lang == "json" || lang == "toml" {
            let style = if lang == "json" { KeyStyle::Json } else { KeyStyle::Toml };
            let mut tree = if lang == "json" {
                CollapsibleTree::from_json_str(code)?
            } else {
                CollapsibleTree::from_toml_str(code)?
            }
            .with_key_style(style)
            .with_show_root(false);
            tree.expand_all();
            return Some(tree.render_lines(self.max_width, &ThemeConfig::default()));
        }
        // Everything else → syntax highlighting via HighlightHooks
        self.highlight.render_code_block(lang, code)
    }
}

// ---------------------------------------------------------------------------
// Chat renderer — ratatui-markdown
// ---------------------------------------------------------------------------

/// Render the main chat area with vertical scroll support.
///
/// `scroll_offset` controls how many lines the view is shifted **up** from the
/// bottom (0 = follow newest content at the bottom).
pub fn render_chat(f: &mut Frame, area: Rect, messages: &[String], scroll_offset: usize) {
    let mut text = String::new();

    for msg in messages {
        if let Some(t) = msg.strip_prefix("🧑 ") {
            text.push_str(&format!("**│ {t}**\n\n"));
        } else if let Some(t) = msg.strip_prefix("🤖 ") {
            text.push_str(t);
            text.push('\n');
            text.push('\n');
        } else if let Some(t) = msg.strip_prefix("🧠 ") {
            text.push_str(&format!("_🧠 {t}_\n\n"));
        } else if let Some(t) = msg.strip_prefix("⚡ ") {
            text.push_str(&format!("_⚡ {t}_\n\n"));
        } else if let Some(t) = msg.strip_prefix("⚠ ") {
            text.push_str(&format!("**⚠ {t}**\n\n"));
        } else if let Some(t) = msg.strip_prefix("✓ ") {
            text.push_str(&format!("__✓ {t}__\n\n"));
        } else if let Some(t) = msg.strip_prefix("✗ ") {
            text.push_str(&format!("__✗ {t}__\n\n"));
        } else {
            text.push_str(msg);
            text.push('\n');
        }
    }

    let renderer = {
        static HIGHLIGHTER: LazyLock<Arc<TreeSitterHighlighter>> =
            LazyLock::new(|| Arc::new(TreeSitterHighlighter::new()));
        let hooks = TreeAwareHooks::new(HIGHLIGHTER.clone(), area.width as usize);
        MarkdownRenderer::new(area.width as usize)
            .with_render_hooks(Box::new(hooks))
    };
    let blocks = renderer.parse(&text);
    let lines = renderer.render(&blocks, &ThemeConfig::default());
    let total_lines = lines.len();
    let visible = area.height as usize;

    // scroll_offset = 0 → auto-follow (show bottom of content).
    // Larger scroll_offset → user scrolled up toward older content.
    let max_offset = total_lines.saturating_sub(visible);
    let clamped = scroll_offset.min(max_offset);
    let skip = max_offset - clamped; // lines hidden from top

    let mut paragraph = Paragraph::new(Text::from(lines));
    paragraph = paragraph.scroll((skip as u16, 0));
    f.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Spinner helpers — tui-spinner FluxFrames presets
// ---------------------------------------------------------------------------

/// Pick an animation frame character for the current status and tick.
fn spinner_char(status: &str, tick: u64) -> char {
    let frames: &[char] = match status {
        "thinking" | "loading" | "planning" => FluxFrames::CLASSIC,
        "streaming" => FluxFrames::BOUNCE,
        "executing" => FluxFrames::DICE,
        "waiting" | "waiting..." => FluxFrames::DIAMOND,
        _ => return ' ',
    };
    frames[(tick / 3) as usize % frames.len()]
}

/// Pick the colour for a given status.
fn status_color(status: &str) -> Color {
    match status {
        "idle" | "connected" => CLR_STATUS_IDLE,
        "thinking" | "loading" | "planning" => CLR_STATUS_BUSY,
        "streaming" | "executing" => CLR_STATUS_STREAM,
        "waiting" | "waiting..." => CLR_STATUS_BUSY,
        "error" => CLR_STATUS_ERR,
        _ => CLR_TOOL,
    }
}

fn status_style(status: &str) -> Style {
    Style::default().fg(status_color(status))
}

// ---------------------------------------------------------------------------
// Input / Status / Layout
// ---------------------------------------------------------------------------

/// Render the input line with command highlighting.
///
/// When input starts with `/`, the command name is highlighted:
/// - Known commands → cyan
/// - Unknown commands → red
/// - Arguments after the command name → default user color
///   Regular input (no `/` prefix) is rendered as before.
pub fn render_input(f: &mut Frame, area: Rect, input: &str) {
    let spans = if let Some(rest) = input.strip_prefix('/') {
        // Command mode — highlight the command name
        let cmd_name = rest
            .split_whitespace()
            .next()
            .unwrap_or("");
        let known = commands::find(cmd_name).is_some();

        let cmd_color = if known {
            Color::Cyan
        } else {
            Color::Red
        };

        let cmd_part = format!("/{cmd_name}");
        let args_start = cmd_part.len();
        let args_part = &input[args_start..];

        if known || !cmd_name.is_empty() {
            vec![
                Span::styled("> ", Style::default().fg(CLR_INPUT_ACCENT)),
                Span::styled(cmd_part, Style::default().fg(cmd_color).add_modifier(Modifier::BOLD)),
                Span::styled(args_part, Style::default().fg(CLR_USER)),
            ]
        } else {
            vec![
                Span::styled("> ", Style::default().fg(CLR_INPUT_ACCENT)),
                Span::styled(input, Style::default().fg(CLR_USER)),
            ]
        }
    } else {
        // Regular input — default style
        vec![
            Span::styled("> ", Style::default().fg(CLR_INPUT_ACCENT)),
            Span::styled(input, Style::default().fg(CLR_USER)),
        ]
    };

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), area);
}

/// Render the status bar with an animated spinner.
pub fn render_status(f: &mut Frame, area: Rect, status: &str, tick: u64, thinking_mode: bool) {
    let style = status_style(status);
    let thinking_tag = if thinking_mode { " 🧠 on" } else { "" };
    let ch = if status == "idle" || status == "connected" {
        "●".to_string()
    } else {
        spinner_char(status, tick).to_string()
    };
    let text = Line::from(Span::styled(
        format!(" {ch} {status}{thinking_tag}"),
        style,
    ));
    f.render_widget(Paragraph::new(text), area);
}

/// Render a horizontal divider line.
pub fn render_divider(f: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(CLR_TOOL),
    ));
    f.render_widget(Paragraph::new(line), area);
}

/// Render the session title bar.
///
/// When AI is active (`phase < 4`) a star animation is shown.
pub fn render_title(f: &mut Frame, area: Rect, session_id: &str, mode: &str, phase: u8) {
    let short = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };

    let ch = if (phase as usize) < FluxFrames::STAR.len() {
        FluxFrames::STAR[phase as usize]
    } else {
        ' '
    };

    let text = Line::from(vec![
        Span::styled(ch.to_string(), Style::default().fg(CLR_TOOL)),
        Span::styled(
            format!(" ARCC · {short} · {mode}"),
            Style::default().fg(CLR_TOOL).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

/// Layout: [title (1)] [chat (fill)] [status (1)] [divider (1)] [input (1)] [divider (1)].
pub fn main_layout(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // [0] session title
            Constraint::Min(1),       // [1] chat area
            Constraint::Length(1),   // [2] status bar
            Constraint::Length(1),   // [3] divider
            Constraint::Length(1),   // [4] input line
            Constraint::Length(1),   // [5] bottom divider
        ])
        .split(area)
        .to_vec()
}

// ---------------------------------------------------------------------------
// Dashboard — ratatui built-in widgets
// ---------------------------------------------------------------------------

/// Dashboard accent colors
const CLR_DASH_ACCENT: Color = Color::Rgb(255, 138, 101);
const CLR_DASH_CHART: Color = Color::Rgb(100, 180, 255);
const CLR_DASH_INPUT: Color = Color::Rgb(100, 210, 255);
const CLR_DASH_OUTPUT: Color = Color::Rgb(255, 150, 200);
const CLR_DASH_OK: Color = Color::Rgb(120, 200, 120);
const CLR_DASH_ERR: Color = Color::Rgb(255, 100, 100);

/// Format a number into a human-readable short form (1.2k, 3.4M).
fn fmt_num(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// Render the full-screen dashboard overlay inside the chat area.
/// `cursor` is the currently selected session row index.
/// `live` provides real-time CPU/memory/network gauges (updated every ~2 s).
pub fn render_dashboard(
    f: &mut Frame,
    area: Rect,
    data: &DashboardData,
    scroll: usize,
    cursor: usize,
    live: &LiveMetrics,
) {
    if area.width < 60 || area.height < 12 {
        let msg = Paragraph::new("Terminal too small for dashboard (min 60×12)")
            .style(Style::new().fg(CLR_STATUS_ERR))
            .block(Block::default().borders(Borders::ALL).title(" Dashboard "));
        f.render_widget(msg, area);
        return;
    }

    // Clear chat area before drawing the dashboard
    f.render_widget(Clear, area);

    // Outer block with rounded-style border
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(" 📊 Dashboard ")
        .title_alignment(Alignment::Center)
        .title_style(Style::new().fg(CLR_DASH_ACCENT).add_modifier(Modifier::BOLD));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // The dashboard might be short — try to give each section room
    let (sys_h, chart_h, table_h) = if inner.height >= 16 {
        (5, 7, inner.height.saturating_sub(13))
    } else if inner.height >= 12 {
        (4, 5, inner.height.saturating_sub(10))
    } else {
        (3, 3, inner.height.saturating_sub(7))
    };

    let vert = Layout::vertical([
        Constraint::Length(sys_h),
        Constraint::Length(chart_h),
        Constraint::Min(table_h),
    ])
    .split(inner);

    // ── Top row: System Info (left) + Token Gauges (right) ──
    let top = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(vert[0]);

    render_sys_panel(f, top[0], &data.system, live);
    render_token_panel(f, top[1], data.total_input, data.total_output);

    // ── Middle row: Daily Chart (left) + Audit (right) ──
    let mid = Layout::horizontal([
        Constraint::Percentage(60),
        Constraint::Percentage(40),
    ])
    .split(vert[1]);

    render_chart_panel(f, mid[0], &data.token_daily);
    render_audit_panel(f, mid[1], &data.audit_items);

    // ── Bottom row: Sessions Table (with cursor for selection highlighting) ──
    let n_sessions = data.sessions.len();
    let clamped_cursor = cursor.min(n_sessions.saturating_sub(1));
    render_sessions_panel(f, vert[2], &data.sessions, scroll, clamped_cursor);
}

// ── Panel renderers ──────────────────────────────────────────────────

fn render_sys_panel(
    f: &mut Frame,
    area: Rect,
    sys: &SystemInfo,
    live: &LiveMetrics,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(" System ")
        .title_style(Style::new().fg(CLR_DASH_ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Top: text info (2 lines), bottom: live gauges (3 lines)
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
    ])
    .split(inner);

    // ── Static text info ──
    let info_lines = vec![
        Line::from(vec![
            Span::styled("OS  ", Style::new().fg(CLR_TOOL)),
            Span::styled(&sys.os, Style::new().fg(CLR_USER)),
        ]),
        Line::from(vec![
            Span::styled("Up  ", Style::new().fg(CLR_TOOL)),
            Span::styled(
                format!("{}  ·  {} cores  ·  ARCC {}", sys.uptime, sys.cpu_count, sys.version),
                Style::new().fg(CLR_USER),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(Text::from(info_lines)), chunks[0]);

    // ── Live gauges (CPU, MEM, NET) ──
    let gauges = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(chunks[1]);

    let fmt_net = |bps: f64| -> String {
        if bps >= 1_000_000.0 {
            format!("{:.1} MB/s", bps / 1_000_000.0)
        } else if bps >= 1_000.0 {
            format!("{:.0} KB/s", bps / 1_000.0)
        } else {
            format!("{:.0} B/s", bps)
        }
    };

    // CPU gauge
    let cpu_r = (live.cpu_pct / 100.0).clamp(0.0, 1.0);
    f.render_widget(
        LineGauge::default()
            .filled_style(Style::new().fg(if live.cpu_pct > 80.0 { CLR_DASH_ERR } else { CLR_DASH_CHART }))
            .ratio(cpu_r)
            .label(format!("CPU {:>5.1}%", live.cpu_pct)),
        gauges[0],
    );

    // Memory gauge
    let mem_r = (live.mem_pct / 100.0).clamp(0.0, 1.0);
    f.render_widget(
        LineGauge::default()
            .filled_style(Style::new().fg(CLR_DASH_INPUT))
            .ratio(mem_r)
            .label(format!("MEM {:>5.1}%", live.mem_pct)),
        gauges[1],
    );

    // Network line (down / up rates)
    let net_line = Line::from(vec![
        Span::styled("NET ", Style::new().fg(CLR_TOOL)),
        Span::styled("↓ ", Style::new().fg(CLR_DASH_OK)),
        Span::styled(fmt_net(live.rx_rate), Style::new().fg(CLR_USER)),
        Span::styled("  ↑ ", Style::new().fg(CLR_DASH_OUTPUT)),
        Span::styled(fmt_net(live.tx_rate), Style::new().fg(CLR_USER)),
    ]);
    f.render_widget(Paragraph::new(net_line), gauges[2]);
}

fn render_token_panel(f: &mut Frame, area: Rect, total_input: i64, total_output: i64) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(" Token Usage (7d) ")
        .title_style(Style::new().fg(CLR_DASH_ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = total_input + total_output;
    let in_ratio = if total > 0 {
        total_input as f64 / total as f64
    } else {
        0.0
    };
    let out_ratio = if total > 0 {
        total_output as f64 / total as f64
    } else {
        0.0
    };

    let summary_line = Line::from(Span::styled(
        format!("Total: {} tokens", fmt_num(total)),
        Style::new().fg(CLR_DASH_ACCENT).add_modifier(Modifier::BOLD),
    ));

    let gauge_area = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(summary_line).centered(), gauge_area[0]);

    let in_label = format!("In  {}", fmt_num(total_input));
    f.render_widget(
        LineGauge::default()
            .filled_style(Style::new().fg(CLR_DASH_INPUT))
            .ratio(in_ratio)
            .label(in_label),
        gauge_area[1],
    );

    let out_label = format!("Out {}", fmt_num(total_output));
    f.render_widget(
        LineGauge::default()
            .filled_style(Style::new().fg(CLR_DASH_OUTPUT))
            .ratio(out_ratio)
            .label(out_label),
        gauge_area[2],
    );
}

fn render_chart_panel(f: &mut Frame, area: Rect, daily: &[(String, u64)]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(" Daily Token Usage ")
        .title_style(Style::new().fg(CLR_DASH_ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if daily.is_empty() {
        let msg = Paragraph::new("No data yet")
            .style(Style::new().fg(CLR_TOOL))
            .centered();
        f.render_widget(msg, inner);
        return;
    }

    // Build bar labels + values. Reverse so oldest is on the left.
    // We own the data so borrow it instead of moving.
    let chart_pairs: Vec<(String, u64)> = daily
        .iter()
        .rev()
        .map(|(l, v)| (l.clone(), *v))
        .collect();

    let max_v = chart_pairs.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1);

    // Clamp bar count to fit area width
    let bar_w: u16 = 4;
    let gap_w: u16 = 1;
    let max_bars = (inner.width.saturating_sub(4) / (bar_w + gap_w)) as usize;
    let slice: &[(String, u64)] = if chart_pairs.len() > max_bars && max_bars > 0 {
        let start = chart_pairs.len() - max_bars;
        &chart_pairs[start..]
    } else {
        &chart_pairs
    };

    // Convert to borrowed &str references for BarChart
    let bar_data: Vec<(&str, u64)> = slice.iter().map(|(l, v)| (l.as_str(), *v)).collect();

    let chart = BarChart::default()
        .bar_width(bar_w)
        .bar_gap(gap_w)
        .bar_style(Style::new().fg(CLR_DASH_CHART))
        .value_style(Style::new().fg(CLR_TOOL))
        .label_style(Style::new().fg(CLR_USER))
        .data(&bar_data)
        .max(max_v);
    f.render_widget(chart, inner);
}

fn render_audit_panel(f: &mut Frame, area: Rect, items: &[(String, String, bool)]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(" Recent Audit ")
        .title_style(Style::new().fg(CLR_DASH_ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if items.is_empty() {
        let msg = Paragraph::new("No audit events")
            .style(Style::new().fg(CLR_TOOL))
            .centered();
        f.render_widget(msg, inner);
        return;
    }

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|(ts, label, ok)| {
            let icon = if *ok { " ✓" } else { " ✗" };
            let color = if *ok { CLR_DASH_OK } else { CLR_DASH_ERR };
            let prefix = Span::styled(icon, Style::new().fg(color));
            let time = Span::styled(format!(" {}", ts), Style::new().fg(CLR_TOOL));
            let txt = Span::styled(format!(" {}", label), Style::new().fg(CLR_USER));
            ListItem::new(Line::from(vec![prefix, time, txt]))
        })
        .collect();

    let list = List::new(list_items).highlight_style(
        Style::new()
            .fg(CLR_USER)
            .add_modifier(Modifier::DIM),
    );
    f.render_widget(list, inner);
}

fn render_sessions_panel(
    f: &mut Frame,
    area: Rect,
    sessions: &[String],
    scroll: usize,
    cursor: usize,
) {
    // Compute available lines for the table body
    let body_h = area.height.saturating_sub(3) as usize; // header row + borders

    let n_rows = sessions.len();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(CLR_TOOL))
        .title(format!(
            " Sessions ({}) — ↑↓ select, Enter to view  [Esc] ",
            n_rows,
        ))
        .title_style(Style::new().fg(CLR_DASH_ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if n_rows == 0 {
        let msg = Paragraph::new("No sessions found")
            .style(Style::new().fg(CLR_TOOL))
            .centered();
        f.render_widget(msg, inner);
        return;
    }

    let clamped_cursor = cursor.min(n_rows.saturating_sub(1));
    let clamped_scroll = scroll.min(n_rows.saturating_sub(body_h));

    // Build visible rows with selection highlighting
    let visible_rows: Vec<Row> = sessions
        .iter()
        .enumerate()
        .skip(clamped_scroll)
        .take(body_h)
        .map(|(idx, s)| {
            let cols: Vec<&str> = s.split('|').collect();
            let is_selected = idx == clamped_cursor;

            // Build cells — first one gets selection marker
            let id_text = if is_selected {
                format!("▸ {}", cols.first().copied().unwrap_or(""))
            } else {
                format!("  {}", cols.first().copied().unwrap_or(""))
            };

            let row = Row::new(vec![
                Cell::from(id_text),
                Cell::from(cols.get(1).copied().unwrap_or("")),
                Cell::from(cols.get(2).copied().unwrap_or("")),
                Cell::from(cols.get(3).copied().unwrap_or("")),
            ]);

            if is_selected {
                row.style(
                    Style::new()
                        .bg(Color::Rgb(55, 55, 90))
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(18),
    ];

    let header = Row::new(vec![
        Cell::from(Span::styled(
            "ID",
            Style::new()
                .fg(CLR_DASH_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "Name",
            Style::new()
                .fg(CLR_DASH_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "Mode",
            Style::new()
                .fg(CLR_DASH_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "Active",
            Style::new()
                .fg(CLR_DASH_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
    ]);

    let table = Table::new(visible_rows, widths)
        .header(header)
        .column_spacing(1)
        .style(Style::new().fg(CLR_USER));
    f.render_widget(table, inner);

    // Scrollbar
    if n_rows > body_h {
        let max_scroll = n_rows.saturating_sub(body_h);
        let mut state =
            ScrollbarState::new(max_scroll).position(clamped_scroll.min(max_scroll));

        let scroll_area = Rect {
            x: inner.right().saturating_sub(1),
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"));
        f.render_stateful_widget(scrollbar, scroll_area, &mut state);
    }
}
