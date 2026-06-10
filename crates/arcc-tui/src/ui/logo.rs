//! ARCC ASCII logo — rendered as the first chat message on startup.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

const LOGO_ART: &[&str] = &[
    "    █████╗ ██████╗  ██████╗ ██████╗",
    "   ██╔══██╗██╔══██╗██╔════╝██╔════╝",
    "   ███████║██████╔╝██║     ██║     ",
    "   ██╔══██║██╔══██╗██║     ██║     ",
    "   ██║  ██║██║  ██║╚██████╗╚██████╗",
    "   ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ ╚═════╝",
];

/// Render the ARCC logo + version info in the given area.
pub fn render_logo(f: &mut Frame, area: Rect, version: &str, model: &str) {
    let layout = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            Constraint::Length(7),  // logo art + spacing
            Constraint::Length(1),  // version
            Constraint::Length(1),  // model
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // hint
            Constraint::Length(1),  // selection hint
            Constraint::Min(0),
        ])
        .split(area);

    // Logo art
    let art_lines: Vec<Line> = LOGO_ART
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                *line,
                Style::default()
                    .fg(Color::Rgb(255, 87, 34))
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    f.render_widget(
        Paragraph::new(Text::from(art_lines)).alignment(Alignment::Center),
        layout[0],
    );

    // Version
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("v{version}"),
            Style::default().fg(Color::Rgb(120, 120, 130)),
        )))
        .alignment(Alignment::Center),
        layout[1],
    );

    // Model
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("model · {model}"),
            Style::default().fg(Color::Rgb(120, 120, 130)),
        )))
        .alignment(Alignment::Center),
        layout[2],
    );

    // Hint
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Type a prompt and press Enter to start.",
            Style::default().fg(Color::Rgb(100, 180, 100)),
        )))
        .alignment(Alignment::Center),
        layout[4],
    );

    // Footer: help hint
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "/help for commands  ·  Ctrl+C to quit",
            Style::default().fg(Color::Rgb(120, 120, 130)),
        )))
        .alignment(Alignment::Center),
        layout[5],
    );
}

/// Generate the logo as a styled string for inline display (first chat message).
pub fn logo_messages(version: &str, model: &str) -> Vec<String> {
    let mut msgs = Vec::new();
    // The first message is the logo art.
    msgs.push(LOGO_ART.join("\n"));
    msgs.push(format!("v{version} · {model}"));
    msgs.push("Type a prompt and press Enter to start.".into());
    msgs
}

/// Build a single startup message with embedded newlines.
pub fn startup_message(version: &str, model: &str) -> String {
    format!(
        "{}\n\nv{} · {}\n\nType a prompt and press Enter to start.",
        LOGO_ART.join("\n"),
        version,
        model,
    )
}
