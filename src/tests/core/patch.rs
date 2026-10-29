use crate::agent::patch::{FilePatch, Hunk, HunkLine, apply_to, parse};
use std::path::PathBuf;

fn hunk(
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: &[&str],
) -> Hunk {
    Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: lines
            .iter()
            .map(|line| {
                let mut chars = line.chars();
                let marker = chars.next().unwrap_or_default();
                let text = chars.as_str().to_string();
                match marker {
                    ' ' => HunkLine::Context(text),
                    '-' => HunkLine::Remove(text),
                    '+' => HunkLine::Add(text),
                    _ => panic!("bad fixture line {line}"),
                }
            })
            .collect(),
        no_newline: false,
    }
}

#[test]
fn parses_a_single_hunk_with_optional_prefix() {
    let diff = "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -2,3 +2,4 @@\n ctx\n-old\n+new\n+extra\n ctx\n";
    let files = parse(diff).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PathBuf::from("src/x.rs"));
    assert_eq!(files[0].hunks[0].old_start, 2);
    assert_eq!(files[0].hunks[0].old_count, 3);
    assert_eq!(files[0].hunks[0].new_count, 4);
}

#[test]
fn parses_a_creation_header_and_no_newline_marker() {
    let diff =
        "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+hello\n\\ No newline at end of file\n";
    let files = parse(diff).unwrap();
    assert!(files[0].create);
    assert_eq!(files[0].hunks[0].old_count, 0);
    assert!(files[0].hunks[0].no_newline);
}

#[test]
fn rejects_malformed_diffs() {
    for diff in [
        "",
        "@@ -1,1 +1,1 @@\n-a\n",
        "--- a/x\n+++ b/x\n@@ -x +1 @@\n-a\n+b\n",
        "--- a/../x\n+++ b/../x\n@@ -1,1 +1,1 @@\n-a\n+b\n",
        "--- a//etc/passwd\n+++ b//etc/passwd\n@@ -1,1 +1,1 @@\n-a\n+b\n",
    ] {
        assert!(parse(diff).is_err(), "{diff:?}");
    }
}

#[test]
fn applies_multiple_hunks_and_reports_counts() {
    let patch = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![
            hunk(2, 1, 2, 1, &["-b", "+B"]),
            hunk(4, 1, 4, 1, &["-d", "+D"]),
        ],
    };
    let (out, stats) = apply_to("a\nb\nc\nd\n", &patch).unwrap();
    assert_eq!(out, "a\nB\nc\nD\n");
    assert_eq!((stats.added, stats.removed, stats.hunks), (2, 2, 2));
}

#[test]
fn applies_a_creation_patch() {
    let diff = "--- /dev/null\n+++ b/c.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n";
    let files = parse(diff).unwrap();
    let (out, stats) = apply_to("", &files[0]).unwrap();
    assert_eq!(out, "one\ntwo\n");
    assert_eq!(stats.added, 2);
}

#[test]
fn a_context_mismatch_names_the_file_hunk_and_line() {
    let patch = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![hunk(2, 1, 2, 1, &["-b", "+B"])],
    };
    let error = apply_to("a\nx\nc\n", &patch).unwrap_err();
    assert!(error.contains("f.txt"), "{error}");
    assert!(error.contains("@@ -2,1 +2,1 @@"), "{error}");
    assert!(error.contains("line 2"), "{error}");
}

#[test]
fn a_wrong_line_number_is_an_error_even_when_the_text_exists() {
    let patch = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![hunk(9, 1, 9, 1, &["-b", "+B"])],
    };
    assert!(apply_to("a\nb\nc\nd\n", &patch).is_err());
}

#[test]
fn reversed_or_overlapping_hunks_are_refused() {
    let reversed = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![
            hunk(4, 1, 4, 1, &["-d", "+D"]),
            hunk(2, 1, 2, 1, &["-b", "+B"]),
        ],
    };
    let error = apply_to("a\nb\nc\nd\n", &reversed).unwrap_err();
    assert!(error.contains("out of order"), "{error}");

    let overlapping = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![
            hunk(2, 1, 2, 1, &["-b", "+B"]),
            hunk(2, 1, 2, 1, &["-b", "+b"]),
        ],
    };
    let error = apply_to("a\nb\nc\nd\n", &overlapping).unwrap_err();
    assert!(error.contains("overlap"), "{error}");
}

#[test]
fn crlf_content_with_lf_context_is_refused_naming_line_endings() {
    let patch = FilePatch {
        path: PathBuf::from("f.txt"),
        create: false,
        hunks: vec![hunk(2, 1, 2, 1, &["-b", "+B"])],
    };
    let error = apply_to("a\r\nb\r\n", &patch).unwrap_err();
    assert!(error.contains("line endings"), "{error}");
}
