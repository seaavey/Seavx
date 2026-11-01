use super::*;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/fixtures/update");
const FIXTURE_KEY: &str = include_str!("../fixtures/update/fixture.pub");

/// The public half of the throwaway fixture key, one line below its comment.
fn key() -> &'static str {
    FIXTURE_KEY.lines().nth(1).unwrap().trim()
}

#[test]
fn the_base_url_keeps_one_slash() {
    assert_eq!(
        resolve_base(Some("http://127.0.0.1:8080/".to_string())),
        "http://127.0.0.1:8080"
    );
    assert_eq!(resolve_base(None), "https://cdn.seavx.com");
}

#[test]
fn the_pointer_path_is_channel_plus_target() {
    let linux = Target::from_name("linux-x86_64").unwrap();
    let macos = Target::from_name("macos-arm64").unwrap();

    assert_eq!(
        Channel::Stable.pointer_path(&linux),
        "latest/linux/x86_64.json"
    );
    assert_eq!(Channel::Edge.pointer_path(&linux), "edge/linux/x86_64.json");
    assert_eq!(Channel::Edge.pointer_path(&macos), "edge/macos/arm64.json");
}

#[test]
fn an_unknown_channel_is_a_usage_error() {
    use clap::Parser;

    let error =
        crate::Cli::try_parse_from(["seavx", "update", "--channel", "nightly"]).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("stable"), "{message}");
    assert!(message.contains("edge"), "{message}");
}

#[test]
fn the_cli_defaults_to_stable_and_accepts_edge() {
    use clap::Parser;

    let cli = crate::Cli::try_parse_from(["seavx", "update"]).unwrap();
    match cli.command {
        Some(crate::Commands::Update { channel, .. }) => assert_eq!(channel, "stable"),
        other => panic!("expected update, got {other:?}"),
    }

    let cli = crate::Cli::try_parse_from(["seavx", "update", "--channel", "edge"]).unwrap();
    match cli.command {
        Some(crate::Commands::Update { channel, .. }) => assert_eq!(channel, "edge"),
        other => panic!("expected update, got {other:?}"),
    }
}

/// Both installers pick the artifact by hand, so a name that drifts from
/// `Target::binary` 404s a real install: every name they spell must be one the
/// table publishes, and every published target must be spelled by one of them.
#[test]
fn the_installers_name_only_published_binaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scripts: Vec<(&str, String)> = ["install.sh", "install.ps1"]
        .iter()
        .map(|name| (*name, std::fs::read_to_string(root.join(name)).unwrap()))
        .collect();

    for (name, script) in &scripts {
        for binary in binaries_named(script) {
            assert!(
                manifest::TARGETS
                    .iter()
                    .any(|target| target.binary == binary),
                "{name} names {binary}, which no target publishes"
            );
        }
    }
    for target in manifest::TARGETS {
        assert!(
            scripts
                .iter()
                .any(|(_, script)| binaries_named(script).contains(&target.binary)),
            "no installer names {}, so that platform cannot install",
            target.binary
        );
    }

    for (name, script) in &scripts {
        if PINNED_KEYS.is_empty() {
            // While the pin is empty the TODO is the only thing tying the three
            // files together, so a key pasted into the client alone still fails.
            assert!(
                script.contains("TODO(update): paste the Task 0 release public key here"),
                "{name} lost the pinned-key TODO"
            );
        } else {
            for key in PINNED_KEYS {
                assert!(
                    script.contains(*key),
                    "{name} does not carry pinned key {key}"
                );
            }
        }
    }
}

/// The `seavx-` artifact names a script spells, up to the closing quote.
fn binaries_named(script: &str) -> Vec<&str> {
    script
        .match_indices("seavx-")
        .map(|(at, _)| {
            let tail = &script[at..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
                .unwrap_or(tail.len());
            &tail[..end]
        })
        .collect()
}

/// The whole ordered path against a local server: the fixture pointers name
/// linux-x86_64, so these run only where `Target::detect` answers that.
#[cfg(all(unix, target_os = "linux", target_arch = "x86_64"))]
mod end_to_end {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const STABLE_POINTER: &str = "latest/linux/x86_64.json";
    const EDGE_POINTER: &str = "edge/linux/x86_64.json";
    /// The route both fixture pointers sign for their artifact.
    const ARTIFACT: &str = "builds/06a4060/81ce1245c381cf69a80d627853806331651fed06808598e34d8af628102ea0eb/seavx-linux-x86_64";

    /// Installs run one at a time: each one execs a freshly written file, and two
    /// in flight make `spawn` fail with `ETXTBSY` on the sibling's open handle.
    static INSTALL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn bytes(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
    }

    /// A stable pointer plus its artifact, the shape a real update request takes.
    fn stable_routes() -> HashMap<String, Vec<u8>> {
        HashMap::from([
            (STABLE_POINTER.to_string(), bytes("stable.json")),
            (ARTIFACT.to_string(), bytes("fake-artifact.sh")),
        ])
    }

    /// Every path answers its body with 200 and anything else with 404; the
    /// counter is what "the artifact was never requested" asserts on.
    async fn serve(
        routes: HashMap<String, Vec<u8>>,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0u8; 2048];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches('/');
                match routes.get(path) {
                    Some(body) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(body).await;
                    }
                    None => {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                            )
                            .await;
                    }
                }
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), handle, requests)
    }

    struct Fixture {
        dir: tempfile::TempDir,
        exe: PathBuf,
        base: String,
        requests: Arc<AtomicUsize>,
        _server: tokio::task::JoinHandle<()>,
    }

    impl Fixture {
        /// `old` is the byte string the installed binary holds before the update.
        async fn new(routes: HashMap<String, Vec<u8>>, old: &[u8]) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join("seavx");
            std::fs::write(&exe, old).unwrap();
            let (base, _server, requests) = serve(routes).await;
            Fixture {
                dir,
                exe,
                base,
                requests,
                _server,
            }
        }

        fn installed(&self) -> Vec<u8> {
            std::fs::read(&self.exe).unwrap()
        }

        /// Any file the updater parked next to the binary.
        fn leftovers(&self) -> Vec<String> {
            std::fs::read_dir(self.dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(".seavx-update-"))
                .collect()
        }

        fn pointer_requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    fn current(version: &str) -> Current {
        Current {
            version: semver::Version::parse(version).unwrap(),
            sha: None,
        }
    }

    /// The stable pointer with one field rewritten, signature left as published.
    fn pointer(change: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
        let mut raw: serde_json::Value = serde_json::from_slice(&bytes("stable.json")).unwrap();
        change(&mut raw);
        raw.to_string().into_bytes()
    }

    #[tokio::test]
    async fn an_up_to_date_binary_is_left_alone() {
        let _install = INSTALL.lock().await;
        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;

        install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.3"),
            &[key()],
        )
        .await
        .unwrap();

        assert_eq!(fixture.pointer_requests(), 1, "nothing was downloaded");
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn an_unwritable_install_directory_fails_and_leaves_the_binary_alone() {
        use std::os::unix::fs::PermissionsExt;
        let _install = INSTALL.lock().await;

        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;
        std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o555))
            .unwrap();

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        // Restore before asserting so a failure still lets the temp dir go.
        std::fs::set_permissions(fixture.dir.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        let message = error.to_string();
        assert!(
            message.contains(&fixture.dir.path().display().to_string()),
            "{message}"
        );
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    /// The guard's job at a late failure: the replace target is a directory, so a
    /// real temp file exists on disk when `replace_exe` fails.
    #[tokio::test]
    async fn a_failed_replace_leaves_no_temp_file() {
        let _install = INSTALL.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("seavx");
        std::fs::create_dir(&exe).unwrap();
        let (base, server, requests) = serve(stable_routes()).await;
        let fixture = Fixture {
            dir,
            exe,
            base,
            requests,
            _server: server,
        };

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("replace"), "{error}");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
        assert!(fixture.exe.is_dir(), "the target is untouched");
    }

    /// An executable shell script in `dir`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn the_probe_times_out_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "slow", "#!/bin/sh\nsleep 5\n");

        let error = probe_exe(&bin, Duration::from_millis(50))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("--version"), "{error}");
    }

    #[tokio::test]
    async fn a_non_zero_probe_exit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "broken", "#!/bin/sh\nexit 3\n");

        let error = probe_exe(&bin, Duration::from_secs(5)).await.unwrap_err();

        assert!(error.to_string().contains('3'), "{error}");
    }

    #[tokio::test]
    async fn a_probe_that_disagrees_with_the_manifest_is_refused() {
        let _install = INSTALL.lock().await;
        // Signed for 0.9.9 over bytes that report 0.1.3: the digest is right, the
        // build is not the one the pointer names.
        let mut routes = stable_routes();
        routes.insert(
            STABLE_POINTER.to_string(),
            bytes("stable-pointer-0.9.9.json"),
        );
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("0.9.9") && message.contains("0.1.3"),
            "{message}"
        );
        assert_eq!(fixture.installed(), b"old binary\n");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
    }

    /// `--force` relaxes the version ordering and nothing else.
    #[tokio::test]
    async fn force_does_not_skip_verification() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(
            STABLE_POINTER.to_string(),
            pointer(|raw| raw["version"] = "0.1.4".into()),
        );
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            true,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("signature"), "{error}");
        assert_eq!(fixture.pointer_requests(), 1);
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn a_forced_update_installs_an_up_to_date_build() {
        let _install = INSTALL.lock().await;
        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;

        install(
            true,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.3"),
            &[key()],
        )
        .await
        .unwrap();

        assert_eq!(fixture.installed(), bytes("fake-artifact.sh"));
        assert_eq!(fixture.pointer_requests(), 2, "pointer, then artifact");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
    }

    #[tokio::test]
    async fn a_forced_downgrade_installs_the_older_build() {
        let _install = INSTALL.lock().await;
        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;

        install(
            true,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.2.0"),
            &[key()],
        )
        .await
        .unwrap();

        assert_eq!(fixture.installed(), bytes("fake-artifact.sh"));
        assert_eq!(fixture.pointer_requests(), 2, "pointer, then artifact");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
    }

    #[tokio::test]
    async fn a_downgrade_is_refused_without_force() {
        let _install = INSTALL.lock().await;
        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.2.0"),
            &[key()],
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("0.2.0") && message.contains("0.1.3"),
            "{message}"
        );
        assert_eq!(
            fixture.pointer_requests(),
            1,
            "the artifact was not fetched"
        );
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn a_pointer_for_another_target_is_refused_before_any_download() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(EDGE_POINTER.to_string(), bytes("edge.json"));
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Edge,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("target")
                && message.contains("macos-arm64")
                && message.contains("linux-x86_64"),
            "{message}"
        );
        assert_eq!(fixture.pointer_requests(), 1);
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn a_pointer_for_another_channel_is_refused_before_any_download() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(STABLE_POINTER.to_string(), bytes("edge.json"));
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("edge") && message.contains("stable"),
            "{message}"
        );
        assert_eq!(fixture.pointer_requests(), 1);
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    /// The signature is checked before the channel binding: a pointer for the
    /// wrong channel whose signature is also broken must be refused as unsigned.
    #[tokio::test]
    async fn a_wrong_channel_pointer_with_a_broken_signature_is_refused_as_unsigned() {
        let _install = INSTALL.lock().await;
        let mut edge: serde_json::Value = serde_json::from_slice(&bytes("edge.json")).unwrap();
        edge["version"] = "0.1.4".into();
        let mut routes = stable_routes();
        routes.insert(STABLE_POINTER.to_string(), edge.to_string().into_bytes());
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("signature"), "{message}");
        assert!(
            !message.contains("channel"),
            "the binding was checked before the signature: {message}"
        );
        assert_eq!(fixture.pointer_requests(), 1);
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn an_unsigned_pointer_is_refused() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(
            STABLE_POINTER.to_string(),
            pointer(|raw| {
                raw.as_object_mut().unwrap().remove("sig");
            }),
        );
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("sig"), "{error}");
        assert_eq!(fixture.pointer_requests(), 1);
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn a_modified_pointer_is_refused() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(
            STABLE_POINTER.to_string(),
            pointer(|raw| raw["version"] = "0.1.4".into()),
        );
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("signature"), "{error}");
        assert_eq!(
            fixture.pointer_requests(),
            1,
            "a forged pointer must not reach the artifact"
        );
        assert_eq!(fixture.installed(), b"old binary\n");
    }

    #[tokio::test]
    async fn a_tampered_artifact_leaves_the_binary_and_no_temp_file() {
        let _install = INSTALL.lock().await;
        let mut routes = stable_routes();
        routes.insert(
            ARTIFACT.to_string(),
            b"#!/bin/sh\necho 'seavx 0.1.3 (06a4060)'\n".to_vec(),
        );
        let fixture = Fixture::new(routes, b"old binary\n").await;

        let error = install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("checksum"), "{error}");
        assert_eq!(fixture.installed(), b"old binary\n");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
    }

    #[tokio::test]
    async fn a_signed_pointer_installs_the_artifact() {
        let _install = INSTALL.lock().await;
        let fixture = Fixture::new(stable_routes(), b"old binary\n").await;

        install(
            false,
            Channel::Stable,
            &fixture.exe,
            &fixture.base,
            &current("0.1.2"),
            &[key()],
        )
        .await
        .unwrap();

        let output = std::process::Command::new(&fixture.exe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "seavx 0.1.3 (06a4060)"
        );
        assert_eq!(fixture.pointer_requests(), 2, "pointer, then artifact");
        assert!(fixture.leftovers().is_empty(), "{:?}", fixture.leftovers());
    }
}
