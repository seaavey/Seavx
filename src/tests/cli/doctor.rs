use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

struct FakeProbe {
    calls: AtomicUsize,
    result: ProbeResult,
}

#[async_trait::async_trait]
impl EndpointProbe for FakeProbe {
    async fn probe(&self, _base_url: &str, _api_key: Option<&str>) -> ProbeResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.result {
            ProbeResult::Ok { models } => ProbeResult::Ok { models: *models },
            ProbeResult::Unauthorized => ProbeResult::Unauthorized,
            ProbeResult::NotFound => ProbeResult::NotFound,
            ProbeResult::Unreachable(e) => ProbeResult::Unreachable(e.clone()),
            ProbeResult::Status(code) => ProbeResult::Status(*code),
        }
    }
}

fn options(temp: &std::path::Path) -> DoctorOptions {
    DoctorOptions {
        offline: false,
        workspace_root: temp.to_path_buf(),
        home: None,
    }
}

#[tokio::test]
async fn offline_skips_the_endpoint_probe() {
    let temp = tempdir().unwrap();
    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 1 },
    };
    let mut opts = options(temp.path());
    opts.offline = true;
    let report = run_with_probe(opts, &probe).await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "endpoint" && c.status == CheckStatus::Skip)
    );
}

#[tokio::test]
async fn unauthorized_is_a_failure_and_404_is_a_warning() {
    let temp = tempdir().unwrap();
    let unauthorized = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Unauthorized,
    };
    let report = run_with_probe(options(temp.path()), &unauthorized).await;
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "endpoint" && c.status == CheckStatus::Fail)
    );

    let not_found = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::NotFound,
    };
    let report = run_with_probe(options(temp.path()), &not_found).await;
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "endpoint" && c.status == CheckStatus::Warn)
    );
}

#[tokio::test]
async fn malformed_config_is_reported_as_failure() {
    let temp = tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".seavx")).unwrap();
    std::fs::write(temp.path().join(".seavx/config.toml"), "model = [").unwrap();
    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 0 },
    };
    let report = run_with_probe(options(temp.path()), &probe).await;
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "config" && c.status == CheckStatus::Fail)
    );
}

#[tokio::test]
async fn doctor_masks_the_configured_key() {
    let temp = tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".seavx")).unwrap();
    std::fs::write(
        temp.path().join(".seavx/config.toml"),
        "model = \"m\"\nmax_turns = 5\napi_key = \"sr-live-12345678\"\n",
    )
    .unwrap();
    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 0 },
    };
    let report = run_with_probe(options(temp.path()), &probe).await;
    let text = render(&report);
    assert!(text.contains("...5678"));
    assert!(!text.contains("sr-live-12345678"));
}

#[test]
fn rendered_report_never_contains_the_full_key() {
    let report = DoctorReport {
        version: "test".into(),
        platform: "test".into(),
        checks: vec![CheckResult {
            id: "secret",
            status: CheckStatus::Ok,
            detail: "set (...5678)".into(),
        }],
    };
    let text = render(&report);
    assert!(text.contains("...5678"));
    assert!(!text.contains("sr-live-12345678"));
}

#[test]
fn exit_code_is_one_only_when_a_check_fails() {
    let mut report = DoctorReport {
        version: "test".into(),
        platform: "test".into(),
        checks: vec![CheckResult {
            id: "one",
            status: CheckStatus::Warn,
            detail: "warn".into(),
        }],
    };
    assert_eq!(exit_code(&report), 0);
    report.checks[0].status = CheckStatus::Fail;
    assert_eq!(exit_code(&report), 1);
}

#[cfg(unix)]
#[test]
fn a_planted_symlink_at_the_probe_name_is_not_followed() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("target.txt");
    std::fs::write(&target, "untouched").unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join(".probe")).unwrap();

    let error = create_probe(temp.path(), ".probe").unwrap_err();
    assert!(matches!(error, ProbeError::InUse));
    assert!(
        temp.path().join(".probe").is_symlink(),
        "the planted link survives"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
}

#[test]
fn a_pre_existing_file_at_the_probe_name_is_left_untouched() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(".probe");
    std::fs::write(&path, "keep me").unwrap();

    let error = create_probe(temp.path(), ".probe").unwrap_err();
    assert!(matches!(error, ProbeError::InUse));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
}

#[test]
fn a_failing_cleanup_is_reported() {
    let temp = tempdir().unwrap();
    let probe = create_probe(temp.path(), ".probe").unwrap();

    // A file replaced under the same name is not the one this check created:
    // removing it would destroy somebody else's data.
    let replacement = temp.path().join("replacement");
    std::fs::write(&replacement, "theirs").unwrap();
    std::fs::rename(&replacement, temp.path().join(".probe")).unwrap();

    let error = remove_probe(probe).unwrap_err();
    assert!(error.contains("no longer the file"), "{error}");
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".probe")).unwrap(),
        "theirs",
        "the replacement is left alone"
    );
}

#[cfg(unix)]
#[test]
fn the_probe_is_owner_only_and_removed() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let probe = create_probe(temp.path(), ".probe").unwrap();
    let mode = std::fs::metadata(temp.path().join(".probe"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the probe is owner-only");

    remove_probe(probe).unwrap();
    assert!(!temp.path().join(".probe").exists());
}

fn check<'a>(report: &'a DoctorReport, id: &str) -> &'a CheckResult {
    report
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("no {id} check in {}", render(report)))
}

#[tokio::test]
async fn doctor_reports_storage_permissions_and_streaming_without_secrets() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".seavx")).unwrap();
    std::fs::write(
        workspace.join(".seavx/config.toml"),
        "model = \"openai/gpt-4o-mini\"\nmax_turns = 10\ntool_permissions = \"ask\"\npermitted_actions = [\"workspace-write\"]\napi_key = \"sk-never-print-me\"\nbase_url = \"https://user:pass@host/v1?token=secret\"\n",
    )
    .unwrap();

    // A session database next to the config, as debug mode keeps it.
    let store =
        crate::session::SessionStore::open_at(&workspace.join(".seavx/sessions.db")).unwrap();
    crate::session::prepare_turn(&store, "ws", "m", None, "hello").unwrap();
    drop(store);

    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 1 },
    };
    let report = run_with_probe(options(&workspace), &probe).await;

    let session = check(&report, "session");
    assert_eq!(session.status, CheckStatus::Ok, "{}", session.detail);
    assert!(session.detail.contains("schema v"), "{}", session.detail);
    assert!(
        session.detail.contains("genai-chat-v1"),
        "{}",
        session.detail
    );
    assert!(session.detail.contains("writable"), "{}", session.detail);

    let permissions = check(&report, "permissions");
    assert_eq!(permissions.status, CheckStatus::Ok);
    assert!(permissions.detail.contains("ask"), "{}", permissions.detail);
    assert!(
        permissions.detail.contains("workspace-write"),
        "{}",
        permissions.detail
    );

    let stream = check(&report, "stream");
    assert_eq!(stream.status, CheckStatus::Ok);
    assert!(stream.detail.contains("enabled"), "{}", stream.detail);

    let text = render(&report);
    assert!(!text.contains("sk-never-print-me"), "{text}");
    assert!(!text.contains("user:pass@"), "{text}");
    assert!(!text.contains("?token="), "{text}");
    assert!(!text.contains("token=secret"), "{text}");
}

#[tokio::test]
async fn a_config_parse_error_is_a_fail() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join(".seavx")).unwrap();
    std::fs::write(
        workspace.join(".seavx/config.toml"),
        "model = \"unterminated\n",
    )
    .unwrap();

    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 1 },
    };
    let report = run_with_probe(options(&workspace), &probe).await;
    let config = check(&report, "config");
    assert_eq!(config.status, CheckStatus::Fail);
    assert!(config.detail.contains("parse error"), "{}", config.detail);
    assert!(
        !config.detail.contains('\n'),
        "one line, no echoed source: {}",
        config.detail
    );
}

#[tokio::test]
async fn a_pending_recovery_conflict_is_named() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let directory = workspace.join(".seavx");
    std::fs::create_dir_all(&directory).unwrap();

    // A `.bak` with its `.new` record, and a third state in the target: the
    // loader keeps the backup and reports the conflict.
    let previous = b"model = \"previous\"\n";
    let intended = b"model = \"intended\"\n";
    std::fs::write(directory.join("config.toml"), "model = \"third\"\n").unwrap();
    let backup = directory.join(format!("config.toml.bak-100-0-{}", previous.len()));
    std::fs::write(&backup, previous).unwrap();
    std::fs::write(
        format!("{}.new", backup.display()),
        format!(
            "{}\t{}\n",
            intended.len(),
            crate::checkpoint::fnv1a64(intended)
        ),
    )
    .unwrap();

    let probe = FakeProbe {
        calls: AtomicUsize::new(0),
        result: ProbeResult::Ok { models: 1 },
    };
    let report = run_with_probe(options(&workspace), &probe).await;
    let recovery = check(&report, "config-recovery");
    assert_eq!(recovery.status, CheckStatus::Warn, "{}", recovery.detail);
    assert!(
        recovery.detail.contains("config recovery conflict") || recovery.detail.contains("backup"),
        "{}",
        recovery.detail
    );
}

#[test]
fn setup_prints_the_redacted_url() {
    let printed = crate::cli::config::redact_url("https://user:pass@host/v1?token=secret");
    assert!(printed.contains("https://host/"), "{printed}");
    assert!(printed.contains("(userinfo/query redacted)"), "{printed}");
    assert!(!printed.contains("user:pass"), "{printed}");
    assert!(!printed.contains("secret"), "{printed}");
}
