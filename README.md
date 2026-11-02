# Seavx 🏺

[![CI & Release](https://github.com/seaavey/Seavx/actions/workflows/ci.yml/badge.svg)](https://github.com/seaavey/Seavx/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Seavx is a coding agent that runs in your terminal. It reads a repository, edits
files, runs commands, and keeps every file change in a checkpoint you can undo.
It ships as one Rust binary: no Node, no Python, no package manager at runtime.

## What it does

Give it a task, in the TUI or as a one-shot command. The model gets a small set
of tools: read, write, edit, apply a unified diff, search, run a command, save a
note, load a skill, ask you a question. Every call goes through one permission
matrix that also covers the shell, and every write is checkpointed before it
lands, so `/undo` walks back through them.

A conversation is stored in SQLite, so a session closed mid-turn can be listed,
exported, or resumed with its tool calls intact. Requests are measured before
they are sent and compacted when they would not fit the model's window.

## Why this one

- One static binary with no runtime to install. `cargo install --path .` or the
  prebuilt download is the whole setup.
- Checkpoints cover batched edits: one `apply_patch` writes all its files or
  none, and a batch interrupted by a crash is reported at the next start instead
  of going missing.
- The same matrix decides a tool call and a shell command. Destructive calls
  (`rm -rf`, `mkfs`, `dd of=/dev/...`, `shutdown`, fork bombs) are refused in
  every mode, and no config key lifts that floor.
- Any OpenAI-compatible endpoint works. A gateway routing several providers
  needs one `base_url` and a `<provider>/<model>` prefix, and `/model` switches
  the model for the current session only.
- Skills load from four roots in order, so a project skill shadows a global one
  with the same name.

## When to use something else

Seavx is a gate, not a sandbox. It cannot stop a deliberately obfuscated shell
command (`eval`, base64, `curl | sh`), it does not encrypt sessions or memory at
rest, and its file-mode and inode checks are Unix-only: on Windows the mode
policy is a no-op. If you need OS-level containment (namespaces, seccomp,
`openat2`), run Seavx inside a container or a VM.

## Try it

```console
$ seavx exec "Inspect Cargo.toml and list dependencies"
Executing task: Inspect Cargo.toml and list dependencies

Tool: read_file(Cargo.toml) completed
Dependencies from `Cargo.toml`:

### `[dependencies]`
- **anyhow**: `1.0.104`
- **clap**: `4.6.7` (features: `["derive"]`)
- **crossterm**: `0.28.1` (features: `["event-stream"]`)
...
```

`exec` is session-free and writes nothing. Run a bare `seavx` for the
interactive agent.

## Install

Linux x86_64, macOS arm64 and Windows x86_64 have prebuilt binaries. Any other
Linux or macOS combination builds from source, which needs
[Cargo](https://rustup.rs). The binary lands in `~/.local/bin`, and the
installer says so when that directory is not on `PATH`.

```bash
curl -fsSL https://raw.githubusercontent.com/seaavey/Seavx/master/install.sh | bash
```

Windows installs to `%LOCALAPPDATA%\Programs\seavx` and adds it to your user
`PATH`:

```powershell
irm https://raw.githubusercontent.com/seaavey/Seavx/master/install.ps1 | iex
```

From a checkout:

```bash
cargo install --path .
```

Every push to master publishes a build tagged with its commit, for linux
x86_64, macOS arm64 and windows x86_64:

```bash
curl -sI https://cdn.seavx.com/latest/linux/x86_64   # 302 to the newest linux build
curl -O https://cdn.seavx.com/builds/<sha>/<sha256>/seavx-linux-x86_64
```

An installed binary updates itself from the same origin:

```bash
seavx update                   # newest tagged release
seavx update --channel edge    # newest master build
```

Before anything is replaced, the client verifies the minisign signature over the
release pointer, that the pointer is for the requested channel and this
platform, that its route names the digest, the `sha256` of the bytes it
downloaded, and that the candidate itself reports the pointer's version.
`--force` only skips the version ordering; a failure leaves the installed binary
untouched. CI signs the pointer on every master build, and the release key is
pinned in the client and in both installers:

```console
$ seavx update
Checking for updates... (current version: v0.1.3-rc.1)
SEAVX is already up to date (v0.1.3-rc.1, 67e4220)
$ seavx update --channel edge
A newer build of v0.1.3-rc.1 is available (6f86d0f).
Downloading v0.1.3-rc.1 (6f86d0f)...
Successfully updated SEAVX v0.1.3-rc.1 (6f86d0f, /home/seaavey/.local/bin/seavx)
signature  ok
checksum   ok
```

A pointer whose signature does not verify is refused before anything is
downloaded, by the client and by both installers.

## Configure a provider

Once per machine. The wizard calls the endpoint's `/models` before saving, so a
wrong URL or a rejected key shows up immediately:

```bash
seavx setup providers          # global ~/.seavx/config.toml
seavx setup --workspace        # this repo's .seavx/config.toml instead
```

```toml
max_turns = 100
base_url = "https://your-gateway.example/v1/"
api_key = "sk-..."
reasoning_effort = "highx"
stream = true
```

A workspace `.seavx/config.toml` shadows the global file entirely. `seavx model`
changes the saved default; `/model` in the TUI changes this session only. A
config that names no model is refused instead of being sent to a provider you
did not choose.

## Modes

Four standing modes, cycled with `Ctrl+Tab` (`Shift+Tab` where Ctrl+Tab is not
reported):

| Mode         | Write in workspace | Command and network |
| ------------ | ------------------ | ------------------- |
| Build        | asks               | asks                |
| Plan         | disabled           | disabled            |
| Accept-edits | runs               | asks                |
| Auto         | runs               | runs                |

Two floors hold in every mode: a write outside the workspace (memory files,
skills) asks, and a destructive call is refused. `/plan` and `/plan off` jump
between Plan and Build.

## Sessions

Stored in `~/.seavx/sessions.db`, or `.seavx/sessions.db` under `--debug`.
Prompts and assistant text are kept verbatim; tool output is redacted before it
is stored.

```bash
seavx sessions list
seavx sessions show svx-2f124b69
seavx sessions export svx-2f124b69 > session.json
seavx resume svx-2f124b69
```

## Documentation

- [docs/tui.md](docs/tui.md): prompt completion, history recall, every slash command, permission keys.
- [docs/sessions.md](docs/sessions.md): storage layout, what resume replays, retention and backup.
- [docs/context-and-compaction.md](docs/context-and-compaction.md): window sizes, the request budget, what a summary must carry.
- [docs/streaming.md](docs/streaming.md): what is retried, what a cancelled turn leaves behind.
- [docs/security.md](docs/security.md): the workspace jail, the credential names refused, the full matrix, the stated limits.
- [CONTRIBUTING.md](CONTRIBUTING.md), [CHANGELOG.md](CHANGELOG.md), [ROADMAP.md](ROADMAP.md).

## Status

Version 0.1.3-rc.1, actively developed by one maintainer. No stability guarantee yet:
config keys and the session schema can change between minor versions, and the
[CHANGELOG](CHANGELOG.md) records what moved. The [roadmap](ROADMAP.md) lists the
three milestones in progress.

## License

Distributed under the [MIT License](LICENSE). Copyright (c) 2026 Muhammad Adriansyah (Seaavey).
