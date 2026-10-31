//! Resolver tests: every case builds a throwaway repository in a temporary
//! directory, and asserts the checkout is byte-identical afterwards.

use crate::agent::git_review::{DIFF_LIMIT, ReviewTarget, parse_target, resolve};
use crate::agent::security::WorkspaceSecurity;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// A repository with one committed file per pair, on `main`.
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    for (path, body) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }
    git(dir.path(), &["add", "--", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    dir
}

fn write(dir: &std::path::Path, path: &str, body: &str) {
    let full = dir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

/// What a review must not move: status, HEAD, the index bytes and every file.
#[derive(Debug, PartialEq, Eq)]
struct Checkout {
    status: String,
    head: String,
    index: Vec<u8>,
    files: Vec<(String, Vec<u8>)>,
}

fn snapshot(dir: &std::path::Path) -> Checkout {
    let mut files = Vec::new();
    for entry in walk(dir) {
        files.push((entry.clone(), std::fs::read(dir.join(&entry)).unwrap()));
    }
    files.sort();
    Checkout {
        status: git(dir, &["status", "--porcelain=v1", "-z"]),
        head: git(dir, &["rev-parse", "HEAD"]),
        index: std::fs::read(dir.join(".git/index")).unwrap_or_default(),
        files,
    }
}

fn walk(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                found.push(relative.to_string_lossy().to_string());
            }
        }
    }
    found
}

fn resolve_after(
    dir: &std::path::Path,
    target: &ReviewTarget,
) -> crate::agent::git_review::ReviewInput {
    let security = WorkspaceSecurity::new(dir);
    let before = snapshot(dir);
    let input = resolve(dir, target, &security).expect("the target resolves");
    assert_eq!(
        snapshot(dir),
        before,
        "resolution must not touch the checkout"
    );
    input
}

#[test]
fn parse_target_accepts_only_the_documented_forms() {
    assert_eq!(parse_target(&[]), Ok(ReviewTarget::WorkingTree));
    assert_eq!(
        parse_target(&["working-tree"]),
        Ok(ReviewTarget::WorkingTree)
    );
    assert_eq!(parse_target(&["staged"]), Ok(ReviewTarget::Staged));
    assert_eq!(
        parse_target(&["commit", "HEAD~1"]),
        Ok(ReviewTarget::Commit("HEAD~1".to_string()))
    );
    assert_eq!(
        parse_target(&["base", "main"]),
        Ok(ReviewTarget::Base("main".to_string()))
    );

    for bad in [
        vec!["staged", "extra"],
        vec!["commit"],
        vec!["base"],
        vec!["commit", "a", "b"],
        vec!["branch"],
        vec![""],
    ] {
        assert!(parse_target(&bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn an_unmodified_checkout_reviews_as_empty() {
    let repo = repo(&[("a.txt", "one\n")]);
    let input = resolve_after(repo.path(), &ReviewTarget::WorkingTree);
    assert!(input.is_empty);
    assert!(input.changed_paths.is_empty());
    assert_eq!(input.withheld_paths, 0);
}

#[test]
fn an_unstaged_edit_is_in_the_diff() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), "a.txt", "one\ntwo\n");
    let input = resolve_after(repo.path(), &ReviewTarget::WorkingTree);
    assert!(!input.is_empty);
    assert_eq!(input.changed_paths, vec!["a.txt".to_string()]);
    assert!(input.diff.contains("+two"), "{}", input.diff);
}

#[test]
fn a_staged_edit_survives_only_in_the_staged_target() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), "a.txt", "one\nstaged\n");
    git(repo.path(), &["add", "--", "a.txt"]);

    let staged = resolve_after(repo.path(), &ReviewTarget::Staged);
    assert!(staged.diff.contains("+staged"), "{}", staged.diff);

    let commit = resolve_after(repo.path(), &ReviewTarget::Commit("HEAD".to_string()));
    assert!(commit.is_empty, "HEAD alone changes nothing");
}

#[test]
fn an_untracked_file_is_included_and_an_ignored_one_is_not() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), ".gitignore", "*.log\n");
    write(repo.path(), "new.txt", "fresh\n");
    write(repo.path(), "noise.log", "ignored\n");

    let input = resolve_after(repo.path(), &ReviewTarget::WorkingTree);
    assert!(
        input.changed_paths.contains(&"new.txt".to_string()),
        "{:?}",
        input.changed_paths
    );
    assert!(!input.changed_paths.contains(&"noise.log".to_string()));
    assert!(input.diff.contains("fresh"), "{}", input.diff);
}

#[test]
fn a_commit_target_reviews_that_commit() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), "a.txt", "one\ntwo\n");
    git(repo.path(), &["add", "--", "a.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "second"]);

    let input = resolve_after(repo.path(), &ReviewTarget::Commit("HEAD".to_string()));
    assert!(input.diff.contains("+two"), "{}", input.diff);
    assert_eq!(input.changed_paths, vec!["a.txt".to_string()]);
}

#[test]
fn a_base_target_reviews_the_merge_base() {
    let repo = repo(&[("a.txt", "one\n")]);
    git(repo.path(), &["checkout", "-q", "-b", "feature"]);
    write(repo.path(), "a.txt", "one\nfeature\n");
    git(repo.path(), &["add", "--", "a.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "feature"]);

    let input = resolve_after(repo.path(), &ReviewTarget::Base("main".to_string()));
    assert!(input.diff.contains("+feature"), "{}", input.diff);
}

#[test]
fn a_bad_ref_fails_before_any_diff_is_built() {
    let repo = repo(&[("a.txt", "one\n")]);
    let security = WorkspaceSecurity::new(repo.path());
    for target in [
        ReviewTarget::Commit("nope".to_string()),
        ReviewTarget::Base("nope".to_string()),
        ReviewTarget::Commit("HEAD^{tree}".to_string()),
        ReviewTarget::Commit("--all".to_string()),
    ] {
        let error = resolve(repo.path(), &target, &security).unwrap_err();
        assert!(!error.is_empty(), "{target:?} must explain itself");
    }
}

#[test]
fn a_directory_without_git_metadata_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let security = WorkspaceSecurity::new(dir.path());
    assert!(resolve(dir.path(), &ReviewTarget::WorkingTree, &security).is_err());
}

#[test]
fn a_protected_file_is_withheld_and_never_in_the_diff() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), ".env", "SECRET=sk-live-should-not-leak\n");

    let input = resolve_after(repo.path(), &ReviewTarget::WorkingTree);
    assert_eq!(input.withheld_paths, 1, "{:?}", input.changed_paths);
    assert!(!input.diff.contains("sk-live-should-not-leak"));
    assert!(!input.changed_paths.contains(&".env".to_string()));
}

#[test]
fn an_untracked_symlink_is_withheld() {
    #[cfg(unix)]
    {
        let repo = repo(&[("a.txt", "one\n")]);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            repo.path().join("link.txt"),
        )
        .unwrap();

        let input = resolve_after(repo.path(), &ReviewTarget::WorkingTree);
        assert_eq!(input.withheld_paths, 1);
        assert!(!input.changed_paths.contains(&"link.txt".to_string()));
    }
}

#[test]
fn output_over_the_cap_fails_the_review_instead_of_truncating() {
    let repo = repo(&[("a.txt", "one\n")]);
    write(repo.path(), "big.txt", &"x".repeat(DIFF_LIMIT + 1024));
    let security = WorkspaceSecurity::new(repo.path());
    let error = resolve(repo.path(), &ReviewTarget::WorkingTree, &security).unwrap_err();
    assert!(
        error.contains("1 MiB") || error.contains("exceeds"),
        "{error}"
    );
}

#[test]
fn the_task_text_names_the_target_and_withholds_no_content() {
    let input = crate::agent::git_review::ReviewInput {
        target: "staged".to_string(),
        diff: "+one\n".to_string(),
        changed_paths: vec!["a.txt".to_string()],
        withheld_paths: 2,
        is_empty: false,
    };
    let text = crate::agent::git_review::task_text(&input);
    assert!(text.contains("staged"), "{text}");
    assert!(text.contains("a.txt"), "{text}");
    assert!(text.contains("+one"), "{text}");
    assert!(text.contains("2 path(s) were withheld"), "{text}");

    let empty = crate::agent::git_review::ReviewInput {
        is_empty: true,
        ..input
    };
    let text = crate::agent::git_review::task_text(&empty);
    assert!(text.contains("no changes"), "{text}");
    assert!(!text.contains("+one"), "{text}");
}

/// A stream that fails mid-read, the way a broken pipe or a killed Git does.
struct FailingReader;

impl std::io::Read for FailingReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("pipe broke"))
    }
}

#[test]
fn a_read_error_is_not_reported_as_a_complete_stream() {
    let overflow = std::sync::atomic::AtomicBool::new(false);
    let (buffer, stop) = super::read_capped(FailingReader, &overflow);
    assert!(buffer.is_empty(), "nothing was read");
    assert!(
        stop.is_some_and(|reason| reason.contains("could not be read")),
        "an interrupted read must never read as a clean end of output: {stop:?}"
    );
    assert!(!overflow.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn a_stream_that_ends_normally_reports_no_stop() {
    let overflow = std::sync::atomic::AtomicBool::new(false);
    let (buffer, stop) = super::read_capped(&b"+one\n"[..], &overflow);
    assert_eq!(buffer, b"+one\n");
    assert!(stop.is_none(), "{stop:?}");
}
