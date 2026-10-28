//! Markdown to ratatui line renderer for agent responses.
//!
//! Provider output is CommonMark/GFM text; this module turns it into styled
//! `Line`s. Parsing lives only here so provider and protocol code stay clean,
//! and the whole response is re-parsed per frame so half-streamed markdown
//! (unclosed fences, split delimiters) can never corrupt renderer state.

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

const INLINE_CODE: Color = Color::Cyan;
const CODE_TEXT: Color = Color::Green;
const QUOTE: &str = "│ ";
const BULLET: &str = "• ";

pub fn render(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    Renderer::new(width.max(20) as usize).run(Parser::new_ext(text, options))
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn heading_style(level: u8) -> Style {
    match level {
        1 => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        2 => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        3 => Style::default().add_modifier(Modifier::BOLD),
        _ => dim().add_modifier(Modifier::ITALIC),
    }
}

struct LinkState {
    dest: String,
    text: String,
}

struct Table {
    aligns: Vec<Alignment>,
    head: Vec<Vec<Span<'static>>>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    cell: Vec<Span<'static>>,
    row: Vec<Vec<Span<'static>>>,
    in_head: bool,
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    lists: Vec<Option<u64>>,
    quotes: usize,
    heading: Option<u8>,
    bold: bool,
    italic: bool,
    strike: bool,
    in_code: bool,
    table: Option<Table>,
    link: Option<LinkState>,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            spans: Vec::new(),
            lists: Vec::new(),
            quotes: 0,
            heading: None,
            bold: false,
            italic: false,
            strike: false,
            in_code: false,
            table: None,
            link: None,
        }
    }

    fn run(mut self, parser: Parser<'_>) -> Vec<Line<'static>> {
        for event in parser {
            match event {
                Event::Start(tag) => self.start(tag),
                Event::End(tag) => self.end(tag),
                Event::Text(text) => self.text(&text),
                Event::Code(code) => self.code(&code),
                Event::SoftBreak => self.push(" ", Style::default()),
                Event::HardBreak => self.flush(),
                Event::Rule => {
                    self.flush();
                    self.lines
                        .push(Line::from(Span::styled("─".repeat(self.width), dim())));
                }
                Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
                Event::TaskListMarker(done) => self.task_marker(done),
                _ => {}
            }
        }

        self.finish()
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.blank();
                self.heading = Some(level as u8);
            }
            Tag::Strong => self.bold = true,
            Tag::Emphasis => self.italic = true,
            Tag::Strikethrough => self.strike = true,
            Tag::BlockQuote(kind) => {
                self.blank();
                self.quotes += 1;
                if let Some(kind) = kind {
                    let (label, style) = match kind {
                        BlockQuoteKind::Note => ("note", Style::default().fg(Color::Cyan)),
                        BlockQuoteKind::Tip => ("tip", Style::default().fg(Color::Green)),
                        BlockQuoteKind::Important => {
                            ("important", Style::default().fg(Color::Magenta))
                        }
                        BlockQuoteKind::Warning => ("warning", Style::default().fg(Color::Yellow)),
                        BlockQuoteKind::Caution => ("caution", Style::default().fg(Color::Red)),
                    };
                    self.push(QUOTE, dim());
                    self.spans.push(Span::styled(
                        label.to_string(),
                        style.add_modifier(Modifier::BOLD),
                    ));
                    self.flush();
                }
            }
            Tag::CodeBlock(kind) => {
                self.blank();
                self.in_code = true;
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.trim().is_empty()
                {
                    self.lines.push(Line::from(Span::styled(
                        format!("  {}", lang.trim()),
                        dim().add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.begin_line();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last().copied().flatten() {
                    Some(next) => format!("{}{next}. ", "  ".repeat(depth)),
                    None => format!("{}{BULLET}", "  ".repeat(depth)),
                };
                self.spans.push(Span::styled(marker, dim()));
            }
            Tag::Table(aligns) => {
                self.blank();
                self.table = Some(Table {
                    aligns,
                    head: Vec::new(),
                    rows: Vec::new(),
                    cell: Vec::new(),
                    row: Vec::new(),
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = true;
                }
            }
            Tag::TableRow | Tag::TableCell => {}
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.link = Some(LinkState {
                    dest: dest_url.to_string(),
                    text: String::new(),
                });
            }
            Tag::FootnoteDefinition(_) | Tag::HtmlBlock | Tag::MetadataBlock(_) => {}
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if self.lists.is_empty() && self.quotes == 0 {
                    self.blank();
                }
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
                self.blank();
            }
            TagEnd::Strong => self.bold = false,
            TagEnd::Emphasis => self.italic = false,
            TagEnd::Strikethrough => self.strike = false,
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
                self.blank();
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.in_code = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                // Nested lists stay glued to their parent item; only close the gap
                // after the outermost list.
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Some(next)) = self.lists.last_mut() {
                    *next += 1;
                }
            }
            TagEnd::Table => {
                self.flush();
                if let Some(table) = self.table.take() {
                    self.render_table(table);
                }
                self.blank();
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.head = std::mem::take(&mut table.row);
                    table.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    if !table.in_head && !(row.is_empty()) {
                        table.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell);
                }
            }
            TagEnd::Link | TagEnd::Image => {
                if let Some(link) = self.link.take()
                    && !link.dest.is_empty()
                    && link.dest != link.text
                {
                    let style = dim().add_modifier(Modifier::UNDERLINED);
                    self.push(&format!(" ({})", link.dest), style);
                }
            }
            _ => {}
        }
    }

    fn code(&mut self, code: &str) {
        let style = Style::default().fg(INLINE_CODE);
        if let Some(table) = self.table.as_mut() {
            table.cell.push(Span::styled(code.to_string(), style));
            return;
        }
        self.push(code, style);
    }

    fn text(&mut self, text: &str) {
        if self.table.is_some() {
            let style = self.inline_style();
            if let Some(table) = self.table.as_mut() {
                table.cell.push(Span::styled(text.to_string(), style));
            }
            return;
        }

        if self.in_code {
            for chunk in text.split_inclusive('\n') {
                let line = chunk.strip_suffix('\n').unwrap_or(chunk);
                self.begin_line();
                self.spans.push(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(CODE_TEXT),
                ));
                if chunk.ends_with('\n') {
                    self.flush();
                }
            }
            return;
        }

        if let Some(link) = self.link.as_mut() {
            link.text.push_str(text);
        }
        let style = self.inline_style();
        self.push(text, style);
    }

    fn task_marker(&mut self, done: bool) {
        let (marker, style) = if done {
            ("[x] ", Style::default().fg(Color::Green))
        } else {
            ("[ ] ", dim())
        };
        self.push(marker, style);
    }

    fn inline_style(&self) -> Style {
        let mut style = match self.heading {
            Some(level) => heading_style(level),
            None => Style::default(),
        };
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn begin_line(&mut self) {
        if !self.spans.is_empty() {
            return;
        }
        for _ in 0..self.quotes {
            self.spans.push(Span::styled(QUOTE, dim()));
        }
    }

    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        self.begin_line();
        self.spans.push(Span::styled(text.to_string(), style));
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn render_table(&mut self, table: Table) {
        let columns = table.aligns.len().max(table.head.len());
        if columns == 0 {
            return;
        }

        let mut rows: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
        if !table.head.is_empty() {
            rows.push(table.head.clone());
        }
        rows.extend(table.rows);

        let plain = |cell: &[Span<'static>]| -> String {
            cell.iter().map(|span| span.content.as_ref()).collect()
        };

        let mut widths = vec![0usize; columns];
        for row in &rows {
            for (index, cell) in row.iter().enumerate().take(columns) {
                widths[index] = widths[index].max(UnicodeWidthStr::width(plain(cell).as_str()));
            }
        }

        // Fit the table into the available width; shrink the widest columns first.
        let gaps = columns.saturating_sub(1) * 2;
        let available = self.width.saturating_sub(gaps).max(columns * 2);
        while widths.iter().sum::<usize>() > available {
            let widest = widths
                .iter()
                .enumerate()
                .max_by_key(|(_, width)| **width)
                .map(|(index, _)| index)
                .unwrap_or(0);
            if widths[widest] <= 3 {
                break;
            }
            widths[widest] -= 1;
        }

        let mut rendered: Vec<Line<'static>> = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (index, width) in widths.iter().enumerate() {
                if index > 0 {
                    spans.push(Span::raw("  "));
                }
                let cell = row.get(index).map(Vec::as_slice).unwrap_or_default();
                let mut cell_spans = fit_spans(cell, *width);
                let used = cell_spans
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                    .sum::<usize>();
                let padding = width.saturating_sub(used);
                let (before, after) = match table.aligns.get(index) {
                    Some(Alignment::Right) => (padding, 0),
                    Some(Alignment::Center) => (padding / 2, padding - padding / 2),
                    _ => (0, padding),
                };
                if before > 0 {
                    spans.push(Span::raw(" ".repeat(before)));
                }
                if row_index == 0 && !table.head.is_empty() {
                    for span in &mut cell_spans {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                }
                spans.append(&mut cell_spans);
                if after > 0 {
                    spans.push(Span::raw(" ".repeat(after)));
                }
            }
            rendered.push(Line::from(spans));

            if row_index == 0 && !table.head.is_empty() {
                let mut rule: Vec<Span<'static>> = Vec::new();
                for (index, width) in widths.iter().enumerate() {
                    if index > 0 {
                        rule.push(Span::raw("  "));
                    }
                    rule.push(Span::styled("─".repeat(*width), dim()));
                }
                rendered.push(Line::from(rule));
            }
        }

        self.lines.extend(rendered);
    }
}

/// Keep the leading display width of a cell, marking the cut with an ellipsis.
fn fit_spans(cell: &[Span<'static>], width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0usize;

    for span in cell {
        let mut text = String::new();
        for ch in span.content.chars() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + ch_width > width {
                break;
            }
            used += ch_width;
            text.push(ch);
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if used >= width {
            break;
        }
    }

    let full = cell
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    if full > width
        && width > 1
        && let Some(last) = out.last_mut()
    {
        let mut text = last.content.to_string();
        if text.pop().is_some() {
            text.push('…');
        }
        last.content = text.into();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str, width: u16) -> String {
        render(text, width)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn style_of(text: &str, needle: &str) -> Style {
        render(text, 80)
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(needle))
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("span containing {needle:?} not rendered"))
    }

    #[test]
    fn full_document_snapshot() {
        let doc = r#"# Seavx Report

**Accurate** summary with `inline code` and a [link](https://example.com/docs).

## Findings

- Tool list matches `src/agent/tools.rs`
  - nested detail
- ~~deprecated~~ item

1. Inspect
2. Implement

- [x] Audit
- [ ] Tests

> **Note:** verify config first.

| Method | Endpoint | Description |
|---|---|---|
| GET | `/v1/models` | List models |
| POST | `/v1/chat/completions` | Create completion |

```rust
fn main() {
    println!("hi");
}
```

---

Done.
"#;

        let expected = r#"Seavx Report

Accurate summary with inline code and a link (https://example.com/docs).

Findings

• Tool list matches src/agent/tools.rs
  • nested detail
• deprecated item

1. Inspect
2. Implement

• [x] Audit
• [ ] Tests

│ Note: verify config first.

Method  Endpoint              Description
──────  ────────────────────  ─────────────────
GET     /v1/models            List models
POST    /v1/chat/completions  Create completion

  rust
  fn main() {
      println!("hi");
  }

────────────────────────────────────────────────────────────────
Done."#;

        let rendered = render(doc, 64)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(rendered, expected);
    }

    #[test]
    fn headings_keep_content_without_hashes() {
        let out = plain("# Main\n\n## Section\n\n### Sub\n\n#### Fourth\n", 80);
        assert!(out.contains("Main"));
        assert!(out.contains("Section"));
        assert!(out.contains("Sub"));
        assert!(out.contains("Fourth"));
        assert!(!out.contains('#'), "{out}");
        assert!(
            style_of("# Main\n", "Main").add_modifier != Modifier::empty()
                || style_of("# Main\n", "Main").fg == Some(Color::Cyan)
        );
    }

    #[test]
    fn inline_formatting_drops_delimiters_and_keeps_styles() {
        let out = plain(
            "**Bold** and *italic* and ***both*** and ~~strike~~ and `code()`",
            80,
        );
        assert!(!out.contains('*'), "{out}");
        assert!(!out.contains('~'), "{out}");
        assert!(!out.contains('`'), "{out}");
        assert!(out.contains("Bold") && out.contains("code()"));

        assert!(
            style_of("**Bold**", "Bold")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            style_of("*italic*", "italic")
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        let both = style_of("***both***", "both").add_modifier;
        assert!(both.contains(Modifier::BOLD) && both.contains(Modifier::ITALIC));
        assert!(
            style_of("~~strike~~", "strike")
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert_eq!(style_of("`code()`", "code()").fg, Some(INLINE_CODE));
    }

    #[test]
    fn lists_keep_order_hierarchy_and_task_markers() {
        let unordered = plain("- First\n- Second\n  - Nested\n", 80);
        assert!(unordered.contains("• First"), "{unordered}");
        assert!(unordered.contains("  • Nested"), "{unordered}");

        let ordered = plain("1. Inspect\n2. Implement\n3. Test\n", 80);
        assert!(ordered.contains("1. Inspect"), "{ordered}");
        assert!(ordered.contains("2. Implement"), "{ordered}");
        assert!(ordered.contains("3. Test"), "{ordered}");

        let tasks = plain("- [x] Done\n- [ ] Todo\n", 80);
        assert!(tasks.contains("[x] Done"), "{tasks}");
        assert!(tasks.contains("[ ] Todo"), "{tasks}");
    }

    #[test]
    fn code_fences_preserve_source_and_language() {
        let source = "```rust\nfn main() {\n    println!(\"**not bold**\");\n}\n```";
        let out = plain(source, 80);
        assert!(out.contains("rust"), "{out}");
        assert!(out.contains("  fn main() {"), "{out}");
        assert!(out.contains("      println!(\"**not bold**\");"), "{out}");
        assert!(
            out.contains("**"),
            "markdown inside a fence must stay literal: {out}"
        );

        let bash = plain("```bash\ncargo test\n```", 80);
        assert!(bash.contains("  cargo test"), "{bash}");

        let json = plain("```json\n{\"stream\": true}\n```", 80);
        assert!(json.contains("  {\"stream\": true}"), "{json}");

        let unknown = plain("```weirdlang\nkeep me\n```", 80);
        assert!(
            unknown.contains("weirdlang") && unknown.contains("  keep me"),
            "{unknown}"
        );

        let bare = plain("```\nplain fence\n```", 80);
        assert!(bare.contains("  plain fence"), "{bare}");
    }

    #[test]
    fn tables_align_and_fit_the_width() {
        let table = "| Method | Endpoint | Description |\n|---|---|---|\n| GET | `/v1/models` | List models |\n| POST | `/v1/chat/completions` | Create completion |\n";
        let out = plain(table, 80);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("Method"), "{out}");
        assert!(lines[1].contains('─'), "{out}");
        assert!(lines[2].starts_with("GET"), "{out}");
        assert!(!out.contains('|'), "pipes must not survive: {out}");

        let narrow = plain(table, 40);
        for line in narrow.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 40,
                "table line wider than terminal: {line:?}"
            );
        }

        let empty = plain("| A | B |\n|---|---|\n|  | x |\n", 60);
        assert!(empty.contains('x'), "{empty}");

        let right = plain("| R |\n|--:|\n| 1 |\n", 40);
        assert!(right.contains('1'), "{right}");
    }

    #[test]
    fn links_images_quotes_and_rules() {
        let link = plain("[Seavx Docs](https://example.com/docs)", 80);
        assert!(link.contains("Seavx Docs"), "{link}");
        assert!(link.contains("https://example.com/docs"), "{link}");

        let autolink = plain("<https://example.com>", 80);
        assert!(autolink.contains("https://example.com"), "{autolink}");
        assert_eq!(autolink.matches("https://example.com").count(), 1);

        let image = plain("![Architecture Diagram](./assets/architecture.png)", 80);
        assert!(image.contains("Architecture Diagram"), "{image}");
        assert!(image.contains("./assets/architecture.png"), "{image}");

        let quote = plain("> Quoted line\n", 80);
        assert!(quote.contains("│ Quoted line"), "{quote}");

        let note = plain("> [!NOTE]\n> Extra info.\n", 80);
        assert!(note.to_lowercase().contains("note"), "{note}");
        assert!(note.contains("Extra info."), "{note}");

        let rule = plain("---\n", 40);
        assert!(rule.contains(&"─".repeat(40)), "{rule}");
    }

    #[test]
    fn escapes_html_and_malformed_markdown_stay_readable() {
        let escaped = plain("literal \\*star\\* and \\_under\\_", 80);
        assert!(escaped.contains("*star*"), "{escaped}");
        assert!(escaped.contains("_under_"), "{escaped}");

        let symbols = plain("use * and _ and # and > and ` freely", 80);
        assert!(symbols.contains('*') && symbols.contains('#'), "{symbols}");

        let html = plain("<span>raw html</span>", 80);
        assert!(html.contains("raw html"), "{html}");

        // Unclosed fence while streaming must not panic or lose text.
        let unclosed = plain("```rust\nfn half() {", 80);
        assert!(unclosed.contains("fn half() {"), "{unclosed}");

        assert!(render("", 80).is_empty());
        assert!(render("   \n\n", 80).is_empty());
    }

    #[test]
    fn unicode_and_wide_characters_survive() {
        let text = "**Halo, dunia** — café ☕ dan 中文宽字符";
        let out = plain(text, 40);
        assert!(out.contains("Halo, dunia"), "{out}");
        assert!(out.contains("café ☕"), "{out}");
        assert!(out.contains("中文宽字符"), "{out}");

        let table = "| Nama | Nilai |\n|---|---|\n| café | 中文 |\n";
        let out = plain(table, 40);
        assert!(out.contains("café") && out.contains("中文"), "{out}");
    }

    #[test]
    fn streaming_chunks_split_across_delimiters_render_cleanly() {
        // Rendering partial prefixes must never panic; complete text must be clean.
        for cut in 0.."The **answer** is `42`.\n\n- item one\n".len() {
            let _ = render(&"The **answer** is `42`.\n\n- item one\n"[..cut], 60);
        }
        let done = plain("The **answer** is `42`.\n\n- item one\n", 60);
        assert!(!done.contains("**"), "{done}");
        assert!(done.contains("• item one"), "{done}");
    }
}
