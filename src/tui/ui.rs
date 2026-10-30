use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

// The header tests name the mode through `use super::*`.
#[cfg(test)]
use crate::agent::engine::AgentMode;
use crate::tui::app::{App, ModalSelection, Segment};
use crate::tui::event::AgentState;
use crate::tui::markdown;

mod header;
mod theme;

pub fn render(app: &App, frame: &mut Frame) {
    let input_height = app.input_height();
    // The permission box takes rows only while a question is on screen, so the
    // transcript keeps the height it had before the agent asked.
    let permission_height = if app.pending_permission.is_some() {
        4
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                 // Header: name, workspace, version
            Constraint::Min(3),                    // Conversation
            Constraint::Length(1),                 // Session title and agent state
            Constraint::Length(permission_height), // Permission question, when asked
            Constraint::Length(input_height),      // Prompt box
            Constraint::Length(1),                 // Footer: model, tokens, keys
        ])
        .split(frame.area());
    let header = chunks[0];
    let conversation = chunks[1];
    let status_bar = chunks[2];
    let permission_bar = chunks[3];
    let input_area = chunks[4];
    let footer = chunks[5];

    let dim = theme::dim();
    let (_, mode_style) = theme::mode_tag(app.mode);

    // 1. Header
    header::render(app, header, frame);

    let millis = crate::util::epoch_millis();

    // 2. Conversation
    let lines = transcript_lines(app, conversation.width);
    let visible_height = conversation.height.saturating_sub(1);
    let max_scroll = (lines.len() as u16).saturating_sub(visible_height);

    let scroll_offset = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0)),
        conversation,
    );

    if app.segments.is_empty() {
        render_splash(conversation, frame);
    }

    // 3. Session title on the left, agent state on the right
    let title = app.session_title.as_deref().unwrap_or("new session");
    let (state, state_style) = match &app.status {
        AgentState::Idle => ("ready", dim),
        AgentState::Thinking => ("thinking", Style::default().fg(Color::Yellow)),
        AgentState::Checking => ("checking", Style::default().fg(Color::Yellow)),
        AgentState::Compacting => ("compacting", Style::default().fg(Color::Yellow)),
        AgentState::Streaming => ("streaming", Style::default().fg(Color::Yellow)),
        AgentState::ExecutingTool(tool) => (tool.as_str(), Style::default().fg(Color::Cyan)),
    };
    let orb = Orb::for_state(&app.status);
    let used = title.chars().count() + state.chars().count() + 8;
    let fill = "─".repeat((status_bar.width as usize).saturating_sub(used));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─ ", dim),
            Span::styled(title.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(format!(" {fill} "), dim),
            Span::styled(orb.glyph(millis), Style::default().fg(orb.color())),
            Span::styled(format!(" {state}"), state_style),
            Span::styled(" ─", dim),
        ])),
        status_bar,
    );

    // 4. Permission question: its own box above the composer, the tool and its
    // effect on one row, the four answers on the next.
    if let Some(pending) = app.pending_permission.as_ref() {
        let request = &pending.request;
        let key = |label: &'static str, color: Color| {
            Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        };
        let hint = |text: &'static str| Span::styled(text, Style::default().fg(Color::DarkGray));

        let body = vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    request.tool.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", dim),
                Span::styled(request.effect.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::raw(" "),
                key("y", Color::Green),
                hint(" once   "),
                key("a", Color::Green),
                hint(" this category   "),
                key("u", Color::Yellow),
                hint(" unrestricted   "),
                key("n", Color::Red),
                hint(" deny"),
            ]),
        ];
        let block = Block::default()
            .title(Span::styled(
                " permission ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(Paragraph::new(body).block(block), permission_bar);
    }
    frame.render_widget(&app.input, input_area);

    // 4b. Inline Slash Command Auto-complete Menu
    let matches = app.slash_matches();
    if !matches.is_empty() && !app.popups_dismissed() {
        // Two border rows plus one row per command; the table is short enough to fit.
        let menu_height = matches.len() as u16 + 2;
        let menu_width = 62.min(frame.area().width.saturating_sub(4));
        let menu_y = input_area.y.saturating_sub(menu_height);
        let menu_area = Rect::new(input_area.x + 2, menu_y, menu_width, menu_height);

        frame.render_widget(Clear, menu_area);

        let selected = app.slash_selected.min(matches.len() - 1);
        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .map(|(i, (cmd, desc))| {
                let is_sel = i == selected;
                let prefix = if is_sel { "> " } else { "  " };
                let cmd_style = if is_sel {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let line = Line::from(vec![
                    Span::styled(format!("{prefix}{cmd:<12}"), cmd_style),
                    Span::raw(" "),
                    Span::styled(*desc, Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let menu_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(List::new(items).block(menu_block), menu_area);
    }

    // 4c. Inline `@` File/Folder Search Popup
    let matches = app.at_matches();
    if !matches.is_empty() && !app.popups_dismissed() {
        let layout = app.at_popup_layout(frame.area(), matches.len());
        let menu_area = layout.area;
        frame.render_widget(Clear, menu_area);

        let selected = app.at_selected.min(matches.len() - 1);
        // Sliding viewport calculation for auto-scrolling
        let window_start = layout.window_start;
        let window_end = (window_start + layout.max_visible).min(matches.len());

        let items: Vec<ListItem> = matches[window_start..window_end]
            .iter()
            .enumerate()
            .map(|(i, (is_dir, path))| {
                let actual_idx = window_start + i;
                let is_sel = actual_idx == selected;
                let prefix = if is_sel { "> " } else { "  " };
                let (kind_tag, kind_color) = if *is_dir {
                    ("[folder]", Color::Blue)
                } else {
                    ("[file]  ", Color::Green)
                };

                let line = Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan)),
                    Span::styled(kind_tag, Style::default().fg(kind_color)),
                    Span::raw(" "),
                    Span::styled(
                        path,
                        if is_sel {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();

        let title = if matches.len() > layout.max_visible {
            format!(
                " Reference File/Folder [{}/{}] (↑/↓: scroll, Tab/Enter: insert) ",
                selected + 1,
                matches.len()
            )
        } else {
            " Reference File/Folder (↑/↓: move, Tab/Enter: insert) ".to_string()
        };

        let menu_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(List::new(items).block(menu_block), menu_area);
    }

    // 5. Footer: model and context on the left, key hints on the right
    let config = &app.config;
    let effort_label = config.reasoning_effort.as_deref().unwrap_or("none");

    // One table, shared with the budget and `doctor`.
    let limit = crate::agent::engine::model_context_limit(&config.model);
    let max_context_limit = limit.tokens;
    let assumed = if limit.assumed { " (assumed)" } else { "" };

    let total = app.total_tokens.max(app.prompt_tokens);

    let token_display = if max_context_limit >= 1_000_000 {
        if total >= 1_000 {
            format!("~{}k/{}M", total / 1_000, max_context_limit / 1_000_000)
        } else {
            format!("{total}/{}M", max_context_limit / 1_000_000)
        }
    } else if total >= 1_000 {
        format!("~{}k/{}k", total / 1_000, max_context_limit / 1_000)
    } else {
        format!("{total}/{}k", max_context_limit / 1_000)
    };

    const HINTS: &str = "↑↓ history  ·  tab complete  ·  ⏎ send  ·  ctrl+c exit";
    let separator = Span::styled("  ·  ", dim);
    let left_area = Rect {
        width: footer
            .width
            .saturating_sub(HINTS.chars().count() as u16 + 2),
        ..footer
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(app.mode.label(), mode_style),
            separator.clone(),
            Span::styled(
                config.model.as_str(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            separator.clone(),
            Span::styled(
                format!("{token_display}{assumed}"),
                Style::default().fg(Color::Cyan),
            ),
            separator,
            Span::styled(format!("effort {effort_label}"), dim),
        ])),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(HINTS, dim)).alignment(Alignment::Right),
        footer,
    );

    // 6. Modal Selection Overlay (Model / Effort)
    if let Some(ref modal) = app.modal {
        let area = centered_rect(60, 50, frame.area());
        frame.render_widget(Clear, area);

        match modal {
            ModalSelection::Model {
                models,
                query,
                selected,
            } => {
                // The list is longer than the box: the items are plain text and
                // the stateful render scrolls the window to the selected row.
                // Without it the window stayed pinned to the first rows and the
                // cursor walked off the bottom, so the picker looked frozen.
                let visible = crate::tui::app::visible_models(models, query);
                let items: Vec<ListItem> = visible
                    .iter()
                    .map(|model| {
                        let suffix = if model.as_str() == app.config.model {
                            " [active]"
                        } else {
                            ""
                        };
                        let style = if model.as_str() == app.config.model {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        ListItem::new(format!("{model}{suffix}")).style(style)
                    })
                    .collect();

                let title = if app.refreshing_models {
                    // A spinner is the only sign of life while the gateway is
                    // slow; without it the frozen picker reads as a hang.
                    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let frame = FRAMES[(millis / 80) as usize % FRAMES.len()];
                    format!(" Refreshing /models {frame} (Esc: close) ")
                } else {
                    // The filter is the picker's search box: type to narrow,
                    // Backspace to widen.
                    let filter = if query.is_empty() {
                        "type to filter".to_string()
                    } else {
                        format!("filter: {query}")
                    };
                    format!(
                        " Select Model {}/{} · {filter} · ↑/↓ move · ctrl+r refresh · Esc ",
                        visible.len(),
                        models.len()
                    )
                };
                let list = List::new(items)
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    // A background, not just a color: the cursor must survive a
                    // font that has no ❯ glyph.
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, area, &mut state);
            }
            ModalSelection::Effort {
                model,
                options,
                selected,
            } => {
                let current_opt = options.get(*selected).copied().unwrap_or("none");
                let border_color = effort_color(current_opt, millis);

                let items: Vec<ListItem> = options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        let prefix = if i == *selected { "❯ " } else { "  " };
                        let style = if i == *selected {
                            let opt_color = effort_color(opt, millis);
                            Style::default().fg(opt_color).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        ListItem::new(format!("{prefix}{opt}")).style(style)
                    })
                    .collect();

                let title = format!(" Select Reasoning Effort for {model} ");
                let list = List::new(items).block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border_color)),
                );
                frame.render_widget(list, area);
            }
            ModalSelection::Session { rows, selected } => {
                // Same as the model picker: a long list scrolls with the cursor.
                let items: Vec<ListItem> = rows
                    .iter()
                    .map(|row| ListItem::new(row.clone()).style(Style::default().fg(Color::White)))
                    .collect();

                let list = List::new(items)
                    .block(
                        Block::default()
                            .title(" Select Session (↑/↓: move, Enter: open, Esc: cancel) ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("❯ ");
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(list, area, &mut state);
            }
        }
    }

    // 7. Toast Notification Popup (Bottom-Right)
    if let Some((ref msg, instant)) = app.toast
        && instant.elapsed() < std::time::Duration::from_secs(3)
    {
        let toast_width = (msg.len() as u16 + 4).min(frame.area().width.saturating_sub(2));
        let toast_area = Rect::new(
            frame.area().width.saturating_sub(toast_width + 2),
            input_area.y.saturating_sub(3),
            toast_width,
            3,
        );
        frame.render_widget(Clear, toast_area);
        let toast_color = if msg.starts_with('✓') {
            Color::Green
        } else {
            Color::Red
        };
        let toast_widget = Paragraph::new(msg.as_str())
            .style(Style::default().fg(toast_color))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(toast_color)),
            );
        frame.render_widget(toast_widget, toast_area);
    }
}

/// Conversation transcript: answer text plain, reasoning indented and gray.
/// Animated status orb, ported from libraries.dev thinking-orbs (dotted orb, 2D).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Orb {
    Idle,
    Breathing,
    Composing,
    Working,
}

impl Orb {
    fn for_state(state: &AgentState) -> Self {
        match state {
            AgentState::Idle => Orb::Idle,
            AgentState::Thinking => Orb::Breathing,
            AgentState::Checking | AgentState::Compacting => Orb::Working,
            AgentState::Streaming => Orb::Composing,
            AgentState::ExecutingTool(_) => Orb::Working,
        }
    }

    /// Frames per state: ring morph, undulating sash, particles on tilted orbits.
    fn frames(self) -> &'static [&'static str] {
        match self {
            Orb::Idle => &["○"],
            // Face-on ring filling and draining; `○`/`◌` alone read as a still dot.
            Orb::Breathing => &["○", "◔", "◐", "◕", "●", "◕", "◐", "◔"],
            Orb::Composing => &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄"],
            Orb::Working => &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
        }
    }

    fn color(self) -> Color {
        match self {
            Orb::Idle => Color::DarkGray,
            _ => Color::Cyan,
        }
    }

    fn glyph(self, millis: u128) -> &'static str {
        let frames = self.frames();
        frames[(millis / 90) as usize % frames.len()]
    }
}

/// Color for a reasoning-effort level; `max` cycles through a rainbow each tick.
fn effort_color(level: &str, millis: u128) -> Color {
    match level {
        "none" => Color::DarkGray,
        "minimal" => Color::Blue,
        "medium" => Color::Cyan,
        "high" => Color::Green,
        "highx" => Color::Magenta,
        "max" => {
            const RAINBOW: [Color; 7] = [
                Color::Red,
                Color::Rgb(255, 127, 0), // Orange
                Color::Yellow,
                Color::Green,
                Color::Cyan,
                Color::Blue,
                Color::Magenta,
            ];
            RAINBOW[((millis / 150) as usize) % RAINBOW.len()]
        }
        _ => Color::Yellow,
    }
}

fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    if let Some(cached) = app.transcript_cache.borrow().as_ref()
        && cached.revision == app.revision
        && cached.width == width
    {
        return cached.lines.clone();
    }

    let lines = build_transcript_lines(app, width);
    *app.transcript_cache.borrow_mut() = Some(crate::tui::app::TranscriptCache {
        revision: app.revision,
        width,
        lines: lines.clone(),
    });
    lines
}

fn build_transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for segment in &app.segments {
        match segment {
            Segment::Text(text) => lines.extend(markdown::render(text, width)),
            // A report keeps the shape it was printed in: markdown would fold
            // every row into one re-wrapped paragraph.
            Segment::Notice(text) => {
                lines.extend(text.lines().map(|line| Line::from(line.to_string())));
            }
            Segment::Thinking(text) => {
                // Transcript blocks are history: only the status line animates.
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("○", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        " thinking",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
                // Four columns, not six bytes: `│` is three bytes wide.
                let prefix = "  │ ";
                let content_width = (width as usize)
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(prefix))
                    .max(10);
                for raw_line in text.lines() {
                    let mut current = String::new();
                    let mut current_len = 0;
                    for word in raw_line.split_whitespace() {
                        let word_len = unicode_width::UnicodeWidthStr::width(word);
                        if current.is_empty() {
                            current.push_str(word);
                            current_len = word_len;
                        } else if current_len + 1 + word_len <= content_width {
                            current.push(' ');
                            current.push_str(word);
                            current_len += 1 + word_len;
                        } else {
                            lines.push(Line::from(Span::styled(
                                format!("{prefix}{current}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                            current.clear();
                            current.push_str(word);
                            current_len = word_len;
                        }
                    }
                    if !current.is_empty() || raw_line.trim().is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{prefix}{current}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
            }
        }
    }
    lines
}

/// Splash art for an empty session; 44 columns so it fits standard terminals.
const SPLASH: [&str; 5] = [
    " ██████  ████████   ████   ██    ██ ██    ██",
    "██       ██        ██  ██  ██    ██  ██  ██ ",
    " █████   ██████   ████████ ██    ██   ████  ",
    "      ██ ██       ██    ██  ██  ██   ██  ██ ",
    "██████   ████████ ██    ██   ████   ██    ██",
];

/// Centered SEAVX splash, degrading to a single centered word on narrow panes.
fn render_splash(area: Rect, frame: &mut Frame) {
    let style = Style::default().fg(Color::DarkGray);
    let art_width = SPLASH[0].chars().count() as u16;
    if area.width < art_width || area.height < SPLASH.len() as u16 {
        frame.render_widget(
            Paragraph::new("SEAVX")
                .alignment(Alignment::Center)
                .style(style),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }

    let slot = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(SPLASH.len() as u16),
            Constraint::Fill(1),
        ])
        .split(area)[1];
    let lines: Vec<Line> = SPLASH.iter().map(|row| Line::styled(*row, style)).collect();
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), slot);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
#[path = "../tests/tui/ui.rs"]
mod tests;
