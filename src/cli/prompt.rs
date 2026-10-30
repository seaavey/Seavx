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
}
