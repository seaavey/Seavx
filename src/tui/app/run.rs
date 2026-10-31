use std::io::stdout;
use std::time::Duration;

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as CEvent, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use tokio::sync::mpsc;

use super::{App, actions, keys, mouse, paste, run_agent_worker};
use crate::agent::engine::AgentMode;
use crate::tui::app::ModalSelection;
use crate::tui::event::{Action, AgentState};
use crate::tui::ui;

pub(super) fn redraw_interval(app: &App) -> Duration {
    if app.status != AgentState::Idle
        || app.refreshing_models
        || matches!(
            app.modal.as_ref(),
            Some(ModalSelection::Effort { options, selected, .. })
                if options.get(*selected) == Some(&"max")
        )
    {
        Duration::from_millis(16)
    } else if let Some((_, instant)) = &app.toast {
        let remaining = Duration::from_secs(3).saturating_sub(instant.elapsed());
        if remaining.is_zero() {
            Duration::from_secs(1)
        } else {
            remaining
        }
    } else {
        Duration::from_secs(1)
    }
}

/// The events the loop acts on. Paste is data: with bracketed paste enabled the
/// terminal hands over one string instead of a keystroke per character, which
/// is what stopped a pasted newline from submitting a turn.
pub(super) fn translate(event: CEvent) -> Option<Action> {
    match event {
        // With keyboard enhancement on, one press arrives three times. Acting on
        // the release would run the key twice: paste twice, toggle twice.
        CEvent::Key(key) if key.kind != KeyEventKind::Release => Some(Action::Key(key)),
        CEvent::Mouse(mouse) => Some(Action::Mouse(mouse)),
        CEvent::Paste(text) => Some(Action::Paste(text)),
        _ => None,
    }
}

pub async fn start_tui(
    initial_prompt: Option<String>,
    loaded: Option<crate::session::LoadedSession>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();

    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();

    // Terminal event loop (Keyboard + Mouse via EventStream)
    let input_tx = tx.clone();
    tokio::spawn(async move {
        use crossterm::event::EventStream;
        use futures::StreamExt;

        let mut reader = EventStream::new();
        while let Some(Ok(event)) = reader.next().await {
            if let Some(action) = translate(event) {
                let _ = input_tx.send(action);
            }
        }
    });

    let mut app = App::new();

    // One store handle for the run; a store that cannot open degrades to one
    // notice and an unpersisted session instead of failing the TUI.
    app.open_store(crate::session::SessionStore::open());

    // `seavx sessions resume` loads before the loop starts, so the indicator is
    // the transcript itself: no async restore to wait for.
    if let Some(session) = loaded {
        app.load_session(session);
    }

    // Trigger initial prompt if provided via CLI args
    if let Some(prompt) = initial_prompt
        && let Some(request) = app.submit_turn(&prompt)
    {
        let is_first_turn = app.session_title.is_none();
        run_agent_worker(request, tx.clone(), is_first_turn, AgentMode::Build, None).await;
    }

    // A batch the last process never finished is reported once, loudly, and
    // only undone when the user asks for it.
    for batch in crate::checkpoint::recover_pending(&app.root).unwrap_or_default() {
        app.push_notice(batch.render());
    }

    while !app.should_quit {
        // Ghost suggestions must fit inside the prompt box, so track resize.
        app.terminal_width = terminal
            .size()
            .map(|area| area.width)
            .unwrap_or(app.terminal_width);
        // Pick up prompts written to `.seavx/svx_history` while running.
        app.sync_history();
        terminal.draw(|frame| ui::render(&app, frame))?;

        tokio::select! {
            action = rx.recv() => {
                if let Some(action) = action {
                    match action {
                        Action::Mouse(mouse) => {
                            let size = terminal.size().unwrap_or_default();
                            let screen = Rect::new(0, 0, size.width, size.height);
                            mouse::handle(&mut app, mouse, screen);
                        }
                        Action::Key(key) => keys::handle(&mut app, key, &tx).await,
                        Action::Paste(text) => paste::handle(&mut app, &text),
                        action => actions::apply(&mut app, action, &tx),
                    }
                }
            }
            _ = tokio::time::sleep(redraw_interval(&app)) => {}
        }
    }

    if keyboard_enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    // Clear debug.log on exit if reasoning effort is enabled
    let config = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
    if let Some(ref effort) = config.reasoning_effort
        && !effort.trim().is_empty()
        && effort.trim().to_lowercase() != "none"
    {
        let log_path = std::path::PathBuf::from(".seavx/debug.log");
        if log_path.exists() {
            let _ = std::fs::write(&log_path, "");
        }
    }

    Ok(())
}
