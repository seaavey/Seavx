use super::*;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/fixtures/update");
const FIXTURE_KEY: &str = include_str!("../../fixtures/update/fixture.pub");
const OTHER_KEY: &str = include_str!("../../fixtures/update/other.pub");

fn key() -> &'static str {
    FIXTURE_KEY.lines().nth(1).unwrap().trim()
}
fn other_key() -> &'static str {
    OTHER_KEY.lines().nth(1).unwrap().trim()
}
fn stable() -> Manifest {
    Manifest::parse(&std::fs::read_to_string(format!("{FIXTURES}/stable.json")).unwrap()).unwrap()
}
fn edge() -> Manifest {
    Manifest::parse(&std::fs::read_to_string(format!("{FIXTURES}/edge.json")).unwrap()).unwrap()
}

#[test]
fn missing_and_malformed_signatures_are_refused() {
    let text = std::fs::read_to_string(format!("{FIXTURES}/stable.json")).unwrap();

    let mut unsigned: serde_json::Value = serde_json::from_str(&text).unwrap();
    unsigned.as_object_mut().unwrap().remove("sig");
    let error = Manifest::parse(&unsigned.to_string()).unwrap_err();
    assert!(error.contains("sig"), "{error}");

    let mut malformed: serde_json::Value = serde_json::from_str(&text).unwrap();
    malformed["sig"] = serde_json::Value::from("nope");
    let error = Manifest::parse(&malformed.to_string()).unwrap_err();
    assert!(error.contains("sig"), "{error}");
}

#[test]
fn the_fixture_key_verifies_the_fixture_pointer() {
    stable().verify(&[key()]).unwrap();
}

#[test]
fn another_key_is_refused() {
    let error = stable().verify(&[other_key()]).unwrap_err();
    assert!(error.contains("signature"), "{error}");
}

#[test]
fn a_changed_signed_field_is_refused() {
    let text = std::fs::read_to_string(format!("{FIXTURES}/stable.json")).unwrap();
    for (field, changed) in [
        ("version", "0.1.4"),
        ("sha", "06a4061"),
        ("channel", "edge"),
        (
            "key",
            "builds/06a4060/81ce1245c381cf69a80d627853806331651fed06808598e34d8af628102ea0eb/seavx-linux-x86_64.old",
        ),
        (
            "sha256",
            "91ce1245c381cf69a80d627853806331651fed06808598e34d8af628102ea0eb",
        ),
        ("built_at", "2026-10-31T16:41:35Z"),
    ] {
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw[field] = serde_json::Value::from(changed);
        let manifest = Manifest::parse(&raw.to_string()).unwrap();
        assert!(
            manifest.verify(&[key()]).is_err(),
            "a changed {field} still verified"
        );
    }
}

#[test]
fn a_valid_signature_for_another_channel_is_refused_before_download() {
    let edge = edge();
    edge.verify(&[key()]).unwrap();
    let error = edge.check_channel(Channel::Stable).unwrap_err();
    assert!(error.contains("stable"), "{error}");
    assert!(error.contains("edge"), "{error}");
}

#[test]
fn a_signed_pointer_for_another_target_is_refused() {
    let edge = edge();
    edge.verify(&[key()]).unwrap();
    let error = edge
        .check_target(&Target::from_name("linux-x86_64").unwrap())
        .unwrap_err();
    assert!(error.contains("macos-arm64"), "{error}");
    assert!(error.contains("linux-x86_64"), "{error}");
    edge.check_target(&Target::from_name("macos-arm64").unwrap())
        .unwrap();
}

#[test]
fn a_route_that_disagrees_with_the_digest_path_is_refused() {
    let text = std::fs::read_to_string(format!("{FIXTURES}/stable.json")).unwrap();
    let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
    raw["key"] = serde_json::Value::from(
        "builds/06a4060/81ce1245c381cf69a80d627853806331651fed06808598e34d8af628102ea0eb",
    );
    let prefix = Manifest::parse(&raw.to_string()).unwrap();
    assert!(
        !prefix.key_matches_route(),
        "a prefix of the digest path is still not the path"
    );
    assert!(stable().key_matches_route());
}

#[test]
fn malformed_fields_are_refused() {
    let text = std::fs::read_to_string(format!("{FIXTURES}/stable.json")).unwrap();
    for (field, value) in [
        ("sha", "06A4060"),
        ("sha", "06a40601"),
        ("sha", "06a406g"),
        ("sha256", "81ce1245"),
        (
            "sha256",
            "81CE1245C381CF69A80D627853806331651FED06808598E34D8AF628102EA0EB",
        ),
        ("version", "0.1"),
        ("built_at", "2026-10-31 16:41:34"),
        ("built_at", "2026-13-31T16:41:34Z"),
        ("built_at", "2026-10-32T16:41:34Z"),
        ("built_at", "2026-10-31T24:41:34Z"),
        ("built_at", "2026-10-31T16:60:34Z"),
        ("built_at", "2026-10-31T16:41:60Z"),
        ("target", "linux-arm64"),
        ("channel", "nightly"),
    ] {
        let mut raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        raw[field] = serde_json::Value::from(value);
        let error = Manifest::parse(&raw.to_string()).unwrap_err();
        assert!(error.contains(field), "{field}={value} gave {error}");
    }
}

#[test]
fn every_pinned_key_parses_and_none_is_the_fixture_key() {
    for pinned in super::super::PINNED_KEYS {
        assert!(
            minisign_verify::PublicKey::from_base64(pinned).is_ok(),
            "pinned key does not parse: {pinned}"
        );
        assert!(*pinned != key(), "the fixture key must never ship as a pin");
    }
}

#[test]
fn an_empty_key_set_refuses() {
    assert_eq!(stable().verify(&[]).unwrap_err(), "no pinned update key");
}

fn v(raw: &str) -> semver::Version {
    semver::Version::parse(raw).unwrap()
}

#[test]
fn version_ordering_covers_equal_newer_older_and_prerelease() {
    let next = stable();
    assert_eq!(
        ordering(&v("0.1.3"), Some("06a4060"), &next),
        Ordering::UpToDate
    );
    assert_eq!(
        ordering(&v("0.1.2"), Some("06a4060"), &next),
        Ordering::Newer
    );
    assert_eq!(
        ordering(&v("0.1.4"), Some("06a4060"), &next),
        Ordering::Downgrade
    );
    assert_eq!(
        ordering(&v("0.1.3-rc.1"), Some("06a4060"), &next),
        Ordering::Newer,
        "a prerelease sorts below the release it precedes"
    );
}

#[test]
fn the_same_version_with_another_commit_is_not_up_to_date() {
    let next = stable();
    assert_eq!(
        ordering(&v("0.1.3"), Some("1111111"), &next),
        Ordering::SameVersionNewCommit
    );
    assert_eq!(
        ordering(&v("0.1.3"), None, &next),
        Ordering::UpToDate,
        "a build with no recorded commit cannot tell, so the version stands alone"
    );
}

#[test]
fn a_probe_version_is_read_past_the_binary_name_and_commit() {
    assert_eq!(probe_version("seavx 0.1.3 (06a4060)\n"), Some(v("0.1.3")));
    assert_eq!(probe_version("garbage\n"), None);
}

#[test]
fn a_digest_is_spelled_in_lowercase_hex() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        hex_lower(&Sha256::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_mismatched_checksum_names_both_values() {
    digest_matches("ABCDEF", "abcdef").unwrap();
    let error = digest_matches("aaaa", "bbbb").unwrap_err();
    assert!(error.contains("aaaa"), "{error}");
    assert!(error.contains("bbbb"), "{error}");
}

#[test]
fn canonical_bytes_are_the_frozen_v1_format() {
    assert_eq!(
        stable().signed_bytes(),
        include_str!("../../fixtures/update/manifest-stable.txt").as_bytes()
    );
}
