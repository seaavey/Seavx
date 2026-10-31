use super::{App, Segment};

impl App<'_> {
    /// Append assistant/tool transcript text, coalescing into the last text block.
    pub fn push_text(&mut self, text: &str) {
        self.revision += 1;
        match self.segments.last_mut() {
            Some(Segment::Text(buf)) => buf.push_str(text),
            _ => self.segments.push(Segment::Text(text.to_string())),
        }
    }

    /// Append reasoning. A stream sends one delta per token, so a delta joins
    /// the open block instead of starting a line: a newline per delta turned a
    /// paragraph into a column of one-word rows.
    pub fn push_thinking(&mut self, text: &str) {
        self.revision += 1;
        match self.segments.last_mut() {
            Some(Segment::Thinking(buf)) => buf.push_str(text),
            _ => self.segments.push(Segment::Thinking(text.to_string())),
        }
    }

    /// Replace the whole transcript with a single status message.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.revision += 1;
        self.segments = vec![Segment::Text(text.into())];
    }

    pub fn clear_transcript(&mut self) {
        self.revision += 1;
        self.segments.clear();
        self.session_context = None;
    }

    /// Replace the transcript with the compacted session block and remember
    /// the summary for later requests.
    pub fn apply_compaction(&mut self, result: crate::tui::compact::CompactResult) {
        self.session_context = Some(result.summary.clone());
        self.revision += 1;

        let text = format!(
            "Session compacted\n\nBefore: ~{} lines, ~{} tokens (estimated)\nAfter:  ~{} lines, ~{} tokens (estimated)\n\n{}",
            result.before_lines,
            result.before_chars.div_ceil(4),
            result.after_lines,
            result.after_chars.div_ceil(4),
            result.summary
        );
        self.segments = vec![Segment::Text(text)];
        self.scroll = 0;
        self.auto_scroll = true;
    }

    /// Append a command report, stripped of terminal control characters and kept
    /// plain: a report is columns and rows, not markdown.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        let text = crate::tui::commands::sanitize(&text.into());
        self.revision += 1;
        let framed = format!("\n{text}\n");
        match self.segments.last_mut() {
            Some(Segment::Notice(buf)) => buf.push_str(&framed),
            _ => self.segments.push(Segment::Notice(framed)),
        }
        self.scroll = self.max_scroll();
        self.auto_scroll = true;
    }

    /// Highest scroll offset: rows the transcript runs past the viewport.
    pub fn max_scroll(&self) -> u16 {
        self.painted_rows
            .get()
            .saturating_sub(self.painted_height.get())
    }

    /// Record the transcript geometry the frame just painted. Scrolling is in
    /// wrapped rows, and only the frame knows how many there are.
    pub(crate) fn set_painted(&self, rows: u16, height: u16) {
        self.painted_rows.set(rows);
        self.painted_height.set(height);
    }
}
