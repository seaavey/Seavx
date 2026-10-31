---
name: seavx
description: Operate Seavx, the Rust terminal coding agent in this repository — CLI, slash commands, debug mode and data locations, provider config, sessions, checkpoints and the permission model. Use when running or configuring Seavx, when a run puts data in the wrong place, or when changing how the agent behaves. Not for building it; that is the seavx-dev skill.
version: 0.1.0
author: Muhammad Adriansyah (Seaavey), Hermes Agent
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [seavx, rust, terminal-agent, sessions, config]
    related_skills: [seavx-dev]
---

# Seavx

A single-binary AI coding agent for the terminal: a ratatui TUI over a provider
layer (`genai`), SQLite sessions, and a checkpoint store `/undo` restores from.

## Running it

| Command | What it does |
| --- | --- |
| `seavx`, `seavx run [prompt]` | TUI session |
| `seavx exec <task>` | headless turn; no session, no `.env`, but its request log and debug log still land in `.seavx/` |
| `seavx resume [id]` | reopen a stored session, picking from a list when no id |
| `seavx sessions list\|show <id>\|export <id>\|resume <id>` | the session store from the shell |
| `seavx model [model] [-e effort]` | set model and effort; this refreshes the model cache |
| `seavx doctor [--offline]` | environment and health report |
| `seavx init`, `seavx setup providers` | write `.seavx/config.toml`; ask for base URL, key, model, effort |
| `seavx memory`, `seavx skills …` | inspect `MEMORY.md`/`USER.md`; list, install, remove skills |
| `seavx update [--force]`, `seavx uninstall` | self-update, removal |

`--debug`/`-d` is global: `seavx -d doctor` is valid. Effort values are `none`,
`minimal`, `medium`, `high`, `highx`, `max`.

## Debug mode decides where the data goes

On when `-d` is passed or `SEAVX_DEBUG` is `1`/`true` — nothing else. A debug
build is not a debug run: `cargo run` writes to `~/.seavx` like an installed
binary.

| | debug on | debug off |
| --- | --- | --- |
| sessions | `./.seavx/sessions.db` | `~/.seavx/sessions.db` |
| tracing | `./.seavx/debug.log` | stderr |

A TUI run records the state it resolved in the workspace `.env` as
`SEAVX_DEBUG=true|false`: created when missing, that one line rewritten in place,
every other line untouched; `exec`, `doctor` and `update` never write it. So a
plain run after a `-d` run is still debug until the line says otherwise, and a
fresh directory becomes `false` on first run. `.env` and its variants are on the
protected list — the agent's tools cannot read or write them, so edit by hand.

## Slash commands

`/help` renders the single command table in `src/tui/commands.rs` — read it there
instead of copying the list into a skill, which is how it goes stale. The
semantics the table cannot show:

- `/plan` is read-only and refused while a turn runs; `/ask` and `/review` are
  read-only single turns.
- `/steer <text>` reaches the running turn at its next step; `/queue <prompt>`
  runs after that turn succeeds.
- `/new` is `/clear`; `/resume` is `/sessions`; `/model` with no argument opens a
  picker over the cached models — a number is a 1-based index and out of range is
  refused, never saved as a name. In the picker, letters filter (case-insensitive,
  Backspace widens), `ctrl+r` refetches `/models` in the background behind a
  spinner and is ignored while that fetch runs, and the list scrolls with the
  cursor.

## Configuration

Nine keys, workspace `.seavx/config.toml` over `~/.seavx/config.toml`:

```
model = "provider/model"
base_url = "https://gateway/v1/"
api_key = "…"
reasoning_effort = "high"
stream = true
max_turns = 100
system_prompt = "…"
tool_permissions = "deny" | "ask" | "allow"
permitted_actions = ["write", "execute", "network"]
```

`SEAVX_MODEL`, `SEAVX_BASE_URL`, `SEAVX_API_KEY` and `SEAVX_REASONING_EFFORT`
override the file. `permitted_actions` lifts kinds from `ask` to `allow`; it
cannot lift global writes or destructive actions, which always ask. `/config` and
`seavx model` write atomically, owner-only (`0600`), and never print the key back.

## Permissions and safety

- `tool_permissions = "ask"` (the default) stops and waits on a write or a shell
  command; `allow` lets workspace actions through; `deny` refuses. `seavx exec`
  has no human, so it denies.
- The workspace jail refuses paths outside the root, symlink swaps, and in-place
  edits between validation and open.
- Never readable or writable: `.git`, `.ssh`, `.gnupg`, `.aws`, `.env*`, `id_rsa*`,
  `id_ed25519*`, `id_ecdsa*`, `.netrc*`, `*.pem`, `*.key`, `credentials*`,
  `secrets.*`, and `config.toml`/`SOUL.md` inside `.seavx`.

## Sessions and undo

Ids are `svx-` plus eight hex digits; a bare number from an older database still
resolves. Resume replays the newest turns up to 80 000 bytes and folds the rest
into one `System: N older messages omitted` line — no summarisation, no network
call on start. `/undo` restores the files the last agent call changed and refuses
to overwrite a file edited afterwards.

## Working on the code

Module map: `src/tui/` (app, event loop, render, commands), `src/agent/` (engine,
backend seam, tools, security, checkpoint, budget), `src/cli/` (doctor, config,
model, sessions, skills, update), `src/session/` (SQLite), `src/util.rs` (debug
mode, paths). Tests live in `src/tests/`, wired per module with
`#[cfg(test)] #[path = "../tests/…"] mod tests;`.

Feature work runs in debug mode without exception — see the table above. The
checklist (debug, tests, changelog, README, skill entry) and the traps are in
`skills/autonomous-agents/seavx-dev/SKILL.md`.

A skill under `skills/` is live on the next run: the four roots are
`.seavx/skills`, `skills/`, `~/.seavx/skills`, `~/.agents/skills`, first wins, so
a repo skill shadows a user skill of the same name and needs no copy or symlink.
