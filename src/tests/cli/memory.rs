use super::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn workspace_memory_wins_over_root_memory() {
    let temp = tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".seavx")).unwrap();
    fs::write(temp.path().join(".seavx/MEMORY.md"), "workspace memory").unwrap();
    fs::write(temp.path().join("MEMORY.md"), "root memory").unwrap();

    let report = build_report_with_home(temp.path(), None);
    assert_eq!(report.project.unwrap().content, "workspace memory");
}

#[test]
fn truncates_long_files_on_a_char_boundary() {
    let temp = tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".seavx")).unwrap();
    let content = "é".repeat(20_000);
    fs::write(temp.path().join(".seavx/MEMORY.md"), content).unwrap();

    let file = build_report_with_home(temp.path(), None).project.unwrap();
    assert!(file.truncated);
    assert!(file.content.len() <= 16 * 1024);
    assert!(std::str::from_utf8(file.content.as_bytes()).is_ok());
}

#[test]
fn renders_missing_guidance_and_view_filters() {
    let temp = tempdir().unwrap();
    let report = build_report_with_home(temp.path(), None);

    let all = render_text(&report, MemoryView::All);
    assert!(all.contains("Project memory: not found"));
    assert!(all.contains("The agent can create one with the `save_memory` tool"));

    let project = render_text(&report, MemoryView::Project);
    assert!(project.contains("Project memory"));
    assert!(!project.contains("Installed skills"));
}
