# Changelog

All notable changes to Seavx are recorded here, grouped by category. One entry
per landed change; no entry describes behaviour that is not in the code.

## 0.1.2 — 2026-10-29

### feature

- Streaming responses: every turn streams by default and tokens render as they
  arrive. `stream = false` sends one plain request per turn.
- One fallback: a stream that fails before the first chunk is retried once
  without streaming, consuming the turn's single retry. A stream that fails
  after a chunk is never retried and its prefix is stored as `partial`.
- Cancellation: `Esc` cancels the running turn (ghost → menu → cancel → quit).
  A turn cancelled while awaiting the provider, mid-stream or between turns ends
  as `cancelled`/`partial` and never looks complete.
- `run_command` kills the whole process group (`SIGTERM`, 200 ms, `SIGKILL`;
  `taskkill /T /F` on Windows) and reports `outcome: cancelled`. A cleanup the OS
  did not confirm within two seconds is reported with its pid, never called
  clean.
- One command registry feeds parsing, `/help`, the autocomplete palette and the
  README table; `/new` is an alias of `/clear`.
- `/sessions [number]` lists this workspace's sessions and resumes one by number,
  refusing a read-only session with its reason.
- `/plan` reports both the switch and the repeat; a bare `/` is a no-op instead
  of an "Unknown command" error.
- `config`, `config-recovery`, `permissions`, `session` and `stream` checks in
  `doctor`; a pending `.bak` conflict is named by `doctor` and `/config`.

### refactor

- The provider is reached through one `ChatBackend` seam with `send` and
  `stream`; `GenaiBackend` is its only production implementation.
- A turn is one `TurnState` machine (`Running → Cancelling → Cancelled`,
  `Running → Completed|Failed`, no reverse edges) and returns a `TurnOutcome`.
- New configs default to `max_turns = 100`.

### fix

- The Unix-only umask guard and its lock in `src/tests/core/atomic_file.rs`, and
  the `run_shell` import in `src/tests/core/cancel.rs`, are `#[cfg(unix)]`, so
  `cargo check --all-targets` compiles on Windows. Test code only; no runtime
  behaviour changed.
- A shell read operand outside the workspace (`cat ../x`, `cat /etc/passwd`,
  `ln -s`, a `mv` source) is no longer classified as a plain read: the shell
  follows the path, so the command asks instead of being allowed. A
  substitution inside double quotes is lexed like one outside them.
- `cargo test` and `cargo build` ask; only `cargo fmt` is a write. `git` with an
  argument naming a protected path (`git show HEAD:.env`) asks.
- `/undo` records the bytes the agent wrote and refuses to restore over a file
  that changed afterwards, naming it. A checkpoint manifest that cannot be read
  is reported at startup instead of being skipped.
- A journal that cannot be appended after a batch rename rolls the renamed files
  back and reports a commit failure with the reason; it used to report a staging
  failure while both files stayed written.
- `run_command` drains output against a fixed two-second window, so a background
  child cannot hold the turn open, and keeps what arrived instead of discarding
  it. A timeout reports an unconfirmed cleanup with its pid.
- `apply_patch` refuses a create patch whose target already exists, instead of
  splicing the new lines in front of the existing content. `search_files` does
  not follow symlinks and skips files over 2 MB.
- `.seavx/SOUL.md` is a protected file: it is injected into the system prompt
  ahead of the security policy, so a tool may not rewrite it.
- Redaction covers a quoted key (`{"token": "…"}`) and a bare `Bearer …` value.
  A read of a file over the 16 MB limit is refused before it is read.
- A tool that picks its own destination is classified by that destination, so
  `skill_write` and `save_memory` ask as global writes instead of being allowed.
- The replay-rejection retry no longer panics on a history with a single turn
  boundary; a body with no user row is kept whole.
- One turn at a time: a second prompt while a turn runs is refused with a notice
  and the running turn keeps its cancel token. A cancelled turn no longer prints
  `Error: the turn was cancelled` on top of its own status.
- A generated title is stored against the session that asked for it and never
  renames the session the user already started. `/undo` and startup recovery use
  the canonical workspace root instead of the current directory.
- `seavx exec` prints a streamed answer once, not twice.
- `doctor`'s session check no longer creates the store it inspects, and warns
  when the schema version or payload format is not the one this build reads.

### tests

- Local SSE server fixtures: frame split at every boundary, multi-line `data:`,
  error event, `[DONE]`, abrupt end, usage and tool-call assembly.
- Fake backends for the stream fallback, mid-stream failure, per-stage
  cancellation and the confirmed/unconfirmed process-tree kill.
- Compatibility fixtures: a v1 session database built from the SQL literal, and
  a payload round-trip for every stored message shape.
- Doctor probe tests for a planted symlink, a pre-existing file and a replaced
  probe.

### docs

- README: streaming and cancellation limits, the cleanup wording ("no orphan
  where the OS confirmed the kill"), the Unix-only platform scope, the remaining
  limits (no encryption at rest, no automatic crash rollback), the `doctor` check
  list, and the registry/palette relationship.
- `src/systems/coding.md` states the streaming and cleanup rules for the agent.

### deps

- `tokio` gains the `net` and `io-util` features for the SSE test server.
