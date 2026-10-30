---
name: seavx-dev
description: Workflow for building Seavx features in this repository — debug mode is mandatory, data must land in the repo .seavx/, and every feature ships with a skill entry. Use when implementing, fixing or reviewing code in the Seavx repository. Don't use it for operating the released agent; that is the seavx skill.
---

# Seavx development

## The one rule: debug on

Any coding session on this repository runs in debug mode. Start it with
`seavx -d`, or `cargo run -- -d` when the binary is not installed, or
`SEAVX_DEBUG=true` when the run already started.

Debug mode is the only thing that moves `sessions.db` and `debug.log` into the
repo `.seavx/`. Without it a development run writes to `~/.seavx` — the store
the real sessions live in — and leaves nothing in the workspace to read after a
failed turn.

Verify, never assume:

```
seavx doctor --offline | grep session     # .seavx/sessions.db, not /home/…/.seavx
```

A debug build is not a debug run: `cargo run` without `-d` takes the release
path. Debug is sticky, too — a `-d` run writes `SEAVX_DEBUG=true` into `.env`,
so the next plain run is debug as well until that line says `false`.

## Feature checklist

1. **Debug on**, confirmed with `doctor`.
2. **Read before writing.** The module you touch owns its own test file:
   `#[cfg(test)] #[path = "../tests/<area>/<name>.rs"] mod tests;` at the bottom
   of the module, tests in `src/tests/`. A behaviour change without a test is
   unfinished.
3. **Prove it.**
   ```
   cargo test
   cargo clippy --all-targets
   cargo fmt
   ```
   Then run the thing: `seavx -d doctor --offline`, or the TUI, or
   `seavx exec` for a scripted turn. A green test suite is not a green feature.
4. **Changelog.** One entry under `## Unreleased` in `CHANGELOG.md`, in the
   right category (`feature`, `fix`, `refactor`, `docs`), describing behaviour
   that is actually in the code.
5. **README** when a command, config key, path or default changes. The README is
   a contract: it states limits instead of implying them.
6. **Skill.** Every feature adds or updates a skill. A new playbook becomes
   `skills/autonomous-agents/<name>/SKILL.md`; something about Seavx itself goes
   into a section of `skills/autonomous-agents/seavx/SKILL.md`. A feature
   without a skill entry is not finished.

## Traps

- **Never develop against `~/.seavx`.** A bug that only reproduces in the repo
  store, or only in the home store, hides for as long as you only run one of them.
- **Do not edit `.env` through agent tools.** `.env` and its variants are on the
  protected list; the read or write is refused before it starts. Edit it by hand.
- **Do not run the TUI without a TTY.** It fails at raw mode; use `seavx exec`
  or `doctor` for scripted checks.
- **`seavx exec` writes nothing** — no session, no log, no `.env`. If you need
  those, you are not testing `exec`.
- **A skill under `skills/` is live on the next run.** The four roots are
  `.seavx/skills`, `skills/`, `~/.seavx/skills`, `~/.agents/skills`; a repo
  skill shadows a user skill of the same name, and no copy is needed.
