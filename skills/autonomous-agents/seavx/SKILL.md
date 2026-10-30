---
name: seavx
description: Operate Seavx, the Rust terminal coding agent in this repository — its CLI, slash commands, debug mode and data locations, provider config, sessions, checkpoints and permission model. Use when running or configuring Seavx, when a Seavx run puts data in the wrong place, or when changing how the agent behaves. Don't invoke it for unrelated Rust work in the same repository.
---

# Seavx

Seavx is a single-binary AI coding agent for the terminal: a ratatui TUI over a
provider layer (`genai`), SQLite sessions and a checkpoint store that `/undo`
restores from. Everything below is behaviour of the code in this repository.

## Running it

| Command | What it does |
| --- | --- |
| `seavx`, `seavx run [prompt]` | TUI session |
| `seavx exec <task>` | headless run; never writes a session or a log |
| `seavx -d` | any of the above with debug mode on |
| `seavx resume [id]` | reopen a stored session, picking from a list when no id |
| `seavx sessions list\|show <id>\|export <id>\|resume <id>` | the session store from the shell |
| `seavx model [model] [-e effort]` | set model and effort, refreshing the model cache |
| `seavx doctor [--offline]` | environment and health report |
| `seavx init` | write `.seavx/config.toml` |
| `seavx setup providers` | base URL, key, model, effort |
| `seavx memory` | inspect `MEMORY.md` and `USER.md` |
| `seavx update`, `seavx uninstall` | self-update, removal |

`--debug` is a global flag: `seavx -d doctor` is valid.

Effort values: `none`, `minimal`, `medium`, `high`, `highx`, `max`.

## Debug mode decides where the data goes

Debug mode is on when `--debug`/`-d` is passed, or when `SEAVX_DEBUG` is `1` or
`true`. Nothing else counts — a debug build is not a debug run, so `cargo run`
writes to `~/.seavx` exactly like an installed binary.

| | debug on | debug off |
| --- | --- | --- |
| sessions | `./.seavx/sessions.db` | `~/.seavx/sessions.db` |
| tracing | `./.seavx/debug.log` | stderr |

Every TUI run writes the state it resolved into the workspace `.env` as
`SEAVX_DEBUG=true` or `SEAVX_DEBUG=false`: the file is created when missing, that
line is rewritten in place, every other line is left alone, and `seavx exec`
skips it. So a directory that was never told anything becomes `false` on the
first run, and `-d` makes the next plain run debug as well until the line says
otherwise. `seavx doctor` and `seavx update` never write `.env`.

Never hand-edit `.env` through the agent's tools: `.env` and its variants are on
the protected list, so a read or a write is refused before it starts.

## Slash commands (TUI)

`/model [model] [effort]`, `/compact [focus]`, `/config …`, `/plan [on|off]`,
`/clear`, `/new` (alias of `/clear`), `/help`, `/init`, `/doctor [--offline]`,
`/memory [show|project|user|skills|path]`, `/undo`, `/sessions [number]`,
`/resume`, `/exit`.

`/model` with no argument opens a picker over the cached model list; a number is
a 1-based index into that list and an index outside it is refused, never saved as
a model name. In the picker, letters filter the list (case-insensitive, Backspace
widens it) and `ctrl+r` re-fetches `/models` from the gateway in the background:
the title shows a spinner until the list lands, and a second `ctrl+r` is ignored
meanwhile. The picker scrolls with the cursor, so a list longer than the box stays
reachable. `/plan` is read-only and is refused while a turn is running.

## Configuration

```
model = "provider/model"
base_url = "https://gateway/v1/"
api_key = "…"
reasoning_effort = "high"
stream = true
max_turns = 100
tool_permissions = "ask" | "allow" | …
permitted_actions = ["workspace-write"]
system_prompt = "…"
```

A workspace `.seavx/config.toml` takes precedence over `~/.seavx/config.toml`.
`/config` and `seavx model` write it atomically, owner-only (`0600`); the key is
never printed back.

## Permissions and safety

- Writes inside the workspace are ungated only when `tool_permissions` allows
  them; the default `ask` stops and waits. `seavx exec` has no human and denies.
- The workspace jail refuses paths outside the root, symlink swaps, and in-place
  edits between validation and open.
- Protected and never readable or writable: `.git`, `.ssh`, `.gnupg`, `.aws`,
  `.env*`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `.netrc*`, `*.pem`, `*.key`,
  `credentials*`, `secrets.*`, and `config.toml` / `SOUL.md` inside `.seavx`.

## Sessions and undo

Sessions are named `svx-` plus eight hex digits; a bare number from an older
database still resolves. Resume replays the newest turns up to 80 000 bytes and
collapses the rest into one `System: N older messages omitted` line — no
summarisation, no network call on start. `/undo` restores the files the last
agent call changed and refuses to overwrite a file edited afterwards.

## Working on the code

Feature work runs in debug mode, without exception: `seavx -d`, or
`SEAVX_DEBUG=true`. A coding session without it writes to `~/.seavx`, which is
the real store, and leaves no log in the workspace. Check before you start:

```
seavx doctor --offline | grep session     # must say .seavx/sessions.db
```

```
cargo test                 # 380+ tests, no network
cargo clippy --all-targets
cargo fmt
```

Module map: `src/tui/` (app, event loop, render, commands), `src/agent/` (engine,
backend seam, tools, security, checkpoint, budget), `src/cli/` (doctor, config,
model, sessions, update), `src/session.rs` (SQLite), `src/util.rs` (debug mode,
paths). Tests live in `src/tests/` and are wired with
`#[cfg(test)] #[path = "../tests/…"] mod tests;` in the module they cover.

Every feature ships with a skill entry: a new
`skills/autonomous-agents/<name>/SKILL.md` when it deserves its own playbook, a
section here when it belongs to Seavx itself. The full workflow — debug rule,
checklist, traps — is in `skills/autonomous-agents/seavx-dev/SKILL.md`.

A skill in `skills/` is loaded on the next run: it is the second of four roots
(`.seavx/skills`, `skills/`, `~/.seavx/skills`, `~/.agents/skills`), so it wins
over a user skill of the same name and needs no copy or symlink.
