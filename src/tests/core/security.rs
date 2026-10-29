use crate::agent::security::{WorkspaceSecurity, check_file};

/// Fixture: one workspace holding every protected class the policy names, plus
/// two files that must stay readable.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for rel in [
        ".env",
        ".env.local",
        "deploy.pem",
        "id_rsa",
        "id_ed25519",
        "credentials.json",
        "secrets.yaml",
        ".npmrc",
        "signing.p12",
        ".seavx/config.toml",
        ".seavx/MEMORY.md",
        ".seavx/checkpoints/x/manifest",
        "notes.md",
    ] {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }
    dir
}

#[test]
fn every_protected_class_is_refused() {
    let dir = workspace();
    for rel in [
        ".env",
        ".env.local",
        "deploy.pem",
        "id_rsa",
        "id_ed25519",
        "credentials.json",
        "secrets.yaml",
        ".npmrc",
        "signing.p12",
        ".seavx/config.toml",
        ".seavx/checkpoints/x/manifest",
    ] {
        let result = check_file(&dir.path().join(rel));
        assert!(result.is_err(), "{rel} must be refused");
    }
}

#[test]
fn memory_and_ordinary_files_stay_readable() {
    let dir = workspace();
    assert!(check_file(&dir.path().join("notes.md")).is_ok());
    assert!(check_file(&dir.path().join(".seavx/MEMORY.md")).is_ok());
}

#[test]
fn validation_refuses_a_protected_file_and_a_hidden_one() {
    let dir = workspace();
    let security = WorkspaceSecurity::new(dir.path());
    assert!(security.validate_path("notes.md").is_ok());
    assert!(security.validate_path(".env").is_err());
    assert!(security.validate_path(".seavx/config.toml").is_err());
}

#[test]
fn nested_new_path_under_an_existing_ancestor_is_accepted() {
    let dir = workspace();
    let security = WorkspaceSecurity::new(dir.path());
    let path = security.validate_path("a/b/c.txt").unwrap();
    assert!(path.ends_with("a/b/c.txt"));
    assert!(!path.exists(), "validation must not create the path");
    assert!(security.validate_path("../outside.txt").is_err());
}
