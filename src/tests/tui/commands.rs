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
        Some(crate::tui::app::Segment::Notice(text))
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
fn every_registry_entry_parses_to_a_handled_command() {
    use crate::agent::engine::AgentMode;
    use crate::cli::config::ConfigAction;
    use crate::cli::memory::MemoryView;

    // One row per registry entry: the command the palette offers must parse to
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
        ("/new", SlashCommand::Clear),
        ("/help", SlashCommand::Help),
        ("/init", SlashCommand::Init),
        ("/doctor", SlashCommand::Doctor { offline: false }),
        (
            "/memory",
            SlashCommand::Memory {
                view: MemoryView::All,
            },
        ),
        ("/undo", SlashCommand::Undo),
        ("/sessions", SlashCommand::Sessions { index: None }),
        ("/resume", SlashCommand::Sessions { index: None }),
        ("/exit", SlashCommand::Exit),
    ];

    assert_eq!(
        COMMANDS.len(),
        expected.len(),
        "registry and parser drifted"
    );

    for (name, expected_command) in expected {
        assert!(
            COMMANDS.iter().any(|spec| spec.name == name),
            "{name} is missing from the registry"
        );
        let parsed = parse(name).unwrap_or_else(|| panic!("{name} does not parse"));
        assert!(
            !matches!(parsed, SlashCommand::Unknown(_)),
            "{name} parses to Unknown"
        );
        assert_eq!(parsed, expected_command, "{name} changed");
    }
}

#[test]
fn every_handled_command_appears_in_the_registry() {
    use crate::agent::engine::AgentMode;
    use crate::cli::config::ConfigAction;
    use crate::cli::memory::MemoryView;

    // Every variant the parser can produce from a name must be reachable from
    // the table; a variant with no row is a command nobody can type.
    let handled: Vec<&str> = vec![
        "/model",
        "/compact",
        "/config",
        "/plan",
        "/clear",
        "/new",
        "/help",
        "/init",
        "/doctor",
        "/memory",
        "/undo",
        "/sessions",
        "/resume",
        "/exit",
    ];
    for name in handled {
        let parsed = parse(name).expect("a registered name parses");
        assert!(
            !matches!(parsed, SlashCommand::Unknown(_)),
            "{name} is registered but not handled"
        );
    }

    // And the reverse: a name the table does not carry is unknown, not silent.
    for absent in ["/nope", "/clear now"] {
        assert!(
            !COMMANDS.iter().any(|spec| spec.name == absent),
            "{absent} should not be registered yet"
        );
    }
    assert!(matches!(parse("/nope"), Some(SlashCommand::Unknown(_))));

    // The variants are all constructible, so the list above cannot go stale.
    let _ = [
        SlashCommand::Model {
            target: None,
            effort: None,
        },
        SlashCommand::Compact { instructions: None },
        SlashCommand::Config {
            action: ConfigAction::Show,
        },
        SlashCommand::Plan {
            mode: AgentMode::Build,
        },
        SlashCommand::Clear,
        SlashCommand::Help,
        SlashCommand::Init,
        SlashCommand::Doctor { offline: true },
        SlashCommand::Memory {
            view: MemoryView::Path,
        },
        SlashCommand::Undo,
        SlashCommand::Exit,
    ];
}

#[test]
fn the_palette_is_the_table() {
    let palette = palette();
    let expected: Vec<(&str, &str)> = COMMANDS
        .iter()
        .map(|spec| (spec.name, spec.summary))
        .collect();
    assert_eq!(palette, expected, "the palette is the table, in order");

    let mut names: Vec<&str> = palette.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), palette.len(), "no duplicate command names");

    // Pinned order: the menu is what the user learns.
    let order: Vec<&str> = palette.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        order,
        vec![
            "/model",
            "/compact",
            "/config",
            "/plan",
            "/clear",
            "/new",
            "/help",
            "/init",
            "/doctor",
            "/memory",
            "/undo",
            "/sessions",
            "/resume",
            "/exit"
        ]
    );
}

#[test]
fn help_lists_every_palette_entry_with_its_usage() {
    let help = help_text();
    for spec in COMMANDS {
        assert!(help.contains(spec.name), "missing {} in {help}", spec.name);
        assert!(
            help.contains(spec.usage),
            "missing usage {} in {help}",
            spec.usage
        );
        assert!(
            help.contains(spec.summary),
            "missing summary {} in {help}",
            spec.summary
        );
    }
}

#[test]
fn notices_strip_control_characters() {
    let mut app = crate::tui::app::test_app();
    app.push_notice("ok\u{1b}[31m red\u{7}\nnext");
    let text = match app.segments.last() {
        Some(crate::tui::app::Segment::Notice(text)) => text,
        other => panic!("expected notice segment, got {other:?}"),
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
        Some(crate::tui::app::Segment::Notice(text)) => text,
        other => panic!("expected notice segment, got {other:?}"),
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
fn clear_and_new_are_the_same_command() {
    // One variant, one handler: the alias cannot drift from its target.
    assert_eq!(parse("/new"), parse("/clear"));
    assert_eq!(parse("/clear"), Some(SlashCommand::Clear));
    assert_eq!(
        COMMANDS
            .iter()
            .find(|spec| spec.name == "/new")
            .expect("a registry row")
            .id,
        CommandId::New
    );

    for name in ["/clear", "/new"] {
        let mut app = crate::tui::app::test_app();
        app.push_text("answer");
        app.session_id = Some(7);
        app.session_title = Some("title".into());
        app.session_context = Some("summary".into());
        assert_eq!(
            execute(&mut app, parse(name).expect("parses")),
            CommandEffect::Handled
        );
        assert!(app.segments.is_empty(), "{name} clears the transcript");
        assert_eq!(app.session_id, None, "{name} starts a new session");
        assert!(app.session_title.is_none(), "{name} drops the title");
        assert!(
            app.session_context.is_none(),
            "{name} clears the compaction context"
        );
    }

    // Idle-only, with the same refusal for both spellings.
    for name in ["/clear", "/new"] {
        let mut app = crate::tui::app::test_app();
        app.status = AgentState::Streaming;
        app.session_id = Some(9);
        assert_eq!(
            execute(&mut app, parse(name).expect("parses")),
            CommandEffect::Handled
        );
        assert!(app.segments.iter().any(|segment| matches!(
            segment,
            crate::tui::app::Segment::Notice(text) if text.contains("Agent is busy")
        )));
        assert_eq!(app.session_id, Some(9), "{name} changes nothing while busy");
    }
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

#[test]
fn undo_is_registered_and_dispatchable() {
    let palette: Vec<&str> = palette().into_iter().map(|(name, _)| name).collect();
    assert!(palette.contains(&"/undo"), "{palette:?}");
    assert_eq!(parse("/undo"), Some(SlashCommand::Undo));

    let mut app = crate::tui::app::test_app();
    assert_eq!(
        execute(&mut app, SlashCommand::Undo),
        CommandEffect::RunUndo
    );
}

#[test]
fn undo_is_refused_while_busy() {
    let mut app = crate::tui::app::test_app();
    app.status = AgentState::Streaming;

    let effect = execute(&mut app, SlashCommand::Undo);

    assert_eq!(effect, CommandEffect::Handled);
    assert!(
        app.segments.iter().any(|segment| matches!(
            segment,
            crate::tui::app::Segment::Notice(text)
                if text.contains("Agent is busy, try again when it is idle.")
        )),
        "the refusal is explained"
    );
}

fn fixture_store() -> (tempfile::TempDir, crate::session::SessionStore, String) {
    let temp = tempfile::tempdir().unwrap();
    let store = crate::session::SessionStore::open_at(&temp.path().join("sessions.db")).unwrap();
    let workspace = "test-workspace".to_string();
    for prompt in ["first prompt", "second prompt"] {
        crate::session::prepare_turn(&store, &workspace, "fake-model", None, prompt).unwrap();
    }
    (temp, store, workspace)
}

#[test]
fn sessions_opens_the_picker_and_resumes_a_number() {
    assert_eq!(
        parse("/sessions"),
        Some(SlashCommand::Sessions { index: None })
    );
    assert_eq!(
        parse("/sessions 2"),
        Some(SlashCommand::Sessions { index: Some(2) })
    );
    // The list is the picker now: no number means "let me choose".
    let mut app = crate::tui::app::test_app();
    assert_eq!(
        execute(&mut app, SlashCommand::Sessions { index: None }),
        CommandEffect::OpenSessionPicker
    );
    assert_eq!(
        execute(&mut app, SlashCommand::Sessions { index: Some(2) }),
        CommandEffect::RunSessions { number: 2 }
    );
    // `/resume` is the alias: same command, same arguments, same effect.
    assert_eq!(
        parse("/resume"),
        Some(SlashCommand::Sessions { index: None })
    );
    assert_eq!(
        parse("/resume 2"),
        Some(SlashCommand::Sessions { index: Some(2) })
    );
    assert!(matches!(parse("/resume 0"), Some(SlashCommand::Unknown(_))));
    assert!(matches!(
        parse("/sessions 0"),
        Some(SlashCommand::Unknown(_))
    ));
    assert!(matches!(
        parse("/sessions nope"),
        Some(SlashCommand::Unknown(_))
    ));

    let (_temp, store, workspace) = fixture_store();
    let listing = crate::cli::sessions::numbered_list_text(&store, &workspace).expect("a listing");
    assert!(listing.contains("/sessions <number>"), "{listing}");
    assert!(listing.contains("first prompt"), "{listing}");
    assert!(listing.contains("second prompt"), "{listing}");
    // Newest first: number 1 is the session created last.
    let lines: Vec<&str> = listing.lines().collect();
    // Numbered row, then its title on the following line.
    assert!(lines[1].trim_start().starts_with("1  svx-"), "{listing}");
    assert!(lines[2].trim().contains("second prompt"), "{listing}");
    assert!(lines[3].trim_start().starts_with("2  svx-"), "{listing}");
    assert!(lines[4].trim().contains("first prompt"), "{listing}");

    let first = crate::cli::sessions::resume_by_number(&store, &workspace, 1).expect("resumes");
    assert_eq!(first.title.as_deref(), Some("second prompt"));
    let second = crate::cli::sessions::resume_by_number(&store, &workspace, 2).expect("resumes");
    assert_eq!(second.title.as_deref(), Some("first prompt"));

    let missing = crate::cli::sessions::resume_by_number(&store, &workspace, 9).unwrap_err();
    assert!(missing.contains("/sessions"), "{missing}");
}

#[test]
fn resuming_a_read_only_session_is_refused_with_the_reason() {
    let (_temp, store, workspace) = fixture_store();
    store
        .connection()
        .execute("UPDATE sessions SET payload_format = 'genai-0.6'", [])
        .unwrap();

    let error = crate::cli::sessions::resume_by_number(&store, &workspace, 1).unwrap_err();
    assert!(error.contains("read-only"), "{error}");
    assert!(
        error.contains("payload format"),
        "the reason travels with the refusal: {error}"
    );
    // Nothing was opened: the message names the session, the transcript stays out.
    assert!(!error.contains("User:"), "{error}");
}

#[test]
fn the_readme_command_table_matches_the_registry() {
    let readme = include_str!("../../../README.md");
    let mut names: Vec<&str> = Vec::new();
    for line in readme.lines() {
        let Some(cell) = line.strip_prefix("| `") else {
            continue;
        };
        let Some(name) = cell.split([' ', '`']).next() else {
            continue;
        };
        if name.starts_with('/') && !names.contains(&name) {
            names.push(name);
        }
    }

    let registered: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names, registered,
        "the README command table and the registry drifted; the palette is generated from the registry"
    );
}
