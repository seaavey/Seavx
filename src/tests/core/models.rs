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

#[test]
fn the_catalog_keys_on_the_model_tail_and_keeps_the_largest_window() {
    let json = serde_json::json!({
        "novita": {"models": {
            "deepseek/deepseek-v4.1-flash-1017": {"limit": {"context": 1_048_576}},
            "deepseek/deepseek-v4.1-flash": {"limit": {"context": 1_000_000}},
            "vendor/Qwen3.8-27B:free": {"limit": {"context": 65_536}},
            "vendor/Qwen3.8-27B": {"limit": {"context": 120_000}},
            "no-limit": {},
            "zero-limit": {"limit": {"context": 0}}
        }}
    });

    let windows = catalog_windows(&json);
    assert_eq!(
        windows.get("deepseek-v4.1-flash-1017").copied(),
        Some(1_048_576)
    );
    assert_eq!(
        catalog_windows(&json).get("qwen3.8-27b").copied(),
        Some(120_000),
        "one model, two provider clamps: the largest window wins, and the `:free` suffix is not part of the name"
    );
    assert!(!windows.contains_key("no-limit"));
    assert!(
        !windows.contains_key("zero-limit"),
        "a zero window is no window"
    );
    assert_eq!(
        window_key("rv/deepseek-ai/DeepSeek-V4.1-Flash-1017"),
        "deepseek-v4.1-flash-1017"
    );
}

#[test]
fn the_cached_catalog_answers_a_gateway_id_and_expires_after_a_week() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join(".seavx/cache");
    std::fs::create_dir_all(&dir).unwrap();
    let windows = [("deepseek-v4.1-flash-1017".to_string(), 1_048_576)]
        .into_iter()
        .collect();
    let efforts = [(
        "deepseek-v4.1-flash-1017".to_string(),
        models(&["low", "high", "max"]),
    )]
    .into_iter()
    .collect();
    let cache = ContextCatalog {
        fetched_at: epoch_secs(),
        windows,
        efforts,
    };
    std::fs::write(
        dir.join("contexts.json"),
        serde_json::to_string(&cache).unwrap(),
    )
    .unwrap();

    assert!(
        !load_context_catalog(temp.path()),
        "a fresh cache is not due for a refresh"
    );
    assert_eq!(
        catalog_context("rv/deepseek-ai/DeepSeek-V4.1-Flash-1017"),
        Some(1_048_576),
        "the gateway lists a routed id; the catalog knows the model behind it"
    );
    assert_eq!(
        catalog_context("deepseek-chat"),
        None,
        "an id the catalog does not know must fall back to the family table"
    );
    assert_eq!(
        catalog_efforts("rv/deepseek-ai/DeepSeek-V4.1-Flash-1017"),
        Some(models(&["low", "high", "max"])),
        "the effort picker reads the same key"
    );
    assert_eq!(
        crate::agent::engine::effort_levels(Some("rv/deepseek-ai/DeepSeek-V4.1-Flash-1017")),
        models(&["minimal", "high", "max"]),
        "the menu speaks the names the config stores: `low` is written as `minimal`"
    );
    assert_eq!(
        crate::agent::engine::effort_levels(Some("no-catalog-entry-for-this-model")),
        models(&["none", "minimal", "medium", "high", "highx", "max"]),
        "an id the catalog does not list keeps the static menu"
    );
    assert_eq!(
        catalog_efforts("deepseek-chat"),
        None,
        "an unknown id keeps the static level list"
    );

    // A cache written before the catalog kept effort levels: it loads, but the
    // picker would answer nothing, so one refresh is due.
    std::fs::write(
        dir.join("contexts.json"),
        serde_json::json!({"fetched_at": epoch_secs(), "windows": {}}).to_string(),
    )
    .unwrap();
    assert!(
        load_context_catalog(temp.path()),
        "a cache without effort levels asks for a refresh"
    );

    let stale = ContextCatalog {
        fetched_at: epoch_secs() - 8 * 24 * 3600,
        windows: BTreeMap::new(),
        efforts: BTreeMap::new(),
    };
    std::fs::write(
        dir.join("contexts.json"),
        serde_json::to_string(&stale).unwrap(),
    )
    .unwrap();
    assert!(
        load_context_catalog(temp.path()),
        "past the TTL a refresh is due"
    );
    assert!(
        load_context_catalog(&temp.path().join("nowhere")),
        "no cache at all is a refresh too"
    );
}

#[test]
fn the_catalog_keeps_the_effort_levels_a_model_accepts() {
    let json = serde_json::json!({
        "one": {"models": {
            "deepseek/deepseek-v4.1-flash-1017": {"reasoning_options": [
                {"type": "toggle"},
                {"type": "effort", "values": ["low", "high", "max"]}
            ]}
        }},
        "two": {"models": {
            "vendor/deepseek-v4.1-flash-1017": {"reasoning_options": [
                {"type": "effort", "values": ["none", "high", "xhigh", "max"]}
            ]},
            "vendor/Qwen3.8-27B": {"reasoning_options": [
                {"type": "effort", "values": ["default", "none", "low", "medium", "high", "xhigh", "max"]},
                {"type": "budget_tokens", "max": 262_144}
            ]},
            "toggle-only": {"reasoning_options": [{"type": "toggle"}]},
            "no-options": {}
        }}
    });

    let levels = catalog_effort_levels(&json);
    assert_eq!(
        levels.get("deepseek-v4.1-flash-1017"),
        Some(&models(&["none", "low", "high", "xhigh", "max"])),
        "two gateways clamp one model differently: the union wins, in menu order"
    );
    assert_eq!(
        levels.get("qwen3.8-27b"),
        Some(&models(&["none", "low", "medium", "high", "xhigh", "max"])),
        "`default` is not a level, and a budget option is not one either"
    );
    assert!(
        !levels.contains_key("toggle-only") || levels["toggle-only"].is_empty(),
        "a model with no effort option stays out, so the static list answers"
    );
    assert!(!levels.contains_key("no-options"));
}
