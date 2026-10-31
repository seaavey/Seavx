use super::*;
use std::path::Path;

/// `seavx setup` writes the global config unless the run asked for this
/// workspace: a workspace file shadows the global one, `api_key` included.
#[test]
fn the_wizard_targets_the_global_config_by_default() {
    let home = Path::new("/home/someone");
    let workspace = Path::new("/repo");

    assert_eq!(
        target_path(false, workspace, Some(home)).unwrap(),
        home.join(".seavx/config.toml")
    );
    assert_eq!(
        target_path(true, workspace, Some(home)).unwrap(),
        workspace.join(".seavx/config.toml")
    );
}

#[test]
fn no_home_only_blocks_the_global_target() {
    let workspace = Path::new("/repo");

    let error = target_path(false, workspace, None).unwrap_err();
    assert!(error.contains("home directory"), "{error}");
    assert_eq!(
        target_path(true, workspace, None).unwrap(),
        workspace.join(".seavx/config.toml")
    );
}
