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
| `seavx init`, `seavx setup providers` | write a config file (`seavx init` → workspace, `seavx setup [--workspace]` → global by default); ask for base URL, key, model, effort; a `401`/`403` from `/models` re-asks the key instead of saving the endpoint |
| `seavx memory`, `seavx skills …` | inspect `MEMORY.md`/`USER.md`; list, install, remove skills |
| `seavx update [--force] [--channel stable\|edge]`, `seavx uninstall` | self-update, removal (`uninstall` asks whether to keep data) |

`--debug`/`-d` is global: `seavx -d doctor` is valid. The effort pickers
(`seavx model`, `seavx setup`, the TUI modal) offer the levels the model
publishes in the models.dev catalog, and fall back to `none`, `minimal`,
`medium`, `high`, `highx`, `max` for an id the catalog does not know. A saved
effort is never refused for being absent from that list.

## Updates

`seavx update` installs the build the CDN's signed pointer names. The channel is
`stable` by default — the pointer at `latest/<os>/<arch>.json` — and
`--channel edge` reads `edge/<os>/<arch>.json`, the newest master build. The
client verifies the pointer's minisign signature over its canonical
`SEAVX-UPDATE-v1` bytes against keys compiled into the binary first, then the
signed channel, the target it was built for, the `builds/{sha}/{sha256}/{binary}`
route and the version ordering, then the download's `sha256`, then the
candidate's own `--version` output, and only then renames it over the binary. A
failure at any point leaves the installed binary byte-identical and deletes the
temp file; `--force` relaxes ordering only, never a check.

An empty key set refuses, so a build from this branch answers
`no pinned update key` — a pointer that is not signed by a pinned key answers
`signature: not signed by a pinned update key`. Do not "fix" that by disabling
the check.

`SEAVX_UPDATE_BASE` moves the origin for a test server; it changes no check, and
both installers read it too. `install.sh` and `install.ps1` pin the same key and
refuse while it is empty. With a key present but `minisign` not on `PATH`, they
warn and verify the checksum only — that installer run is not authenticated,
which is why a machine that installs this way should also run `seavx update`
once a pinned build exists.

## Debug mode decides where the data goes

On when `-d` is passed or `SEAVX_DEBUG` is `1`/`true` — nothing else. A debug
build is not a debug run: `cargo run` writes to `~/.seavx` like an installed
binary.

| | debug on | debug off |
| --- | --- | --- |
| sessions | `./.seavx/sessions.db` | `~/.seavx/sessions.db` |
| tracing | `./.seavx/debug.log` | stderr |
| project memory | `./.seavx/MEMORY.md` | `~/.seavx/MEMORY.md` |
| user preferences | `./.seavx/USER.md` | `~/.seavx/USER.md` |

Reads are not the write path: prompt injection and `seavx memory` take the
checkout's `.seavx/MEMORY.md` first, then a legacy root `MEMORY.md`, then the
global file — so a repro that reuses one store can hide a bug in the other.

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
  Backspace widens), row 0 is always `↻ Refresh models from <host>` and runs the
  same fetch as `ctrl+r` (background, behind a spinner, ignored while that fetch
  runs), a filter matching nothing says `no match for "<query>" · ctrl+r
  refreshes`, and a cache past its 24 h TTL is refetched on open with the stale
  rows still listed. `/model` and both pickers are **session-scoped**: they change the model
  of the turns that follow and leave the config file alone, so the saved default
  survives the run. Only `seavx model [name]` (and `/config set model …`) write
  the file. `/clear` keeps the session's model for the rest of the process.
- Mouse capture belongs to the app, so dragging in the transcript selects text:
  the block is painted on a background and released to the clipboard with a
  `copied N lines` toast. The block is held in painted rows, so scrolling moves
  it with the text; `Esc` clears it and a resize drops it. A click copies
  nothing, and `Shift`+drag still reaches the terminal's own selection.
- `run_command` runs with a 120 s ceiling and, when it fires, returns the stdout
  and stderr it had already drained behind the timeout line — a slow network
  install is not reported as a hang with no evidence.

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

Context windows are not a config key: the models.dev catalog answers first,
cached seven days in `.seavx/cache/contexts.json` (one `limit.context` and one
`reasoning_options` effort list per model name, matched on the id's tail with
every separator dropped, so a routed `rv/vendor/Model-1017` resolves and
`v4.1`/`v4-1`/`V4_1` are one name) and refreshed in the background at TUI
start and once at the end of `seavx setup`. `doctor` and `exec` read the cache
and never fetch. An id the catalog misses falls back to the family table —
`claude`, `gemini`, `nemotron` → 1M; `deepseek`, `glm`, `gpt-4o`, `gpt-4.1`,
`o3`, `o4` → 128k — and then to 64 000 marked `(assumed)` in the footer and
`doctor`. The budget is 80 % of whichever answer wins.

A write that names no file — `seavx model`, a permission approval, `/config set`
— lands in the config the loader reads: the workspace file when one exists, the
global one otherwise. The workspace file shadows the global config *entirely*,
and these writers never persist `api_key`, so one created where none existed
leaves the key behind and the next turn dies with `ApiKeyEnvNotFound`. `seavx
setup` is the deliberate exception: it writes the global file by default and this
workspace's with `--workspace`. `/model` is no writer: the session's model is
carried by the turn request (`TurnRequest::model`), so the file keeps the default
that `seavx model` wrote.

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
