use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("seavx-history-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn append_then_load_round_trips() {
    let dir = temp_dir("roundtrip");
    let mut history = ChatHistory::load(&dir);
    assert!(history.entries().is_empty(), "missing file starts empty");

    history.append("adrian");
    history.append("hello\nmultiline");
    assert_eq!(history.entries(), ["adrian", "hello\nmultiline"]);
}

#[test]
fn repeated_newest_prompt_is_not_stored_twice() {
    let dir = temp_dir("dedupe");
    let mut history = ChatHistory::load(&dir);
    history.append("adrian");
    history.append("adrian");
    assert_eq!(history.entries(), ["adrian"]);
}

#[test]
fn appends_after_a_file_without_a_trailing_newline() {
    let dir = temp_dir("no-newline");
    let mut history = ChatHistory::load(&dir);
    std::fs::write(&history.path, "adrian").unwrap();
    history.append("hello");
    assert_eq!(history.entries(), ["adrian", "hello"]);
}

#[test]
fn trimming_keeps_the_last_entries_only() {
    let dir = temp_dir("trim");
    let mut history = ChatHistory::load(&dir);
    for i in 0..MAX_ENTRIES + 10 {
        history.append(&format!("prompt-{i}"));
    }
    let entries = history.entries();
    assert_eq!(entries.len(), MAX_ENTRIES);
    assert_eq!(entries[0], format!("prompt-{}", 10));
    assert_eq!(entries.last(), Some(&format!("prompt-{}", MAX_ENTRIES + 9)));
}

#[test]
fn blank_prompts_do_not_create_a_file_line() {
    let dir = temp_dir("blank");
    let mut history = ChatHistory::load(&dir);
    history.append("   ");
    assert!(history.entries().is_empty());
}
