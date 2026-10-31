---
name: seavx-dev
description: Workflow for building Seavx features in this repository — debug mode is mandatory, data must land in the repo .seavx/, and every feature ships with a skill entry. Use when implementing, fixing or reviewing code in the Seavx repository. Don't use it for operating the released agent; that is the seavx skill.
version: 0.1.0
author: Muhammad Adriansyah (Seaavey), Hermes Agent
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [seavx, rust, development-workflow, testing]
    related_skills: [seavx]
---

# Seavx development

## Debug on, always

Every coding session on this repository runs in debug mode: `seavx -d`,
`cargo run -- -d` when the binary is not installed, or `SEAVX_DEBUG=true` for a
run already started. Without it `sessions.db` and `debug.log` land in
`~/.seavx` — the real store — and the workspace keeps nothing to read after a
failed turn. The full table is in `../seavx/SKILL.md`.

Verify, never assume:

```
seavx doctor --offline | rg session     # .seavx/sessions.db, not /home/…/.seavx
```

## Feature checklist

1. **Debug on**, confirmed with `doctor`.
2. **Read before writing.** The module you touch owns its test file:
   `#[cfg(test)] #[path = "../tests/<area>/<name>.rs"] mod tests;` at its bottom,
   tests in `src/tests/`. A behaviour change without a test is unfinished.
3. **Prove it** — `cargo test`, `cargo clippy --all-targets`, `cargo fmt`, then
   run the thing (`seavx -d doctor --offline`, the TUI, or `seavx exec` for a
   scripted turn). A green suite is not a green feature.
4. **Changelog.** One entry under `## Unreleased` in `CHANGELOG.md`, under the
   heading the block already uses (`feat`, `fix`, `breaking`) — copy the existing
   style instead of inventing a category — describing behaviour that is in the
   code.
5. **README** when a command, config key, path or default changes. It is a
   contract: state limits instead of implying them.
6. **Skill.** Every feature adds or updates one: a new playbook becomes
   `skills/autonomous-agents/<name>/SKILL.md`, something about Seavx itself joins
   a section of `skills/autonomous-agents/seavx/SKILL.md`. Without it the feature
   is not finished.

## Traps

- **Never develop against `~/.seavx`.** A bug that only reproduces in one of the
  two stores hides for as long as you run only that one.
- **Do not edit `.env` through agent tools.** It is protected; the read or write
  is refused. Edit it by hand.
- **Do not run the TUI without a TTY** — it fails at raw mode. Use `seavx exec`
  or `doctor` for scripted checks.
- **`seavx exec` writes no session and no `.env`**, but the engine still appends
  every request payload to `.seavx/request.log` and tracing writes
  `.seavx/debug.log`, even without `-d`. Expect those two files afterwards; the
  absence of a session is the only thing that proves `exec`.
