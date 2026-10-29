use super::*;
use crate::tui::event::AgentState;

#[test]
fn parses_existing_commands_and_arguments() {
    assert_eq!(parse("/help"), Some(SlashCommand::Help));
    assert_eq!(parse("/clear"), Some(SlashCommand::Clear));
    assert_eq!(parse("/exit"), Some(SlashCommand::Exit));
    assert_eq!(
        parse("/model"),
        Some(SlashCommand::Model {
            target: None,
            effort: None
        })
    );
    assert_eq!(
        parse("/model rv/model high"),
        Some(SlashCommand::Model {
            target: Some("rv/model".into()),
            effort: Some("high".into()),
        })
    );
    assert_eq!(parse("hello /help"), None);
    assert_eq!(parse("/"), None);
    assert_eq!(parse("/nope"), Some(SlashCommand::Unknown("/nope".into())));
}

#[test]
fn busy_app_rejects_clear_but_allows_help_and_exit() {
    let mut app = crate::tui::app::test_app();
    app.status = AgentState::Streaming;
    assert!(matches!(
        execute(&mut app, SlashCommand::Clear),
        CommandEffect::Handled
    ));
    assert!(matches!(
        app.segments.last(),
        Some(crate::tui::app::Segment::Text(text))
            if text.contains("Agent is busy, try again when it is idle.")
    ));
    assert!(matches!(
        execute(&mut app, SlashCommand::Help),
        CommandEffect::Handled
    ));
    assert!(matches!(
        execute(&mut app, SlashCommand::Exit),
        CommandEffect::Quit
    ));
}

#[test]
fn every_palette_entry_parses_to_a_handled_command() {
    use crate::agent::engine::AgentMode;
    use crate::cli::config::ConfigAction;
    use crate::cli::memory::MemoryView;

    // One row per palette entry: the command the palette offers must parse to
    // exactly this variant, so autocomplete can never suggest a dead command.
    let expected = [
        (
            "/model",
            SlashCommand::Model {
                target: None,
                effort: None,
            },
        ),
        ("/compact", SlashCommand::Compact { instructions: None }),
        (
            "/config",
            SlashCommand::Config {
                action: ConfigAction::Show,
            },
        ),
        (
            "/plan",
            SlashCommand::Plan {
                mode: AgentMode::Plan,
            },
        ),
        ("/clear", SlashCommand::Clear),
        ("/help", SlashCommand::Help),
        ("/init", SlashCommand::Init),
        ("/doctor", SlashCommand::Doctor { offline: false }),
        (
            "/memory",
            SlashCommand::Memory {
                view: MemoryView::All,
            },
        ),
        ("/exit", SlashCommand::Exit),
    ];

    let palette: Vec<&str> = SLASH_COMMANDS.iter().map(|(name, _)| *name).collect();
    let pinned: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
    assert_eq!(palette, pinned, "palette and parser table drifted");

    for (name, expected_command) in expected {
        let parsed = parse(name).unwrap_or_else(|| panic!("{name} does not parse"));
        assert!(
            !matches!(parsed, SlashCommand::Unknown(_)),
            "{name} parses to Unknown"
        );
        assert_eq!(parsed, expected_command, "{name} changed");
    }
}

#[test]
fn help_lists_every_palette_entry() {
    let help = help_text();
    for (command, _) in crate::tui::app::SLASH_COMMANDS {
        assert!(help.contains(command), "missing {command} in {help}");
    }
}

#[test]
fn notices_strip_control_characters() {
    let mut app = crate::tui::app::test_app();
    app.push_notice("ok\u{1b}[31m red\u{7}\nnext");
    let text = match app.segments.last() {
        Some(crate::tui::app::Segment::Text(text)) => text,
        other => panic!("expected text segment, got {other:?}"),
    };
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{7}'));
    assert!(text.contains("next"));
}

#[test]
fn parses_compact_with_focus_instructions() {
    assert_eq!(
        parse("/compact"),
        Some(SlashCommand::Compact { instructions: None })
    );
    assert_eq!(
        parse("/compact keep file paths"),
        Some(SlashCommand::Compact {
            instructions: Some("keep file paths".into())
        })
    );
}

#[test]
fn parses_doctor_offline_flag() {
    assert_eq!(
        parse("/doctor"),
        Some(SlashCommand::Doctor { offline: false })
    );
    assert_eq!(
        parse("/doctor --offline"),
        Some(SlashCommand::Doctor { offline: true })
    );
    assert_eq!(
        parse("/doctor nope"),
        Some(SlashCommand::Unknown("/doctor nope".into()))
    );
}

#[test]
fn parses_config_flags_and_values_with_spaces() {
    assert_eq!(
        parse("/config set --global model x"),
        Some(SlashCommand::Config {
            action: crate::cli::config::ConfigAction::Set {
                key: "model".into(),
                value: "x".into(),
                global: true,
            },
        })
    );
    assert_eq!(
        parse("/config set system_prompt be nice to me"),
        Some(SlashCommand::Config {
            action: crate::cli::config::ConfigAction::Set {
                key: "system_prompt".into(),
                value: "be nice to me".into(),
                global: false,
            },
        })
    );
    assert_eq!(parse("/init"), Some(SlashCommand::Init));
}

#[test]
fn config_rejects_api_key_writes() {
    let mut app = crate::tui::app::test_app();
    execute(
        &mut app,
        SlashCommand::Config {
            action: crate::cli::config::ConfigAction::Set {
                key: "api_key".into(),
                value: "sr-live-12345678".into(),
                global: false,
            },
        },
    );
    let text = match app.segments.last() {
        Some(crate::tui::app::Segment::Text(text)) => text,
        other => panic!("expected text segment, got {other:?}"),
    };
    assert!(text.contains("setup providers"));
    assert!(!text.contains("sr-live-12345678"));
}

#[test]
fn parses_memory_views() {
    assert_eq!(
        parse("/memory"),
        Some(SlashCommand::Memory {
            view: crate::cli::memory::MemoryView::All
        })
    );
    assert_eq!(
        parse("/memory user"),
        Some(SlashCommand::Memory {
            view: crate::cli::memory::MemoryView::User
        })
    );
    assert_eq!(
        parse("/memory path"),
        Some(SlashCommand::Memory {
            view: crate::cli::memory::MemoryView::Path
        })
    );
    assert_eq!(
        parse("/memory nope"),
        Some(SlashCommand::Unknown("/memory nope".into()))
    );
}

#[test]
fn clear_resets_the_transcript_and_title() {
    let mut app = crate::tui::app::test_app();
    app.push_text("answer");
    app.session_title = Some("title".into());
    assert!(matches!(
        execute(&mut app, SlashCommand::Clear),
        CommandEffect::Handled
    ));
    assert!(app.segments.is_empty());
    assert!(app.session_title.is_none());
}

#[test]
fn parses_plan_forms() {
    assert_eq!(
        parse("/plan"),
        Some(SlashCommand::Plan {
            mode: crate::agent::engine::AgentMode::Plan
        })
    );
    assert_eq!(
        parse("/plan on"),
        Some(SlashCommand::Plan {
            mode: crate::agent::engine::AgentMode::Plan
        })
    );
    assert_eq!(
        parse("/plan off"),
        Some(SlashCommand::Plan {
            mode: crate::agent::engine::AgentMode::Build
        })
    );
    assert_eq!(
        parse("/plan nope"),
        Some(SlashCommand::Unknown("/plan nope".into()))
    );
    assert_eq!(parse("plan"), None);
}
