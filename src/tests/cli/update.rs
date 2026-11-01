use super::*;

#[test]
fn a_pointer_without_a_checksum_still_parses() {
    // Published before the field existed: the update must still run.
    let latest: Latest = serde_json::from_str(
        r#"{"sha":"abc1234","key":"builds/abc1234/seavx-linux-x86_64","version":"0.1.2","built_at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    assert!(latest.sha256.is_none());

    let latest: Latest = serde_json::from_str(
        r#"{"sha":"abc1234","key":"k","version":"0.1.2","built_at":"t","sha256":"abc"}"#,
    )
    .unwrap();
    assert_eq!(latest.sha256.as_deref(), Some("abc"));
}

#[test]
fn the_recorded_commit_decides_whether_a_build_is_latest() {
    let latest: Latest = serde_json::from_str(
        r#"{"sha":"06a4060","key":"k","version":"0.1.3","built_at":"t","sha256":"3e92be"}"#,
    )
    .unwrap();

    assert!(
        same_commit(Some("06a4060"), &latest),
        "built from the published commit: nothing to download"
    );
    assert!(
        !same_commit(Some("1111111"), &latest),
        "an older commit is not latest, even under the same version"
    );
    assert!(
        same_commit(None, &latest),
        "a build with no recorded commit leaves the version check standing"
    );
}
