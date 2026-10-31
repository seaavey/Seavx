//! Terminal input helpers shared by the interactive commands.

/// Read one stdin line; an empty answer keeps `current`.
pub fn line(label: &str, current: &str) -> anyhow::Result<String> {
    use std::io::{self, Write};
    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{current}]: ");
    }
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    Ok(if input.is_empty() {
        current.to_string()
    } else {
        input.to_string()
    })
}

/// Masked variant of `line`, for values that must not land in scrollback.
pub fn secret(label: &str, current: &str) -> anyhow::Result<String> {
    use std::io::{self, IsTerminal, Write};

    if current.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [****]: ");
    }
    io::stdout().flush()?;

    let input = if io::stdin().is_terminal() {
        read_masked_line()?
    } else {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        println!();
        line.trim_end().to_string()
    };

    Ok(if input.is_empty() {
        current.to_string()
    } else {
        input
    })
}

/// A checkbox list over `items` (`name`, one-line description).
///
/// Returns the marked indices, or the row under the cursor when nothing is
/// marked; `None` on Esc/q/Ctrl-C. `*global` flips with `d`, and the header
/// always names the root Enter would write to. Only call this on a terminal:
/// raw mode needs one, and a piped run must keep its non-interactive path.
pub fn pick_skills(
    title: &str,
    items: &[(String, String)],
    global: &mut bool,
) -> Option<Vec<usize>> {
    use crossterm::cursor::{Hide, Show};
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use crossterm::queue;
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io;

    if items.is_empty() {
        return None;
    }
    enable_raw_mode().ok()?;
    let mut out = io::stdout();
    let _ = queue!(out, Hide);

    let mut cursor = 0usize;
    let mut marked = vec![false; items.len()];
    let mut drawn = 0u16;
    let result = loop {
        let block = picker_block(title, items, &marked, cursor, *global);
        if draw(&mut out, &block, &mut drawn).is_err() {
            break None;
        }
        let key = match event::read() {
            Ok(Event::Key(key)) => key,
            Ok(_) => continue,
            Err(_) => break None,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(items.len() - 1),
            KeyCode::Char(' ') => marked[cursor] = !marked[cursor],
            KeyCode::Char('a') => marked.fill(true),
            KeyCode::Char('n') => marked.fill(false),
            KeyCode::Char('d') => *global = !*global,
            KeyCode::Enter => break Some(marked_indices(&marked, cursor)),
            KeyCode::Esc | KeyCode::Char('q') => break None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
            _ => {}
        }
    };

    let _ = queue!(out, Show);
    let _ = disable_raw_mode();
    println!();
    result
}

/// A single-select list, with a search box once the list needs more than one
/// screen. Used for the model list and the reasoning-effort list, so both
/// answer to the same keys.
///
/// `current` preselects a row, so Enter with no navigation keeps what the
/// config already names. Returns `None` on Esc/Ctrl-C. Only call this on a
/// terminal: a piped run keeps its typed prompt. Typing filters, so `j`/`k`
/// cannot be shortcuts here — arrows move.
pub fn pick(title: &str, items: &[String], current: Option<&str>) -> Option<String> {
    use crossterm::cursor::{Hide, Show};
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use crossterm::queue;
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io;

    if items.is_empty() {
        return None;
    }
    let searchable = items.len() > PICK_ROWS;
    enable_raw_mode().ok()?;
    let mut out = io::stdout();
    let _ = queue!(out, Hide);

    let mut query = String::new();
    let mut cursor = current
        .and_then(|current| items.iter().position(|item| item == current))
        .unwrap_or(0);
    let mut drawn = 0u16;
    let result = loop {
        let rows = filter_items(items, &query);
        // A narrowed search can leave the cursor past the last row.
        cursor = cursor.min(rows.len().saturating_sub(1));
        let block = select_block(title, items, &rows, cursor, &query, searchable);
        if draw(&mut out, &block, &mut drawn).is_err() {
            break None;
        }
        let key = match event::read() {
            Ok(Event::Key(key)) => key,
            Ok(_) => continue,
            Err(_) => break None,
        };
        match key.code {
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Enter if !rows.is_empty() => {
                break rows.get(cursor).map(|index| items[*index].clone());
            }
            KeyCode::Esc => break None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
            _ => {
                // A short list has no search box, so nothing here is text.
                if searchable && let Some(next) = edit_search(&query, &key) {
                    query = next;
                    cursor = 0;
                }
            }
        }
    };

    let _ = queue!(out, Show);
    let _ = disable_raw_mode();
    println!();
    result
}

/// Rows a picker shows at once; the cursor scrolls the window, so a list of
/// ninety models never draws past the terminal. A list at or under this height
/// needs no search box.
const PICK_ROWS: usize = 10;

/// Indices into `items` whose name contains `query`, case-insensitively.
fn filter_items(items: &[String], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| query.is_empty() || item.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// The slice of rows to draw: at most `height`, always containing `cursor`.
fn visible_window(cursor: usize, len: usize, height: usize) -> (usize, usize) {
    if len <= height {
        return (0, len);
    }
    let start = cursor.saturating_sub(height / 2).min(len - height);
    (start, start + height)
}

/// The search box: a printable key appends, Backspace deletes one character.
/// `None` means the key is not text, so the caller keeps it for navigation.
fn edit_search(query: &str, key: &crossterm::event::KeyEvent) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Backspace => {
            let mut trimmed = query.to_string();
            trimmed.pop();
            Some(trimmed)
        }
        KeyCode::Char(character) => Some(format!("{query}{character}")),
        _ => None,
    }
}

fn select_block(
    title: &str,
    items: &[String],
    rows: &[usize],
    cursor: usize,
    query: &str,
    searchable: bool,
) -> Vec<String> {
    let height = rows.len().min(PICK_ROWS);
    let (start, end) = visible_window(cursor, rows.len(), PICK_ROWS);
    // Fixed height per mode: a shrinking block would leave the previous frame's
    // rows on screen, because `draw` only rewrites the lines it is given.
    let mut block = vec![format!(" {title} · {}/{}", rows.len(), items.len())];
    if searchable {
        block.push(format!(" search: {query}"));
        block.push(match start {
            0 => String::new(),
            start => format!(" ↑ {start} more"),
        });
    }
    for (offset, index) in rows[start..end].iter().enumerate() {
        let marker = if start + offset == cursor {
            "❯ "
        } else {
            "  "
        };
        let row = format!("{marker}{}", items[*index]);
        block.push(if start + offset == cursor {
            format!("\x1b[7m{row}\x1b[0m")
        } else {
            row
        });
    }
    for _ in height..PICK_ROWS {
        if searchable {
            block.push(String::new());
        }
    }
    if searchable {
        block.push(match end < rows.len() {
            true => format!(" ↓ {} more", rows.len() - end),
            false => String::new(),
        });
        block.push(" ↑↓ move · type to search · enter select · esc cancel".to_string());
    } else {
        block.push(" ↑↓ move · enter select · esc cancel".to_string());
    }
    block
}

/// The picked set: the marked rows, or the cursor row when none is marked.
fn marked_indices(marked: &[bool], cursor: usize) -> Vec<usize> {
    let picked: Vec<usize> = marked
        .iter()
        .enumerate()
        .filter(|(_, marked)| **marked)
        .map(|(index, _)| index)
        .collect();
    if picked.is_empty() {
        vec![cursor]
    } else {
        picked
    }
}

fn destination_label(global: bool) -> &'static str {
    if global {
        "global (~/.agents/skills)"
    } else {
        "local (.seavx/skills)"
    }
}

fn picker_block(
    title: &str,
    items: &[(String, String)],
    marked: &[bool],
    cursor: usize,
    global: bool,
) -> Vec<String> {
    let selected = marked.iter().filter(|marked| **marked).count();
    let width = items
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut block = vec![
        format!(" {title} {selected} of {} skills", items.len()),
        format!(" destination: {}   [d] switch", destination_label(global)),
    ];
    for (index, (name, description)) in items.iter().enumerate() {
        let check = if marked[index] { 'x' } else { ' ' };
        let row = format!(" [{check}] {name:<width$}  {description}");
        // Reverse video marks the row the keys act on; escapes add no width.
        block.push(if index == cursor {
            format!("\x1b[7m{row}\x1b[0m")
        } else {
            row
        });
    }
    block.push(
        " space toggle · d destination · a all · n none · enter install · esc cancel".to_string(),
    );
    block
}

/// Rewrite the whole block in place instead of appending a second copy.
// shortcut: a list taller than the terminal scrolls this out of step; paginate
// when repositories start carrying more skills than a screen.
fn draw(out: &mut impl std::io::Write, block: &[String], drawn: &mut u16) -> std::io::Result<()> {
    use crossterm::cursor::{MoveToColumn, MoveUp};
    use crossterm::queue;
    use crossterm::style::Print;
    use crossterm::terminal::{Clear, ClearType};

    if *drawn > 0 {
        queue!(out, MoveUp(*drawn))?;
    }
    for line in block {
        queue!(
            out,
            MoveToColumn(0),
            Clear(ClearType::UntilNewLine),
            Print(line),
            Print("\r\n")
        )?;
    }
    *drawn = block.len() as u16;
    out.flush()
}

fn read_masked_line() -> anyhow::Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use std::io::{self, Write};

    crossterm::terminal::enable_raw_mode()?;
    let mut value = String::new();
    let mut out = io::stdout();

    let result = loop {
        match event::read() {
            Ok(Event::Key(key)) => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Char(c) => {
                    value.push(c);
                    let _ = out.write_all(b"*");
                }
                KeyCode::Backspace if value.pop().is_some() => {
                    let _ = out.write_all(b"\x08 \x08");
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(anyhow::anyhow!(e)),
        }
        let _ = out.flush();
    };

    crossterm::terminal::disable_raw_mode()?;
    println!();
    result?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw-mode loop needs a tty; its key handling reduces to this.
    #[test]
    fn the_pick_is_the_marked_rows_or_the_cursor() {
        assert_eq!(marked_indices(&[false, true, false], 2), vec![1]);
        assert_eq!(marked_indices(&[false, false, false], 1), vec![1]);
        assert_eq!(marked_indices(&[true, false, true], 0), vec![0, 2]);
    }

    #[test]
    fn the_header_names_the_root_enter_writes_to() {
        assert_eq!(destination_label(false), "local (.seavx/skills)");
        assert_eq!(destination_label(true), "global (~/.agents/skills)");
    }

    fn model_list() -> Vec<String> {
        [
            "openai/gpt-4o-mini",
            "anthropic/claude-sonnet-4",
            "google/gemini-2.0-flash",
        ]
        .iter()
        .map(|model| model.to_string())
        .collect()
    }

    #[test]
    fn the_search_matches_a_case_insensitive_substring() {
        let models = model_list();
        assert_eq!(filter_items(&models, ""), vec![0, 1, 2]);
        assert_eq!(filter_items(&models, "CLAUDE"), vec![1]);
        assert_eq!(filter_items(&models, "4o"), vec![0]);
        assert!(filter_items(&models, "nope").is_empty());
    }

    #[test]
    fn the_window_keeps_the_cursor_visible_within_its_height() {
        assert_eq!(visible_window(0, 3, 10), (0, 3));
        assert_eq!(visible_window(0, 90, 10), (0, 10));
        assert_eq!(visible_window(45, 90, 10), (40, 50));
        assert_eq!(visible_window(89, 90, 10), (80, 90));
    }

    #[test]
    fn a_picker_block_keeps_one_height_wherever_the_cursor_sits() {
        let models: Vec<String> = (0..12).map(|index| format!("m{index}")).collect();
        let rows = filter_items(&models, "");
        let top = select_block("Model", &models, &rows, 0, "", true);
        let deep = select_block("Model", &models, &rows, 11, "", true);
        assert_eq!(top.len(), deep.len(), "a shrinking block leaves stale rows");
        assert!(top[0].contains("Model · 12/12"));
        assert!(top.iter().any(|line| line.contains(" search: ")));
        assert!(top.iter().any(|line| line.contains("↓ 2 more")));
        assert!(deep.iter().any(|line| line.contains("↑ 2 more")));
        assert!(deep.iter().any(|line| line.contains("\x1b[7m❯ m11")));
    }

    #[test]
    fn a_short_list_has_no_search_box_and_marks_exactly_one_row() {
        let levels: Vec<String> = ["none", "minimal", "medium", "high", "highx", "max"]
            .iter()
            .map(|level| (*level).to_string())
            .collect();
        let rows = filter_items(&levels, "");
        let block = select_block("Reasoning effort", &levels, &rows, 2, "", false);
        assert!(block[0].contains("Reasoning effort · 6/6"));
        assert!(
            !block.iter().any(|line| line.contains("search:")),
            "six options do not need a search box"
        );
        assert_eq!(block.iter().filter(|line| line.contains('❯')).count(), 1);
        assert!(block.iter().any(|line| line.contains("❯ medium")));
    }
}
