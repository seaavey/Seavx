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
