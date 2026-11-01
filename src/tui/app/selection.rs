use ratatui::layout::Rect;

use super::App;

/// A drag over the transcript, held in painted rows and columns. Rows are
/// absolute (the scroll offset is already added), so scrolling moves the
/// highlight with the text it was dragged over instead of losing it.
pub struct Selection {
    /// First cell: (painted row, column inside the transcript area).
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
    /// Width it was made at. The rows it was mapped against are gone after a
    /// resize, so a selection from another width is dropped rather than
    /// highlighting whatever now sits at those cells.
    pub width: u16,
    /// False for a plain click: a click selects nothing and copies nothing.
    pub dragged: bool,
}

impl Selection {
    /// The drag's two ends, ordered.
    fn span(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// The columns this drag covers on one painted row, both ends included.
    pub fn columns(&self, row: u16, width: u16) -> Option<(u16, u16)> {
        let (start, end) = self.span();
        if row < start.0 || row > end.0 {
            return None;
        }
        let last = width.saturating_sub(1);
        Some((
            if row == start.0 { start.1 } else { 0 },
            if row == end.0 { end.1.min(last) } else { last },
        ))
    }

    /// The selected text out of the painted rows, and how many lines it holds.
    /// `top` is the absolute row of `rows[0]`.
    pub fn text(&self, rows: &[String], top: u16) -> (String, usize) {
        let mut copied: Vec<&str> = Vec::new();
        for (index, painted) in rows.iter().enumerate() {
            let Some((from, to)) = self.columns(top + index as u16, self.width) else {
                continue;
            };
            copied.push(slice(painted, from, to));
        }
        // A block ends with the last row that had a glyph: dragging into the
        // empty space under the transcript copies no trailing blank lines.
        while copied.last().is_some_and(|line| line.is_empty()) {
            copied.pop();
        }
        (copied.join("\n"), copied.len())
    }
}

/// The characters a cell range covers. Columns are terminal cells, so a wide
/// glyph counts for the two it paints.
fn slice(text: &str, from: u16, to: u16) -> &str {
    text[char_at(text, from)..char_at(text, to.saturating_add(1))].trim_end()
}

/// Byte index where the character at `column` starts, or the end of `text`.
fn char_at(text: &str, column: u16) -> usize {
    let mut cells = 0u16;
    for (byte, ch) in text.char_indices() {
        if cells >= column {
            return byte;
        }
        cells += unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(0)
            .max(1) as u16;
    }
    text.len()
}

impl App<'_> {
    /// Remember what the frame just painted for the transcript: the pointer is
    /// mapped through this geometry, and a copy reads the text back out of it.
    pub(crate) fn set_transcript(&self, area: Rect, scroll: u16, rows: Vec<String>) {
        self.transcript_area.set(area);
        self.transcript_scroll.set(scroll);
        *self.transcript_rows.borrow_mut() = rows;
    }

    /// Start a drag at a screen cell. A press outside the transcript leaves the
    /// pointer behaving as it did before: no selection, no drag.
    pub fn selection_begin(&mut self, column: u16, row: u16) {
        let area = self.transcript_area.get();
        let inside = column >= area.x
            && column < area.x + area.width
            && row >= area.y
            && row < area.y + area.height;
        self.dragging = inside;
        if !inside {
            return;
        }
        let at = (
            self.transcript_scroll.get() + (row - area.y),
            column - area.x,
        );
        self.selection = Some(Selection {
            anchor: at,
            cursor: at,
            width: area.width,
            dragged: false,
        });
    }

    /// Extend the drag. Rows and columns clamp to the transcript, so a pointer
    /// that leaves it still selects up to the last cell inside.
    pub fn selection_extend(&mut self, column: u16, row: u16) {
        if !self.dragging {
            return;
        }
        let area = self.transcript_area.get();
        let Some(mut selection) = self.selection.take() else {
            return;
        };
        if selection.width != area.width {
            return;
        }
        let row = row.clamp(area.y, area.y + area.height.saturating_sub(1));
        let column = column.clamp(area.x, area.x + area.width.saturating_sub(1));
        selection.cursor = (
            self.transcript_scroll.get() + (row - area.y),
            column - area.x,
        );
        selection.dragged |= selection.cursor != selection.anchor;
        self.selection = Some(selection);
    }

    /// Release: copy what the drag covered and confirm it. A click copies
    /// nothing and leaves no selection behind.
    pub fn selection_finish(&mut self) {
        if !self.dragging {
            return;
        }
        self.dragging = false;
        let Some(selection) = self.selection.as_ref() else {
            return;
        };
        if !selection.dragged {
            self.selection = None;
            return;
        }
        let rows = self.transcript_rows.borrow();
        let (text, lines) = selection.text(&rows, self.transcript_scroll.get());
        if text.trim().is_empty() {
            self.selection = None;
            return;
        }
        // The clipboard worker owns the only `arboard` handle, so the write is
        // one call to it; a failure says so instead of claiming a copy.
        self.toast = Some(match self.clipboard.write(&text) {
            Ok(()) => (
                if lines == 1 {
                    "✓ copied 1 line".to_string()
                } else {
                    format!("✓ copied {lines} lines")
                },
                std::time::Instant::now(),
            ),
            Err(error) => (format!("✗ copy failed: {error}"), std::time::Instant::now()),
        });
    }
}

#[cfg(test)]
#[path = "../../tests/tui/selection.rs"]
mod tests;
