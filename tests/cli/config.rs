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

// ---- Crash-recoverable config writer (phase 0.5, task 4) -------------------

fn config_file(dir: &Path) -> PathBuf {
    let path = dir.join(".seavx/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

/// Plant the pair a crashed replace leaves behind: `.bak-<nanos>-<len>` plus the
/// `.new` record naming the bytes that replace was writing.
fn plant_backup(path: &Path, previous: &[u8], next: &[u8]) -> PathBuf {
    plant_backup_at(path, 1000, previous, next)
}

fn plant_backup_at(path: &Path, stamp: u64, previous: &[u8], next: &[u8]) -> PathBuf {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let backup = path.with_file_name(format!("{name}.bak-{stamp}-0-{}", previous.len()));
    fs::write(&backup, previous).unwrap();
    let record = format!("{}\t{}\n", next.len(), crate::checkpoint::fnv1a64(next));
    fs::write(PathBuf::from(format!("{}.new", backup.display())), record).unwrap();
    backup
}

fn stash_written_config(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn config_writer_preserves_comments_and_unknown_keys() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(
        &path,
        "# keep me\nmodel = \"old\"\nmax_turns = 10\ncustom = 1\n",
    )
    .unwrap();

    update_config(&path, |document| {
        document["model"] = toml_edit::value("new-model");
        Ok(())
    })
    .unwrap();

    let text = stash_written_config(&path);
    assert!(text.contains("# keep me"), "{text}");
    assert!(text.contains("custom = 1"), "{text}");
    assert!(text.contains("model = \"new-model\""), "{text}");

    // The whole-config write used by `seavx model`/`seavx setup` keeps them too.
    let config = crate::agent::engine::AgentConfig {
        model: "third".to_string(),
        max_turns: 11,
        base_url: None,
        api_key: None,
        reasoning_effort: None,
        system_prompt: None,
    };
    write_agent_config(&path, &config).unwrap();
    let text = stash_written_config(&path);
    assert!(text.contains("# keep me"), "{text}");
    assert!(text.contains("custom = 1"), "{text}");
    assert!(text.contains("model = \"third\""), "{text}");
    assert!(text.contains("max_turns = 11"), "{text}");
}

#[test]
fn config_writer_validates_all_changes_before_write() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    let original = "model = \"old\"\nmax_turns = 10\n";
    fs::write(&path, original).unwrap();

    let error = update_config(&path, |document| {
        document["model"] = toml_edit::value("changed");
        Err("invalid combination".to_string())
    })
    .unwrap_err();

    assert!(error.contains("invalid combination"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn set_model_and_effort_writes_both_fields_or_neither() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    let original = "model = \"old\"\nmax_turns = 10\n";
    fs::write(&path, original).unwrap();

    let error = set_model_and_effort(&path, "new-model", Some("turbo")).unwrap_err();
    assert!(error.contains("reasoning_effort"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    set_model_and_effort(&path, "new-model", Some("high")).unwrap();
    let text = stash_written_config(&path);
    assert!(text.contains("model = \"new-model\""), "{text}");
    assert!(text.contains("reasoning_effort = \"high\""), "{text}");
}

#[cfg(unix)]
#[test]
fn legacy_config_0644_becomes_0600_while_source_0644_stays_0644() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();

    let config = config_file(temp.path());
    fs::write(&config, "model = \"old\"\nmax_turns = 10\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
    set_value(&config, ConfigKey::Model, "new").unwrap();
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600,
        "Force applies to config writes"
    );

    let source = temp.path().join("src.rs");
    fs::write(&source, "fn main() {}\n").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
    crate::atomic_file::write_atomic(
        &source,
        b"fn main() { }\n",
        crate::atomic_file::ModePolicy::Preserve,
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o644,
        "Preserve keeps an ordinary file's mode"
    );
}

#[cfg(unix)]
#[test]
fn config_writer_tightens_0644_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"old\"\nmax_turns = 10\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    set_value(&path, ConfigKey::Model, "new").unwrap();

    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn config_writer_never_echoes_url_userinfo_or_query() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());

    let shown = set_value(
        &path,
        ConfigKey::BaseUrl,
        "https://user:pass@host/v1?token=abc123",
    )
    .unwrap();
    assert!(!shown.contains("user:pass"), "{shown}");
    assert!(!shown.contains("pass"), "{shown}");
    assert!(!shown.contains("abc123"), "{shown}");
    assert!(shown.contains("host"), "{shown}");
    // The stored value keeps the credentials the endpoint needs.
    assert!(stash_written_config(&path).contains("host"));

    let error =
        set_value(&path, ConfigKey::BaseUrl, "https://user:pass@?token=abc123").unwrap_err();
    assert!(!error.contains("user:pass"), "{error}");
    assert!(!error.contains("abc123"), "{error}");
}

#[test]
fn config_writer_rechecks_target_before_remove() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"old\"\n").unwrap();

    let error = replace_config_with(
        &path,
        b"model = \"new\"\n",
        Some(Fault::ChangeTargetDuringWrite),
    )
    .unwrap_err();

    assert!(error.contains("config changed during write"), "{error}");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("concurrent = true"), "{text}");
}

#[test]
fn config_recovery_handles_new_old_missing_and_both_missing() {
    let temp = tempdir().unwrap();

    // Target already holds the new bytes: the stale backup is dropped.
    let path = config_file(temp.path());
    fs::write(&path, "model = \"new\"\n").unwrap();
    let backup = plant_backup(&path, b"model = \"old\"\n", b"model = \"new\"\n");
    recover_config(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "model = \"new\"\n");
    assert!(!backup.exists(), "a landed replace drops its backup");

    // Target still holds the old bytes: the replace never landed.
    let path = config_file(&temp.path().join("old"));
    fs::write(&path, "model = \"old\"\n").unwrap();
    let backup = plant_backup(&path, b"model = \"old\"\n", b"model = \"new\"\n");
    recover_config(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "model = \"old\"\n");
    assert!(!backup.exists());

    // Target removed by the crash: restore it from the backup.
    let path = config_file(&temp.path().join("missing"));
    fs::write(&path, "model = \"old\"\n").unwrap();
    plant_backup(&path, b"model = \"old\"\n", b"model = \"new\"\n");
    fs::remove_file(&path).unwrap();
    recover_config(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "model = \"old\"\n");

    // Both gone: report instead of inventing a config.
    let path = config_file(&temp.path().join("both-gone"));
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    fs::create_dir(path.with_file_name(format!("{name}.bak-1000-5"))).unwrap();
    let error = recover_config(&path).unwrap_err();
    assert!(error.contains("config unavailable"), "{error}");
    assert!(!path.exists());
}

#[test]
fn config_recovery_keeps_third_state_and_backup() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"third\"\n").unwrap();
    let backup = plant_backup(&path, b"model = \"old\"\n", b"model = \"new\"\n");

    let error = recover_config(&path).unwrap_err();

    assert!(error.contains("config recovery conflict"), "{error}");
    assert!(error.contains(&backup.display().to_string()), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "model = \"third\"\n");
    assert!(backup.exists(), "the backup is kept for the user");
    assert!(
        PathBuf::from(format!("{}.new", backup.display())).exists(),
        "the record of the interrupted write is kept"
    );
}

#[test]
fn config_writer_conflicts_if_target_changes_after_final_recheck() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"old\"\n").unwrap();

    let error = replace_config_with(
        &path,
        b"model = \"new\"\n",
        Some(Fault::ThirdStateAfterRemove),
    )
    .unwrap_err();

    assert!(error.contains("config recovery conflict"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "third = 3\n");
    let backups: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.contains(".bak-") && !name.ends_with(".new")
        })
        .collect();
    assert_eq!(backups.len(), 1, "the backup is kept");
}

#[test]
fn config_writer_faults_leave_old_or_new() {
    let temp = tempdir().unwrap();
    let old = b"model = \"old\"\n";
    let new = b"model = \"new\"\n";

    for fault in [Fault::BackupCreate, Fault::TargetRemove, Fault::Rename] {
        let path = config_file(&temp.path().join(format!("{fault:?}")));
        fs::write(&path, old).unwrap();

        let result = replace_config_with(&path, new, Some(fault));

        let text = fs::read(&path).unwrap();
        assert!(
            text == old || text == new,
            "{fault:?} left neither the old nor the new config: {text:?}"
        );
        assert!(result.is_err(), "{fault:?} must not report success");
        assert!(
            fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains("seavx-tmp")),
            "{fault:?} left a staging temp behind"
        );
    }

    // A failing backup cleanup keeps the new config and the stale backup.
    let path = config_file(&temp.path().join("cleanup"));
    fs::write(&path, old).unwrap();
    replace_config_with(&path, new, Some(Fault::BackupCleanup)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), new);
    assert!(
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".bak-")),
        "the backup survives a failed cleanup"
    );
}

fn backup_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".bak-") && !name.ends_with(".new"))
        .collect()
}

#[test]
fn config_writer_keeps_a_concurrent_third_state_after_rename() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"old\"\n").unwrap();

    let error = replace_config_with(
        &path,
        b"model = \"new\"\n",
        Some(Fault::ThirdStateAfterRename),
    )
    .unwrap_err();

    assert!(error.contains("config recovery conflict"), "{error}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "third = 3\n",
        "a write that landed after the rename is never overwritten"
    );
    assert_eq!(
        backup_names(path.parent().unwrap()).len(),
        1,
        "the previous bytes stay available"
    );
}

#[test]
fn config_recovery_prunes_older_backups() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"new\"\n").unwrap();
    let older = plant_backup_at(&path, 1000, b"model = \"older\"\n", b"model = \"new\"\n");
    let newer = plant_backup_at(&path, 2000, b"model = \"old\"\n", b"model = \"new\"\n");

    recover_config(&path).unwrap();

    assert!(!newer.exists(), "the resolved backup is dropped");
    assert!(
        !older.exists(),
        "an older leftover from an earlier crash is pruned too"
    );
}

#[test]
fn config_backup_names_do_not_collide_within_one_millisecond() {
    let temp = tempdir().unwrap();
    let path = config_file(temp.path());
    fs::write(&path, "model = \"old\"\n").unwrap();

    for _ in 0..50 {
        write_backup(&path, b"model = \"old\"\n", b"model = \"new\"\n").unwrap();
    }

    assert_eq!(
        backup_names(path.parent().unwrap()).len(),
        50,
        "every write keeps its own backup, even in the same millisecond"
    );
}
