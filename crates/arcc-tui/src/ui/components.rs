use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};
use ratatui_markdown::markdown::MarkdownRenderer;
use ratatui_markdown::theme::ThemeConfig;
use tui_spinner::FluxFrames;
use crate::commands;

// Warm-tone palette (aligned with logo #FF5722)
const CLR_USER: Color = Color::White;
const CLR_TOOL: Color = Color::Rgb(140, 140, 140);
const CLR_INPUT_ACCENT: Color = Color::Rgb(255, 138, 101);
const CLR_STATUS_IDLE: Color = Color::Rgb(160, 200, 120);
const CLR_STATUS_BUSY: Color = Color::Rgb(255, 200, 100);
const CLR_STATUS_STREAM: Color = Color::Rgb(255, 210, 180);
const CLR_STATUS_ERR: Color = Color::Rgb(255, 100, 100);

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

    let renderer = MarkdownRenderer::new(area.width as usize);
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
