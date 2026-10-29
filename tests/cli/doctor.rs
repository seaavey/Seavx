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
