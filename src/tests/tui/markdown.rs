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
