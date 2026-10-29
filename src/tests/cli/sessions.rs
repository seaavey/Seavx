use crate::cli::sessions::{
    export_text, list_text, parse_resume_number, relative_age, resume_target, session_rows,
    show_text,
};
use crate::session::{MessageStatus, SessionStore};

/// Two workspaces, one answered session each, same-second timestamps.
fn fixture() -> (tempfile::TempDir, SessionStore) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SessionStore::open_at(&dir.path().join("sessions.db")).unwrap();
    for (workspace, prompt) in [("/x", "older prompt"), ("/y", "elsewhere")] {
        let id = store
            .create(workspace, Some("fake-model"), Some(prompt))
            .unwrap();
        store.record_user(id, prompt).unwrap();
        store
            .record_message(
                id,
                &genai::chat::ChatMessage::assistant("answer"),
                MessageStatus::Complete,
            )
            .unwrap();
    }
    (dir, store)
}

#[test]
fn list_renders_columns_and_an_empty_message() {
    let (_dir, store) = fixture();

    let text = list_text(&store, "/x").unwrap();
    let header = text.lines().next().unwrap();
    assert!(
        header.starts_with("slug"),
        "columns start with the id: {text}"
    );
    for column in ["age", "msgs", "state", "title"] {
        assert!(header.contains(column), "{column} is missing: {text}");
    }
    assert!(text.contains("svx-"), "rows carry the slug: {text}");
    assert!(text.contains("older prompt"), "{text}");
    assert!(text.contains("idle"), "{text}");
    assert!(text.contains('2'), "the message count: {text}");
    assert!(!text.contains("elsewhere"), "workspace filtered: {text}");

    assert_eq!(
        list_text(&store, "/nowhere").unwrap(),
        "No sessions yet for /nowhere.\n"
    );

    let now = 1_000_000;
    assert_eq!(relative_age(now, now), "now");
    assert_eq!(relative_age(now - 59, now), "now");
    assert_eq!(relative_age(now - 300, now), "5m");
    assert_eq!(relative_age(now - 3 * 3_600, now), "3h");
    assert_eq!(relative_age(now - 2 * 86_400, now), "2d");
}

#[test]
fn unknown_id_names_the_list_command() {
    let (_dir, store) = fixture();

    let error = show_text(&store, "/x", 999).unwrap_err();
    assert_eq!(error, "Unknown session 999. Run 'seavx sessions list'.");
    let error = export_text(&store, "/x", 999).unwrap_err();
    assert_eq!(error, "Unknown session 999. Run 'seavx sessions list'.");
}

#[test]
fn resume_refuses_a_session_from_another_workspace() {
    let (_dir, store) = fixture();
    let id = store.list("/x").unwrap()[0].id;

    let error = resume_target(&store, "/y", id).unwrap_err();
    assert!(error.contains("/x"), "the error names the owner: {error}");

    let loaded = resume_target(&store, "/x", id).unwrap();
    assert_eq!(loaded.id, id);
    assert_eq!(loaded.messages.len(), 2);
}

#[test]
fn show_prints_the_header_and_the_transcript() {
    let (_dir, store) = fixture();
    let (id, slug) = {
        let summary = &store.list("/x").unwrap()[0];
        (summary.id, summary.slug.clone())
    };

    let text = show_text(&store, "/x", id).unwrap();
    assert!(text.contains(&format!("Session {slug}")), "{text}");
    assert!(text.contains("older prompt"), "{text}");
    assert!(text.contains("payload format: genai-chat-v1"), "{text}");
    assert!(text.contains("answer"), "{text}");
}

#[test]
fn show_and_export_refuse_a_session_from_another_workspace() {
    let (_dir, store) = fixture();
    let id = store.list("/x").unwrap()[0].id;

    for error in [
        show_text(&store, "/y", id).unwrap_err(),
        export_text(&store, "/y", id).unwrap_err(),
    ] {
        assert!(error.contains("/x"), "the error names the owner: {error}");
    }

    assert!(show_text(&store, "/x", id).is_ok());
    assert!(export_text(&store, "/x", id).is_ok());
}

#[test]
fn the_resume_prompt_accepts_only_a_number_it_printed() {
    assert_eq!(parse_resume_number("1", 2), Ok(1));
    assert_eq!(
        parse_resume_number(" 2 ", 2),
        Ok(2),
        "surrounding space is trimmed"
    );

    for rejected in ["0", "3", "two", ""] {
        let error = parse_resume_number(rejected, 2).unwrap_err();
        assert!(error.contains("1 to 2"), "{rejected:?} -> {error}");
    }
    assert_eq!(
        parse_resume_number("1", 0).unwrap_err(),
        "no sessions to resume"
    );
}

#[test]
fn picker_rows_are_numbered_newest_first_and_carry_the_slug() {
    let (_dir, store) = fixture();

    let rows = session_rows(&store, "/x").unwrap();
    assert_eq!(rows.len(), 1, "only this workspace: {rows:?}");
    assert!(rows[0].starts_with("1  svx-"), "{}", rows[0]);
    assert!(rows[0].contains("older prompt"), "{}", rows[0]);
    assert!(!rows[0].contains("elsewhere"), "{}", rows[0]);

    assert!(session_rows(&store, "/nowhere").unwrap().is_empty());
}
