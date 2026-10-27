use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::tui::app::App;
use crate::tui::event::AgentState;

pub fn render(app: &App, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Top Header Bar (Hermes-style banner)
            Constraint::Min(8),    // Main Output & History Area
            Constraint::Length(1), // Divider / Action line
            Constraint::Length(3), // Input area (clean prompt `❯ `)
            Constraint::Length(1), // Bottom Status Bar (Model, Tokens, Metrics)
        ])
        .split(frame.area());

    // 1. Top Header Bar: 🏺 Seavx Agent
    let header_line = Line::from(vec![
        Span::styled("🏺 Seavx", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Autonomous AI Coding Agent", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(header_line), chunks[0]);

    // 2. Main Output Area: Clean dark terminal background with auto-scroll
    let visible_height = chunks[1].height.saturating_sub(1);
    let total_lines = app.response.lines().count() as u16;

    let scroll_offset = if app.auto_scroll {
        total_lines.saturating_sub(visible_height)
    } else {
        app.scroll.min(total_lines.saturating_sub(visible_height))
    };

    let response_widget = Paragraph::new(app.response.as_str())
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0))
        .block(Block::default().borders(Borders::NONE));
    frame.render_widget(response_widget, chunks[1]);

    // 3. Divider line with active context
    let task_status_str = match &app.status {
        AgentState::Idle => "Idle — Ready for task",
        AgentState::Thinking => "Thinking...",
        AgentState::Streaming => "Generating response...",
        AgentState::ExecutingTool(tool) => tool.as_str(),
    };

    let divider = Line::from(vec![
        Span::styled("─".repeat(chunks[2].width.saturating_sub(task_status_str.len() as u16 + 4) as usize), Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(task_status_str, Style::default().fg(Color::Cyan)),
        Span::raw(" ─"),
    ]);
    frame.render_widget(Paragraph::new(divider), chunks[2]);

    // 4. Input Prompt (Clean `❯ `)
    frame.render_widget(&app.input, chunks[3]);

    // 5. Bottom Status Bar: Hermes Status Strip
    let config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
    let effort_label = config.reasoning_effort.as_deref().unwrap_or("none");

    // Dynamic model context window limit detection
    let max_context_limit: i32 = if config.model.contains("claude") || config.model.contains("nemotron") || config.model.contains("gemini") {
        1_000_000 // 1M context
    } else if config.model.contains("deepseek") || config.model.contains("glm") {
        128_000 // 128k context
    } else {
        128_000
    };

    let total = app.total_tokens.max(app.prompt_tokens);
    let percentage = if max_context_limit > 0 {
        ((total as f32 / max_context_limit as f32) * 100.0).min(100.0)
    } else {
        0.0
    };

    let filled_blocks = ((percentage / 10.0).round() as usize).min(10);
    let empty_blocks = 10usize.saturating_sub(filled_blocks);
    let progress_bar = format!("[{}{}]", "█".repeat(filled_blocks), " ".repeat(empty_blocks));

    let token_display = if total >= 1_000 {
        format!("~{}k/{}M", total / 1_000, max_context_limit / 1_000_000)
    } else {
        format!("{total}/{}M", max_context_limit / 1_000_000)
    };

    let bottom_status = Line::from(vec![
        Span::styled(" 🍸 ", Style::default().fg(Color::Cyan)),
        Span::styled(&config.model, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(token_display, Style::default().fg(Color::Yellow)),
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{progress_bar} ~{:.0}%", percentage), Style::default().fg(Color::Green)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("effort: {effort_label}"), Style::default().fg(Color::LightCyan)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled("⚡ Active", Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(bottom_status).style(Style::default().bg(Color::Rgb(20, 24, 34))), chunks[4]);
}
