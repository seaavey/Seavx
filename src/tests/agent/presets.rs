use crate::agent::presets;

fn workspace() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

fn write_preset(root: &std::path::Path, dir: &str, name: &str, text: &str) {
    let path = root.join(dir).join("prompts");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(format!("{name}.md")), text).unwrap();
}

#[test]
fn the_project_copy_shadows_the_user_copy() {
    let home = tempfile::TempDir::new().unwrap();
    let root = workspace();

    write_preset(home.path(), ".seavx", "review", "user wording");
    write_preset(root.path(), ".seavx", "review", "project wording");

    assert_eq!(
        presets::load_in(
            root.path(),
            Some(&home.path().join(".seavx").join("prompts")),
            "review"
        )
        .unwrap(),
        "project wording"
    );
    let listed: Vec<String> = presets::list_in(
        root.path(),
        Some(&home.path().join(".seavx").join("prompts")),
    )
    .into_iter()
    .map(|(name, _)| name)
    .collect();
    assert_eq!(listed, vec!["review".to_string()], "listed once");
}

#[test]
fn a_name_that_could_traverse_is_refused_before_the_filesystem() {
    let home = tempfile::TempDir::new().unwrap();
    let root = workspace();

    for name in ["../secret", "a/b", ".hidden", "", "with space", "x.md"] {
        let error = presets::load_in(
            root.path(),
            Some(&home.path().join(".seavx").join("prompts")),
            name,
        )
        .unwrap_err();
        assert!(
            error.contains("not a preset name") || error.contains("no preset"),
            "{name}: {error}"
        );
    }
}

#[test]
fn a_symlink_an_oversize_file_and_a_missing_name_are_refused_clearly() {
    let home = tempfile::TempDir::new().unwrap();
    let root = workspace();
    let dir = root.path().join(".seavx").join("prompts");
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(root.path().join("outside.md"), "secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("outside.md"), dir.join("link.md")).unwrap();
    #[cfg(unix)]
    {
        let error = presets::load_in(
            root.path(),
            Some(&home.path().join(".seavx").join("prompts")),
            "link",
        )
        .unwrap_err();
        assert!(error.contains("symlink"), "{error}");
    }

    std::fs::write(
        dir.join("big.md"),
        "x".repeat((presets::PRESET_LIMIT + 1) as usize),
    )
    .unwrap();
    let error = presets::load_in(
        root.path(),
        Some(&home.path().join(".seavx").join("prompts")),
        "big",
    )
    .unwrap_err();
    assert!(error.contains("limit"), "{error}");

    let error = presets::load_in(
        root.path(),
        Some(&home.path().join(".seavx").join("prompts")),
        "absent",
    )
    .unwrap_err();
    assert!(error.contains("no preset named 'absent'"), "{error}");
}
