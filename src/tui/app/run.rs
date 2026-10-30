use std::io::stdout;
use std::time::Duration;

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use tokio::sync::mpsc;

use super::{App, actions, keys, mouse, run_agent_worker};
use crate::agent::engine::AgentMode;
use crate::tui::event::Action;
use crate::tui::ui;

pub async fn start_tui(
    initial_prompt: Option<String>,
    loaded: Option<crate::session::LoadedSession>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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
            match event {
                CEvent::Key(key) => {
                    let _ = input_tx.send(Action::Key(key));
                }
                CEvent::Mouse(mouse) => {
                    let _ = input_tx.send(Action::Mouse(mouse));
                }
                _ => {}
            }
        }
    });

    // 60 FPS tick timer for smooth redraws
    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(16));
        loop {
            interval.tick().await;
            if tick_tx.send(Action::Tick).is_err() {
                break;
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

        if let Some(action) = rx.recv().await {
            match action {
                Action::Tick => {}
                Action::Mouse(mouse) => {
                    let size = terminal.size().unwrap_or_default();
                    let screen = Rect::new(0, 0, size.width, size.height);
                    mouse::handle(&mut app, mouse, screen);
                }
                Action::Key(key) => keys::handle(&mut app, key, &tx).await,
                action => actions::apply(&mut app, action, &tx),
            }
        }
    }

    if keyboard_enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
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
