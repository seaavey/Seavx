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
