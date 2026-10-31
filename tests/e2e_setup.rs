//! End-to-end: the shipped binary run the way a script or CI runs it — no
//! terminal, and no answer after the first prompt.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A regression here hangs instead of failing, so the wait has a deadline.
const LIMIT: Duration = Duration::from_secs(10);

#[test]
fn setup_with_closed_stdin_fails_instead_of_looping() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let mut child = Command::new(env!("CARGO_BIN_EXE_seavx"))
        .arg("setup")
        .current_dir(workspace.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seavx setup");

    let deadline = Instant::now() + LIMIT;
    let finished = loop {
        if child.try_wait().expect("wait for setup").is_some() {
            break true;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        finished,
        "`seavx setup` looped on closed stdin instead of failing"
    );

    // The child has exited, so both pipes reach EOF; cap what a runaway prints.
    let mut output = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        let _ = stdout.by_ref().take(64 * 1024).read_to_string(&mut output);
    }
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.by_ref().take(64 * 1024).read_to_string(&mut output);
    }

    let status = child.try_wait().expect("status after exit");
    assert!(
        matches!(status, Some(status) if !status.success()),
        "closed stdin is a usage error, not a success: {output}"
    );
    assert!(
        output.contains("no more input"),
        "the failure names the empty read: {output}"
    );
}
