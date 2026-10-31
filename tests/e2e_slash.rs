#![cfg(unix)]
//! End-to-end: the binary a user runs, driven through a real pty, typing slash
//! commands. The unit tests call `parse`/`execute` directly; this proves the
//! wiring in front of them — raw-mode input, the slash menu, the effect
//! handlers and the writes they make — still works from the outside.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a command may take to reach the disk or end the process. The event
/// loop is free-running, so every wait polls instead of sleeping a fixed time.
const LIMIT: Duration = Duration::from_secs(10);

const READY: &str = "ctrl+c exit";

struct Tui {
    child: Child,
    pty: fs::File,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Tui {
    fn start(workspace: &Path, home: &Path) -> Tui {
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        // `termp` and `winp` are `*mut` on Apple and `*const` on Linux; a
        // `*mut` coerces to either, so the call stays one shape per platform.
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // shortcut: `openpty` lives in libutil before glibc 2.34, so a very old
        // build host needs `-lutil`; upgrade when one shows up in CI.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());

        let stdin = unsafe { fs::File::from_raw_fd(slave) };
        let stdout = stdin.try_clone().expect("dup pty slave");
        let stderr = stdin.try_clone().expect("dup pty slave");

        let mut command = Command::new(env!("CARGO_BIN_EXE_seavx"));
        command
            .current_dir(workspace)
            .env("HOME", home)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        // The TUI switches its terminal to raw mode and reads key events from
        // it, which needs a controlling terminal, not just a tty fd.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn the seavx binary");

        let pty = unsafe { fs::File::from_raw_fd(master) };
        let screen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&screen);
        let mut reader = pty.try_clone().expect("dup pty master");
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => sink
                        .lock()
                        .expect("screen lock")
                        .extend_from_slice(&buffer[..read]),
                    // A signal during a blocking read must not end the capture.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        Tui { child, pty, screen }
    }

    /// Type one command and press Enter, the way a user sends it.
    fn run(&mut self, command: &str) {
        self.press(&format!("{command}\r"));
    }

    fn press(&mut self, keys: &str) {
        self.pty
            .write_all(keys.as_bytes())
            .expect("write to the pty");
    }

    /// Everything drawn so far with the escapes removed. Ratatui writes rows as
    /// cell runs and skips a cell it already drew, so a space can be missing
    /// between two words: comparisons ignore whitespace on both sides.
    fn screen(&self) -> String {
        let bytes = self.screen.lock().expect("screen lock");
        let mut text = Vec::with_capacity(bytes.len());
        let mut rest = bytes.as_slice();
        while let Some((&byte, tail)) = rest.split_first() {
            if byte == 0x1b {
                // Skip one escape sequence: `ESC [ … final` or `ESC ( x`.
                let mut consumed = 1;
                if let Some(&first) = tail.first() {
                    consumed += 1;
                    if first == b'[' {
                        for &byte in &tail[1..] {
                            consumed += 1;
                            if (0x40..=0x7e).contains(&byte) {
                                break;
                            }
                        }
                    }
                }
                rest = &rest[consumed.min(rest.len())..];
                continue;
            }
            if byte == b'\n' || byte == b' ' || byte.is_ascii_graphic() {
                text.push(byte);
            }
            rest = tail;
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    fn shows(&self, needle: &str) -> bool {
        let screen: String = self.screen().split_whitespace().collect();
        let needle: String = needle.split_whitespace().collect();
        screen.contains(&needle)
    }

    fn waits_for(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if done() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {what}; screen tail:\n{}",
            self.tail()
        );
    }

    fn wait_for_screen(&self, needle: &str) {
        self.waits_for(needle, || self.shows(needle));
    }

    /// `/exit` must leave raw mode and the alternate screen behind, which the
    /// process can only do on its cleanup path: a non-zero status means it did not.
    fn exit(mut self) -> ExitStatus {
        self.run("/exit");
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("wait for the TUI") {
                return status;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("/exit did not quit the TUI; screen tail:\n{}", self.tail());
    }

    fn tail(&self) -> String {
        let screen = self.screen();
        let start = screen.len().saturating_sub(2000);
        screen[start..].to_string()
    }
}

impl Drop for Tui {
    /// A panic must not leave a raw-mode TUI behind on the developer's machine.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn workspace_root(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".seavx")
}

#[test]
fn the_mode_shortcut_cycles_the_header_in_a_real_terminal() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");

    let mut tui = Tui::start(workspace.path(), home.path());
    tui.wait_for_screen(READY);
    tui.wait_for_screen("[build]");

    // Ctrl+Tab arrives as `CSI 9;5u` on a terminal that speaks the kitty
    // protocol, which the TUI asks for on start; `ESC [ Z` is the Shift+Tab
    // sequence every terminal sends and the same handler owns both.
    tui.press("\x1b[9;5u");
    tui.wait_for_screen("[plan]");
    tui.press("\x1b[Z");
    tui.wait_for_screen("[accept-edits]");
    tui.press("\x1b[Z");
    tui.wait_for_screen("[auto]");

    assert!(tui.exit().success(), "the TUI exits cleanly after a cycle");
}

#[test]
fn slash_commands_that_write_land_on_disk_and_the_tui_exits_cleanly() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let fixture = tempfile::tempdir().expect("temp fixture");

    let skill = fixture.path().join("e2e-skill");
    fs::create_dir(&skill).expect("fixture skill dir");
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: e2e-skill\ndescription: fixture for the e2e run\n---\n\nFixture.\n",
    )
    .expect("fixture SKILL.md");
    let preset = fixture.path().join("e2e.md");
    fs::write(&preset, "Preset body for the e2e run.\n").expect("fixture preset");

    let mut tui = Tui::start(workspace.path(), home.path());
    tui.wait_for_screen(READY);

    tui.run("/init");
    let config = workspace_root(workspace.path()).join("config.toml");
    tui.waits_for("the config /init writes", || config.is_file());

    tui.run(&format!("/skills add {}", skill.display()));
    tui.waits_for("the skill copy", || {
        workspace_root(workspace.path())
            .join("skills/e2e-skill/SKILL.md")
            .is_file()
    });

    tui.run(&format!("/preset add {}", preset.display()));
    tui.waits_for("the preset copy", || {
        workspace_root(workspace.path())
            .join("prompts/e2e.md")
            .is_file()
    });

    tui.run("/config set model probe/e2e");
    tui.waits_for("the model write", || {
        fs::read_to_string(&config).is_ok_and(|text| text.contains("model = \"probe/e2e\""))
    });
    // `/init` seeded the defaults and `set` edited them in place.
    let written = fs::read_to_string(&config).expect("read the config");
    assert!(
        written.contains("max_turns"),
        "the workspace config lost its other keys:\n{written}"
    );

    let status = tui.exit();
    assert!(status.success(), "/exit returned {status}");
}

#[test]
fn read_only_slash_commands_run_a_whole_session_without_writing_a_config() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");

    let mut tui = Tui::start(workspace.path(), home.path());
    tui.wait_for_screen(READY);

    // Screen assertions come first: `/help` prints a notice taller than the
    // pane, and the transcript then stops following the bottom (see the note in
    // the test's doc), so nothing after it is visible.
    tui.run("/status");
    tui.wait_for_screen("state: idle");
    tui.run("/memory");
    tui.wait_for_screen("Project memory");
    tui.run("/preset");
    tui.wait_for_screen("No presets yet");
    tui.run("/help");
    tui.run("/skills list");
    tui.run("/goal");
    tui.run("/tasks");
    tui.run("/undo");

    let root = workspace_root(workspace.path());
    assert!(
        !root.join("config.toml").exists(),
        "a read-only command wrote a config"
    );
    assert!(
        !root.join("prompts").exists(),
        "a read-only command created the preset directory"
    );
    assert!(
        !root.join("skills").exists(),
        "a read-only command created the skills directory"
    );

    let status = tui.exit();
    assert!(status.success(), "/exit returned {status}");
}

/// A cold cache is not a dead end: `/model` asks the configured endpoint, and a
/// fetch that cannot connect names the host instead of a dead-end sentence.
#[test]
fn a_cold_model_picker_names_the_unreachable_host() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    fs::create_dir_all(workspace_root(workspace.path())).expect("config dir");
    fs::write(
        workspace_root(workspace.path()).join("config.toml"),
        "base_url = \"http://127.0.0.1:9/v1/\"\nmax_turns = 3\n",
    )
    .expect("write config");

    let mut tui = Tui::start(workspace.path(), home.path());
    tui.wait_for_screen(READY);

    tui.run("/model");

    // The fetch cannot connect, and the notice names the host it tried.
    // No `/exit` here: the picker owns the keyboard while it is open, and this
    // test is about the notice, not the teardown. `Drop` kills the child.
    tui.wait_for_screen("127.0.0.1:");
}
