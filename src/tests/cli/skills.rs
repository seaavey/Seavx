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
fn render_show_finds_the_winning_copy_and_errors_on_an_unknown_name() {
    let (_temp, workspace, home) = fixture();
    write_skill(
        &home.join(".agents/skills/shared"),
        "only",
        "A shared skill.",
    );

    let text =
        render_show_with_home(&workspace, Some(&home), "only").expect("a skill in a later root");
    assert!(text.starts_with("## only"), "{text}");
    assert!(text.contains("body"), "{text}");

    let err = render_show_with_home(&workspace, Some(&home), "nope").unwrap_err();
    assert!(err.contains("nope"), "the error names the skill: {err}");
}

#[test]
fn render_show_truncates_a_long_body_and_names_the_dropped_lines() {
    let (_temp, workspace, home) = fixture();
    let dir = workspace.join(".seavx/skills/big");
    std::fs::create_dir_all(&dir).unwrap();
    let body: String = (0..600)
        .map(|index| format!("line {index} {}\n", "x".repeat(80)))
        .collect();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: big\ndescription: Big.\n---\n{body}"),
    )
    .unwrap();

    let text = render_show_with_home(&workspace, Some(&home), "big").expect("the winner");
    assert!(
        text.contains(&dir.join("SKILL.md").display().to_string()),
        "{text}"
    );
    assert!(text.contains("line 0 "), "{text}");
    assert!(!text.contains("line 599 "), "the tail is dropped: {text}");

    let marker = text.find("... truncated (").expect("a truncation marker");
    let more: usize = text[marker..]
        .trim_start_matches("... truncated (")
        .trim_end_matches(" more lines)")
        .parse()
        .unwrap();
    // Three header lines (heading, path, blank) sit above the body.
    let shown = text[..marker].lines().count() - 3;
    assert_eq!(more, 604 - shown, "the marker names the dropped line count");
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

#[test]
fn install_copies_the_whole_directory_and_the_skill_shows_up() {
    let (_temp, workspace, home) = fixture();
    let source = workspace.join("incoming/my-skill");
    write_skill(
        &workspace.join("incoming"),
        "my-skill",
        "An installed skill.",
    );
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(source.join("scripts/run.sh"), "echo hi").unwrap();

    let destination = install(&workspace, &source).unwrap();

    assert_eq!(destination, workspace.join(".seavx/skills/my-skill"));
    assert_eq!(
        std::fs::read_to_string(destination.join("scripts/run.sh")).unwrap(),
        "echo hi"
    );
    let report = list_report_with_home(&workspace, Some(&home));
    assert!(
        report.skills.iter().any(|row| row.name == "my-skill"),
        "an installed skill is loadable: {:?}",
        report
            .skills
            .iter()
            .map(|row| &row.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn install_refuses_a_source_without_a_skill_file() {
    let (_temp, workspace, _home) = fixture();
    let source = workspace.join("incoming/empty");
    std::fs::create_dir_all(&source).unwrap();

    let error = install(&workspace, &source).unwrap_err();
    assert!(error.contains("no SKILL.md"), "{error}");
}

#[test]
fn install_refuses_to_overwrite_an_existing_skill() {
    let (_temp, workspace, _home) = fixture();
    let source = workspace.join("incoming/demo");
    write_skill(&workspace.join("incoming"), "demo", "The new copy.");
    let existing = workspace.join(".seavx/skills/demo");
    write_skill(&workspace.join(".seavx/skills"), "demo", "The old copy.");

    let error = install(&workspace, &source).unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    let kept = std::fs::read_to_string(existing.join("SKILL.md")).unwrap();
    assert!(kept.contains("The old copy."), "{kept}");
}

#[cfg(unix)]
#[test]
fn install_refuses_a_source_that_contains_a_symlink() {
    let (_temp, workspace, _home) = fixture();
    let source = workspace.join("incoming/linked");
    write_skill(
        &workspace.join("incoming"),
        "linked",
        "A skill with a link.",
    );
    let outside = workspace.join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, source.join("leak.txt")).unwrap();

    let error = install(&workspace, &source).unwrap_err();
    assert!(error.contains("symlink"), "{error}");
    assert!(
        !workspace.join(".seavx/skills/linked").exists(),
        "a refused install leaves nothing behind"
    );
}
