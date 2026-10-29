use super::*;
use crate::agent::engine::AgentConfig;
use std::fs;
use tempfile::tempdir;

#[test]
fn set_preserves_comments_and_unknown_keys() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(".seavx/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "# keep me\nmodel = \"old\"\ncustom = 1\nmax_turns = 10\n",
    )
    .unwrap();

    set_value(&path, ConfigKey::Model, "new-model").unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# keep me"));
    assert!(text.contains("custom = 1"));
    assert!(text.contains("model = \"new-model\""));
}

#[test]
fn rejects_invalid_values() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("config.toml");
    assert!(set_value(&path, ConfigKey::Model, "   ").is_err());
    assert!(set_value(&path, ConfigKey::ReasoningEffort, "turbo").is_err());
    assert!(set_value(&path, ConfigKey::MaxTurns, "0").is_err());
    assert!(set_value(&path, ConfigKey::MaxTurns, "10001").is_err());
    assert!(set_value(&path, ConfigKey::BaseUrl, "not a url").is_err());
    assert!(set_value(&path, ConfigKey::ApiKey, "sr-live-12345678").is_err());
    assert!(!path.exists());
}

#[test]
fn parse_errors_do_not_echo_the_offending_source() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("config.toml");
    fs::write(&path, "api_key = \"sr-live-12345678\n").unwrap();

    let error = set_value(&path, ConfigKey::Model, "m").unwrap_err();
    assert!(!error.contains("sr-live-12345678"), "{error}");
}

#[test]
fn set_on_missing_file_writes_a_loadable_config() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(".seavx/config.toml");
    set_value(&path, ConfigKey::Model, "new-model").unwrap();

    let (config, report) = AgentConfig::load_with_report_env(temp.path(), |_| None);
    assert!(report.parse_error.is_none(), "{:?}", report.parse_error);
    assert_eq!(config.model, "new-model");
    assert_eq!(config.max_turns, 1000);
}

#[test]
fn system_prompt_keeps_spaces_between_words() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("config.toml");
    set_value(&path, ConfigKey::SystemPrompt, "be nice to me").unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("be nice to me"));
}

#[test]
fn unset_removes_only_the_target_key() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("config.toml");
    set_value(&path, ConfigKey::Model, "m").unwrap();
    set_value(&path, ConfigKey::MaxTurns, "5").unwrap();
    assert!(unset_value(&path, ConfigKey::Model).unwrap());
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("model"));
    assert!(text.contains("max_turns = 5"));
    assert!(!unset_value(&path, ConfigKey::Model).unwrap());
}

#[test]
fn mask_keeps_only_the_last_four_characters() {
    assert_eq!(mask_key("sr-live-12345678"), "set (...5678)");
    assert!(mask_key("short").contains("****"));
}

#[test]
fn report_renders_origins_and_masks_the_key() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(".seavx/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "model = \"workspace-model\"\nmax_turns = 5\napi_key = \"sr-live-12345678\"\n",
    )
    .unwrap();
    let (config, report) = AgentConfig::load_with_report_env(temp.path(), |_| None);
    let text = render_report(&config, &report);
    assert!(text.contains("(workspace)"));
    assert!(text.contains("...5678"));
    assert!(!text.contains("sr-live-12345678"));
}

#[test]
fn init_creates_the_config_once() {
    let temp = tempdir().unwrap();
    let (path, created) = init_workspace(temp.path()).unwrap();
    assert!(created);
    assert!(path.exists());
    let (_, created_again) = init_workspace(temp.path()).unwrap();
    assert!(!created_again);
}

#[cfg(unix)]
#[test]
fn written_config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let path = temp.path().join("config.toml");
    set_value(&path, ConfigKey::Model, "m").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
