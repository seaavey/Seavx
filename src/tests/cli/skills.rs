use super::*;

fn write_skill(root: &Path, name: &str, description: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nbody\n"),
    )
    .unwrap();
}

/// A workspace and a home, both inside one temp dir: no test touches a real root.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("repo");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    (temp, workspace, home)
}

#[test]
fn render_list_names_the_owning_root_and_marks_shadowed_copies() {
    let (_temp, workspace, home) = fixture();
    write_skill(
        &workspace.join(".seavx/skills"),
        "demo",
        "A demo skill. Second sentence.",
    );
    write_skill(&home.join(".agents/skills/shared"), "demo", "A copy.");

    let report = list_report_with_home(&workspace, Some(&home));
    assert_eq!(report.skills.len(), 1, "one row per winning copy");
    let row = &report.skills[0];
    assert_eq!(row.name, "demo");
    assert_eq!(row.root, workspace.join(".seavx/skills"));
    assert_eq!(row.shadowed, vec![home.join(".agents/skills")]);

    let text = render_list(&report);
    assert!(
        text.contains(&format!(
            "- demo: A demo skill ({})",
            workspace.join(".seavx/skills").display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "[shadowed: {}]",
            home.join(".agents/skills").display()
        )),
        "{text}"
    );
}

#[test]
fn render_list_keeps_one_line_per_skill() {
    let (_temp, workspace, home) = fixture();
    let root = workspace.join(".seavx/skills");
    write_skill(&root, "alpha", "First.");
    write_skill(&root, "beta", &format!("{} tail.", "x".repeat(200)));

    let text = render_list(&list_report_with_home(&workspace, Some(&home)));
    let rows: Vec<&str> = text.lines().filter(|line| line.starts_with("- ")).collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert!(
        rows[1].contains('…'),
        "a long description is capped: {text}"
    );
    assert!(
        rows.iter().all(|row| !row.contains("tail.")),
        "the second sentence is dropped: {text}"
    );
}

#[test]
fn an_empty_report_says_so_and_lists_the_roots() {
    let (_temp, workspace, home) = fixture();
    let text = render_list(&list_report_with_home(&workspace, Some(&home)));

    assert!(text.contains("No skills installed."), "{text}");
    for root in [
        workspace.join(".seavx/skills"),
        workspace.join("skills"),
        home.join(".seavx/skills"),
        home.join(".agents/skills"),
    ] {
        assert!(
            text.contains(&root.display().to_string()),
            "missing root {root:?}: {text}"
        );
    }
}

#[test]
fn render_paths_lists_the_roots_in_precedence_order() {
    let (_temp, workspace, home) = fixture();
    std::fs::create_dir_all(workspace.join("skills")).unwrap();

    let text = render_paths_with_home(&workspace, Some(&home));
    let rows: Vec<&str> = text.lines().filter(|line| line.starts_with("- ")).collect();
    let expected = [
        (workspace.join(".seavx/skills"), "missing"),
        (workspace.join("skills"), "found"),
        (home.join(".seavx/skills"), "missing"),
        (home.join(".agents/skills"), "missing"),
    ];
    assert_eq!(rows.len(), expected.len(), "{text}");
    for (row, (root, state)) in rows.iter().zip(expected) {
        assert_eq!(*row, format!("- {} ({state})", root.display()));
    }
}
