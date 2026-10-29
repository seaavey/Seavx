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

#[cfg(unix)]
mod verified_open {
    use crate::agent::security::{
        PathSnapshot, check_write_target, open_verified, read_verified, snapshot,
    };
    use std::os::unix::fs::symlink;

    fn outside(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn swapped_symlink_before_open_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("notes.md");
        std::fs::write(&target, "mine").unwrap();
        let (_outside_dir, secret) = outside("outside.txt", "secret");

        let before: PathSnapshot = snapshot(&target).unwrap();
        std::fs::remove_file(&target).unwrap();
        symlink(&secret, &target).unwrap();

        let opened = open_verified(&target, &before).await;
        assert!(opened.is_err(), "a swapped symlink must not open");
    }

    #[tokio::test]
    async fn path_changed_between_validation_and_open_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("notes.md");
        std::fs::write(&target, "before").unwrap();

        let before = snapshot(&target).unwrap();
        std::fs::write(&target, "a longer body written in place").unwrap();

        let error = open_verified(&target, &before)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("path changed between validation and open"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn hard_linked_files_refuse_a_write_but_allow_a_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("notes.md");
        std::fs::write(&target, "body").unwrap();
        std::fs::hard_link(&target, dir.path().join("link.md")).unwrap();

        let error = check_write_target(&target).unwrap_err();
        assert!(
            error.contains("file has 2 hard links"),
            "unexpected: {error}"
        );

        let before = snapshot(&target).unwrap();
        let file = open_verified(&target, &before).await;
        assert!(file.is_ok(), "a read of a hard-linked file stays allowed");
    }

    #[tokio::test]
    async fn fifo_without_a_writer_fails_fast() {
        let dir = tempfile::TempDir::new().unwrap();
        let fifo = dir.path().join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a plain mkfifo(2) on a path this test owns.
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(created, 0, "mkfifo failed");

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), read_verified(&fifo)).await;
        let outcome = result.expect("reading a FIFO must not block");
        assert!(outcome.is_err(), "a FIFO must be refused, not read");
    }

    #[tokio::test]
    async fn socket_and_device_nodes_are_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let result = read_verified(&socket).await;
        assert!(result.is_err(), "a socket must be refused");
    }
}

#[test]
fn redact_masks_every_documented_shape() {
    let cases = [
        ("api_key = \"sk-live-123\"", "sk-live-123"),
        ("Authorization: Bearer abc.def", "abc.def"),
        ("token=ghp_abcdef", "ghp_abcdef"),
        ("password: hunter2", "hunter2"),
        ("key «redacted:AKIA…»", "«redacted:AKIA…»"),
    ];
    for (line, secret) in cases {
        let redacted = crate::agent::security::redact(line);
        assert!(!redacted.contains(secret), "{line} leaked as {redacted}");
    }

    let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END-----";
    let redacted = crate::agent::security::redact(pem);
    assert!(redacted.contains("-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(!redacted.contains("AAAA"));

    assert_eq!(
        crate::agent::security::redact("ordinary text"),
        "ordinary text"
    );
}

/// Fixture for the search tests: one ordinary hit, and three protected ones.
fn search_workspace() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for rel in ["notes.md", "deploy.pem", ".env", ".git/config"] {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "PRIVATE_KEY here\n").unwrap();
    }
    dir
}

#[tokio::test]
async fn search_skips_protected_files_and_counts_them() {
    let dir = search_workspace();
    let security = WorkspaceSecurity::new(dir.path());
    let output = crate::agent::tools::ToolRegistry::with_default_tools()
        .get("search_files")
        .unwrap()
        .execute(serde_json::json!({ "pattern": "PRIVATE_KEY" }), &security)
        .await
        .unwrap();

    assert!(
        output.contains("notes.md"),
        "an ordinary hit must stay: {output}"
    );
    for leaked in ["deploy.pem", ".env", ".git"] {
        assert!(!output.contains(leaked), "{leaked} leaked: {output}");
    }
    assert!(
        output.ends_with("[2 files skipped by policy]"),
        "unexpected tail: {output}"
    );
}

#[tokio::test]
async fn search_over_only_protected_files_returns_no_lines() {
    let dir = tempfile::TempDir::new().unwrap();
    for rel in ["deploy.pem", ".env"] {
        std::fs::write(dir.path().join(rel), "PRIVATE_KEY here\n").unwrap();
    }
    let security = WorkspaceSecurity::new(dir.path());
    let output = crate::agent::tools::ToolRegistry::with_default_tools()
        .get("search_files")
        .unwrap()
        .execute(serde_json::json!({ "pattern": "PRIVATE_KEY" }), &security)
        .await
        .unwrap();

    assert!(
        !output.contains("PRIVATE_KEY here"),
        "a protected line leaked: {output}"
    );
    assert!(
        output.ends_with("[2 files skipped by policy]"),
        "unexpected output: {output}"
    );
}

#[test]
fn the_matrix_answers_every_cell() {
    use crate::agent::permissions::*;

    let table = [
        (PermissionMode::Deny, ActionKind::Read, Verdict::Allow),
        (PermissionMode::Deny, ActionKind::Write, Verdict::Deny),
        (PermissionMode::Deny, ActionKind::GlobalWrite, Verdict::Deny),
        // `deny` refuses every command that is not an allowlisted read; those
        // classify as `Read`, so an `Execute` cell here is a denial.
        (PermissionMode::Deny, ActionKind::Execute, Verdict::Deny),
        (PermissionMode::Deny, ActionKind::Network, Verdict::Deny),
        (PermissionMode::Ask, ActionKind::Read, Verdict::Allow),
        (PermissionMode::Ask, ActionKind::Write, Verdict::Allow),
        (PermissionMode::Ask, ActionKind::GlobalWrite, Verdict::Ask),
        (PermissionMode::Ask, ActionKind::Execute, Verdict::Ask),
        (PermissionMode::Ask, ActionKind::Network, Verdict::Ask),
        (PermissionMode::Ask, ActionKind::Destructive, Verdict::Deny),
        (PermissionMode::Allow, ActionKind::Read, Verdict::Allow),
        (PermissionMode::Allow, ActionKind::Write, Verdict::Allow),
        (PermissionMode::Allow, ActionKind::GlobalWrite, Verdict::Ask),
        (PermissionMode::Allow, ActionKind::Execute, Verdict::Allow),
        (PermissionMode::Allow, ActionKind::Network, Verdict::Allow),
        (
            PermissionMode::Allow,
            ActionKind::Destructive,
            Verdict::Deny,
        ),
    ];
    for (mode, kind, expected) in table {
        assert_eq!(decide(mode, &[], kind), expected, "{mode:?}/{kind:?}");
    }

    // `permitted_actions` lifts execute and network in `ask` mode only …
    assert_eq!(
        decide(
            PermissionMode::Ask,
            &[ActionKind::Network],
            ActionKind::Network
        ),
        Verdict::Allow
    );
    assert_eq!(
        decide(
            PermissionMode::Ask,
            &[ActionKind::Execute],
            ActionKind::Execute
        ),
        Verdict::Allow
    );
    // … never the global-write floor, in any mode.
    assert_eq!(
        decide(
            PermissionMode::Allow,
            &[ActionKind::GlobalWrite],
            ActionKind::GlobalWrite
        ),
        Verdict::Ask
    );
    assert_eq!(
        decide(
            PermissionMode::Ask,
            &[ActionKind::GlobalWrite],
            ActionKind::GlobalWrite
        ),
        Verdict::Ask
    );

    // A config naming a non-liftable kind is a validation error.
    assert!(parse_permitted(&["global-write".to_string()]).is_err());
    assert!(parse_permitted(&["destructive".to_string()]).is_err());
    assert_eq!(
        parse_permitted(&["execute".to_string(), "network".to_string()]).unwrap(),
        vec![ActionKind::Execute, ActionKind::Network]
    );
    assert_eq!(parse_mode("ask").unwrap(), PermissionMode::Ask);
    assert!(parse_mode("sometimes").is_err());
}

#[test]
fn target_classification_names_the_three_cases() {
    use crate::agent::permissions::{ActionKind, classify_target_with_home};

    let workspace = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let security = WorkspaceSecurity::new(workspace.path());

    let inside = workspace.path().join("notes.md");
    assert_eq!(
        classify_target_with_home(Some(home.path()), &security, &inside).unwrap(),
        ActionKind::Write
    );

    let memory = home.path().join(".seavx/USER.md");
    assert_eq!(
        classify_target_with_home(Some(home.path()), &security, &memory).unwrap(),
        ActionKind::GlobalWrite
    );
    let skill = home.path().join(".agents/skills/x/SKILL.md");
    assert_eq!(
        classify_target_with_home(Some(home.path()), &security, &skill).unwrap(),
        ActionKind::GlobalWrite
    );

    let elsewhere = tempfile::TempDir::new().unwrap();
    assert!(
        classify_target_with_home(
            Some(home.path()),
            &security,
            &elsewhere.path().join("x.txt")
        )
        .is_err(),
        "an unlisted outside path must not classify"
    );
}
