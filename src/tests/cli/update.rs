use super::*;

#[test]
fn a_digest_is_spelled_in_lowercase_hex() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        hex_lower(&Sha256::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_published_checksum_is_compared_case_insensitively() {
    assert!(verify_digest(Some("ABCDEF"), "abcdef").unwrap());
    assert!(
        !verify_digest(None, "abcdef").unwrap(),
        "a pointer without a checksum is not a match"
    );
}

#[test]
fn a_mismatched_checksum_names_both_values() {
    let error = verify_digest(Some("aaaa"), "bbbb").unwrap_err();
    assert!(error.contains("aaaa"), "{error}");
    assert!(error.contains("bbbb"), "{error}");
}

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
