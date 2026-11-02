//! End-to-end: the shipped binary run the way a script or CI runs it — no
//! terminal, and no answer after the first prompt.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A regression here hangs instead of failing, so the wait has a deadline.
const LIMIT: Duration = Duration::from_secs(10);

/// The wizard seeds its prompts from the config in effect, so a developer shell
/// that exports a key would leak into these runs.
fn setup_command(home: &std::path::Path, workspace: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seavx"));
    command
        .arg("setup")
        .current_dir(workspace)
        .env("HOME", home)
        .env_remove("SEAVX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("SEAVX_BASE_URL")
        .env_remove("SEAVX_MODEL");
    command
}

#[test]
fn setup_with_closed_stdin_fails_instead_of_looping() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let mut child = setup_command(home.path(), workspace.path())
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

    let output = drain(&mut child);
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

/// Both pipes reach EOF once the child has exited; cap what a runaway prints.
fn drain(child: &mut std::process::Child) -> String {
    let mut output = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        let _ = stdout.by_ref().take(64 * 1024).read_to_string(&mut output);
    }
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.by_ref().take(64 * 1024).read_to_string(&mut output);
    }
    output
}

/// A `/models` that refuses every credential, so the wizard has to offer its
/// exit instead of re-asking the key forever.
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
fn a_refused_key_offers_a_way_out_instead_of_re_asking_forever() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let (base_url, server) = refusing_endpoint();

    let mut child = setup_command(home.path(), workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seavx setup");

    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        // The URL, then an empty key. The 401 must end at the save prompt, not in
        // a loop: stdin stops here, so the run fails on that prompt, not on the
        // key prompt.
        stdin
            .write_all(format!("{base_url}\n\n").as_bytes())
            .expect("answer the url and the key");
    }

    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("wait for setup").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`seavx setup` looped on a refused key instead of offering an exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = server.join();

    let output = drain(&mut child);
    assert!(
        output.contains("the endpoint rejected that key"),
        "a 401 is named as a credential problem: {output}"
    );
    assert!(
        output.contains("no more input on stdin while asking for Save this endpoint anyway"),
        "the wizard reached the save-anyway prompt instead of re-asking the key: {output}"
    );
    assert!(
        !output.contains("Model:"),
        "a refused key reached the model step: {output}"
    );
    assert!(
        !home.path().join(".seavx/config.toml").exists(),
        "a keyless provider was written without being asked: {output}"
    );
}

#[test]
fn a_refused_key_can_be_saved_without_one() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let (base_url, server) = refusing_endpoint();

    let mut child = setup_command(home.path(), workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seavx setup");

    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        // url, empty key, then "y" at the save-anyway prompt, then the model and
        // an empty effort.
        stdin
            .write_all(format!("{base_url}\n\ny\nmy-model\n\n").as_bytes())
            .expect("answer the wizard");
    }

    // The config lands before the catalog refresh, which reaches the network and
    // is not what this test is about: stop at the file.
    let config_path = home.path().join(".seavx/config.toml");
    let deadline = Instant::now() + LIMIT;
    let written = loop {
        if let Ok(text) = std::fs::read_to_string(&config_path)
            && text.contains("base_url")
        {
            break text;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let output = drain(&mut child);
            panic!("keyless endpoint was not saved: {output}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = server.join();

    assert!(
        written.contains("my-model"),
        "the typed model was saved: {written}"
    );
    assert!(
        !written.contains("api_key"),
        "a keyless endpoint was saved with a key: {written}"
    );
}

#[test]
fn an_exported_key_is_offered_instead_of_none() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let (base_url, server) = refusing_endpoint();

    let mut child = setup_command(home.path(), workspace.path())
        .env("SEAVX_API_KEY", "sk-from-the-environment")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn seavx setup");

    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        // The URL and an empty answer: Enter keeps the key already in effect.
        stdin
            .write_all(format!("{base_url}\n\n").as_bytes())
            .expect("answer the url and keep the key");
    }

    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("wait for setup").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`seavx setup` never reached the save prompt");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = server.join();

    let output = drain(&mut child);
    assert!(
        output.contains("API key (optional) [****]"),
        "the exported key was offered instead of `none`: {output}"
    );
    assert!(
        output.contains("api key:   ****"),
        "the screen showed the key in effect: {output}"
    );
}
