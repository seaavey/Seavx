use super::*;
use std::path::PathBuf;

fn env_file(dir: &Path) -> PathBuf {
    dir.join(".env")
}

#[test]
fn an_explicit_value_is_parsed_and_nothing_else_is() {
    assert_eq!(debug_flag(Some("1")), Some(true));
    assert_eq!(debug_flag(Some("true")), Some(true));
    assert_eq!(debug_flag(Some(" 1 ")), Some(true));
    assert_eq!(debug_flag(Some("0")), Some(false));
    assert_eq!(debug_flag(Some("false")), Some(false));
    assert_eq!(debug_flag(None), None);
    assert_eq!(debug_flag(Some("")), None);
    assert_eq!(debug_flag(Some("yes")), None);
}

#[test]
fn a_missing_env_file_is_created_with_the_resolved_state() {
    let temp = tempfile::tempdir().unwrap();
    persist_debug_flag(temp.path(), false).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "SEAVX_DEBUG=false\n"
    );

    persist_debug_flag(temp.path(), true).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "SEAVX_DEBUG=true\n",
        "a second run rewrites the line instead of appending another"
    );
}

#[test]
fn only_the_debug_line_is_rewritten() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        env_file(temp.path()),
        "API_KEY=abc\nSEAVX_DEBUG=1\n# keep me\n",
    )
    .unwrap();

    persist_debug_flag(temp.path(), false).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "API_KEY=abc\nSEAVX_DEBUG=false\n# keep me\n"
    );
}

#[test]
fn an_env_file_without_the_key_gets_it_appended() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(env_file(temp.path()), "API_KEY=abc\n").unwrap();

    persist_debug_flag(temp.path(), true).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "API_KEY=abc\nSEAVX_DEBUG=true\n"
    );
}

#[test]
fn a_comment_or_a_longer_name_is_not_the_key() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(env_file(temp.path()), "# SEAVX_DEBUG=1\nSEAVX_DEBUGGER=1\n").unwrap();

    persist_debug_flag(temp.path(), false).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "# SEAVX_DEBUG=1\nSEAVX_DEBUGGER=1\nSEAVX_DEBUG=false\n",
        "neither the comment nor the longer name may be treated as the flag"
    );
}

#[test]
fn an_exported_form_of_the_key_is_rewritten_in_place() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(env_file(temp.path()), "export SEAVX_DEBUG=1\n").unwrap();

    persist_debug_flag(temp.path(), false).unwrap();
    assert_eq!(
        std::fs::read_to_string(env_file(temp.path())).unwrap(),
        "SEAVX_DEBUG=false\n"
    );
}

#[test]
fn a_yaml_block_scalar_loses_its_marker_and_keeps_its_text() {
    let folded = "---\ndescription: >\n  Multi-line folded text\n  that continues here.\nname: demo\n---\nbody";
    assert_eq!(
        frontmatter_value(folded, "description").as_deref(),
        Some("Multi-line folded text that continues here.")
    );

    let literal = "---\ndescription: |-\n  Line one.\n  Line two.\n---";
    assert_eq!(
        frontmatter_value(literal, "description").as_deref(),
        Some("Line one. Line two.")
    );

    // A marker with no body must not become the description.
    assert_eq!(frontmatter_value("description: >\n", "description"), None);
}
