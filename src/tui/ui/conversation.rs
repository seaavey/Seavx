use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::splash;
use super::theme;
use crate::tui::app::{App, Segment};
use crate::tui::markdown;

pub(super) fn render(app: &App, area: Rect, frame: &mut Frame) {
    let lines = transcript_lines(app, area.width);
    // `transcript_lines` just made the cache current for this width, so the row
    // count is here: the widget wraps, a source line is not a row, and scrolling
    // counts the rows it paints.
    let rows = app
        .transcript_cache
        .borrow()
        .as_ref()
        .map_or(0, |cache| cache.rows);
    let visible_height = area.height.saturating_sub(1);
    app.set_painted(rows, visible_height);
    let max_scroll = app.max_scroll();

    let scroll_offset = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0)),
        area,
    );

    if app.segments.is_empty() {
        splash::render(app, area, frame);
    }
}

/// The transcript lines for `width`, cached per revision and width together with
/// the rows they wrap to: `line_count` walks the whole transcript, so it must not
/// run twice.
pub(crate) fn transcript_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    if let Some(cached) = app.transcript_cache.borrow().as_ref()
        && cached.revision == app.revision
        && cached.width == width
    {
        return cached.lines.clone();
    }

    let lines = build_transcript_lines(app, width);
    let rows = u16::try_from(
        Paragraph::new(lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(width),
    )
    .unwrap_or(u16::MAX);
    *app.transcript_cache.borrow_mut() = Some(crate::tui::app::TranscriptCache {
        revision: app.revision,
        width,
        lines: lines.clone(),
        rows,
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
                    Span::styled(" thinking", theme::dim().add_modifier(Modifier::ITALIC)),
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
                                theme::dim(),
                            )));
                            current.clear();
                            current.push_str(word);
                            current_len = word_len;
                        }
                    }
                    if !current.is_empty() || raw_line.trim().is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{prefix}{current}"),
                            theme::dim(),
                        )));
                    }
                }
            }
        }
    }
    lines
}
