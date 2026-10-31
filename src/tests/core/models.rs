use super::*;

fn write_cache(root: &Path, slug: &str, fetched_at: u64, models: &[&str]) {
    let dir = root.join(".seavx/cache");
    std::fs::create_dir_all(&dir).unwrap();
    let cache = ModelCache {
        fetched_at,
        domain: slug.to_string(),
        models: models.iter().map(|model| model.to_string()).collect(),
    };
    std::fs::write(
        dir.join(format!("models-{slug}.json")),
        serde_json::to_string(&cache).unwrap(),
    )
    .unwrap();
}

fn models(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn expired_cache_feeds_the_picker_but_not_the_fetch_path() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(temp.path());
    let old = epoch_secs() - 2 * 24 * 3600;
    write_cache(temp.path(), "gw.example", old, &["alpha", "beta"]);

    assert_eq!(
        manager.get_cached_models("gw.example"),
        None,
        "a cache past its TTL must not satisfy the fetch path, or `seavx model` never refreshes"
    );
    assert_eq!(
        manager.get_cached_models_allow_stale("gw.example"),
        Some(models(&["alpha", "beta"])),
        "the picker prefers a stale list over an empty one"
    );
}

#[test]
fn fresh_cache_answers_both_readers() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(temp.path());
    write_cache(temp.path(), "gw.example", epoch_secs(), &["alpha"]);

    assert_eq!(
        manager.get_cached_models("gw.example"),
        Some(models(&["alpha"]))
    );
    assert_eq!(
        manager.get_cached_models_allow_stale("gw.example"),
        Some(models(&["alpha"]))
    );
}

#[test]
fn a_refused_credential_is_told_apart_from_a_bad_endpoint() {
    let refused = status_error(reqwest::StatusCode::UNAUTHORIZED);
    assert!(key_rejected(&refused), "{refused}");
    assert!(
        refused.contains("401"),
        "the status stays in the message the user reads: {refused}"
    );
    assert!(key_rejected(&status_error(reqwest::StatusCode::FORBIDDEN)));
    assert!(
        !key_rejected(&status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR)),
        "a 5xx may still be the URL, so the wizard must keep offering to save it"
    );
    assert!(
        !key_rejected(
            "HTTP request failed: error sending request for url (http://127.0.0.1:4010/v1/models)"
        ),
        "a port that happens to read 401 is not a refusal"
    );
}

#[test]
fn missing_or_unparsable_cache_answers_neither_reader() {
    let temp = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(temp.path());
    assert_eq!(manager.get_cached_models("gw.example"), None);
    assert_eq!(manager.get_cached_models_allow_stale("gw.example"), None);

    let dir = temp.path().join(".seavx/cache");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("models-gw.example.json"), "not json").unwrap();
    assert_eq!(manager.get_cached_models("gw.example"), None);
    assert_eq!(manager.get_cached_models_allow_stale("gw.example"), None);
}
