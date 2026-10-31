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

/// A `/models` that refuses every credential, so the wizard has to re-ask.
fn refusing_endpoint() -> (String, std::thread::JoinHandle<()>) {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind endpoint");
    let port = listener.local_addr().expect("endpoint addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept /models");
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    (format!("http://127.0.0.1:{port}/v1/"), server)
}

#[test]
fn a_refused_key_is_asked_for_again_instead_of_saved() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let (base_url, server) = refusing_endpoint();

    let mut child = Command::new(env!("CARGO_BIN_EXE_seavx"))
        .arg("setup")
        .current_dir(workspace.path())
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seavx setup");

    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        // The URL, then an empty key: the endpoint refuses that credential, so
        // the key has to be asked for again before stdin reaches EOF.
        stdin
            .write_all(format!("{base_url}\n\n").as_bytes())
            .expect("answer the url and the key");
    }

    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("wait for setup").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`seavx setup` looped on a refused key instead of re-asking it");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = server.join();

    let mut output = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        let _ = stdout.by_ref().take(64 * 1024).read_to_string(&mut output);
    }
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.by_ref().take(64 * 1024).read_to_string(&mut output);
    }

    assert!(
        output.contains("the endpoint needs a key it accepts"),
        "a 401 is named as a credential problem: {output}"
    );
    assert!(
        output.contains("no more input on stdin while asking for API key"),
        "the wizard asked for the key again instead of moving on: {output}"
    );
    assert!(
        !output.contains("Model:"),
        "a refused key reached the model step: {output}"
    );
    assert!(
        !home.path().join(".seavx/config.toml").exists(),
        "a keyless provider was written: {output}"
    );
}
