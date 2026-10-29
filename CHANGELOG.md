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
- `/resume` opens the same list as a picker: arrows move, Enter opens, Esc
  cancels. `seavx resume` is its shell form, and `seavx resume <id>` is the
  same session `seavx sessions resume <id>` opens.
- Sessions are named `svx-` plus eight hex digits when created. `list`, `show`,
  `export`, `resume` and both pickers print that name; a bare number from an
  older database still resolves to the same row.
- A workspace write asks in the default `ask` mode: `write_file`, `edit_file`
  and the shell's write destinations change nothing until the question is
  answered. `permitted_actions = ["write"]` or `tool_permissions = "allow"`
  stops the asking; `seavx exec`, which has no human, denies instead.
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
- The config writer is `atomic_file::write_atomic` with one rename: a crash
  leaves the old file or the new one, never neither. The backup/`.new` protocol
  and its fault-injection enum are gone, so a write no longer creates a `.bak`;
  `recover_config` stays only to resolve one an older build left behind.
- `DEFAULT_MODEL` and `DEFAULT_MAX_TURNS` have one definition, in `engine`.
- `SCHEMA_VERSION` 2 adds `sessions.slug` with a unique index; `migrate` alters
  a v1 file and names every row it already holds.

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
- A config file that sets only some keys keeps the built-in defaults for the
  rest instead of failing to parse and being ignored, so a hand-written file is
  no longer silently discarded.
- The session database's `-wal` and `-shm` sidecars are created by SQLite with
  the umask; they are tightened to `0600`, so a transcript is not
  world-readable on a multi-user machine.
- `run_command` treats a child with no pid as a failure instead of falling back
  to `0`, which signalled the agent's own process group.
- `doctor`, `update`, `uninstall` and the other non-agent commands no longer
  create `.seavx/debug.log` in the current directory: their logs go to stderr,
  so a command started where it cannot write no longer aborts.
- A turn that stops because a tool call or the same error repeated twice ends
  with an assistant row instead of a completed turn with no answer, and the
  reason is masked before it reaches the transcript.
- The arguments of a model's tool call are masked in the transcript and the
  session; the request keeps the original arguments, so the tool result still
  matches its call.
- `seavx update` checks the SHA-256 the release pointer publishes against the
  bytes it received, and aborts naming both on a mismatch. A pointer without
  the field still updates and says the download was not verified.
- `run_command` caps output in characters, so the head, the tail and the
  reported omission agree for multi-byte output.
- Streamed reasoning renders as one paragraph: a delta joins the open block
  instead of starting a line, and the block wraps at the pane width rather than
  a column two characters narrower.
- A pending permission is a bordered box between the status bar and the
  composer — tool and effect on one row, `y`/`a`/`u`/`n` on the next — instead
  of a yellow line painted over the status bar.
- `Ctrl+D`, `Ctrl+Q` and the double `Ctrl+C` exit work while a permission
  question is on screen: the prompt used to consume every key, so the only way
  out was to answer. Leaving without answering denies, never approves.

### tests

- Local SSE server fixtures: frame split at every boundary, multi-line `data:`,
  error event, `[DONE]`, abrupt end, usage and tool-call assembly.
- Fake backends for the stream fallback, mid-stream failure, per-stage
  cancellation and the confirmed/unconfirmed process-tree kill.
- Compatibility fixtures: a v1 session database built from the SQL literal, and
  a payload round-trip for every stored message shape.
- Doctor probe tests for a planted symlink, a pre-existing file and a replaced
  probe.
- A partial config merges with the defaults, the WAL sidecars are owner-only, a
  refused config write leaves the previous file in place, a stopped turn leaves
  its assistant row, tool-call arguments are masked, and the update checksum is
  pinned against a known SHA-256 vector.

### docs

- README: streaming and cancellation limits, the cleanup wording ("no orphan
  where the OS confirmed the kill"), the Unix-only platform scope, the remaining
  limits (no encryption at rest, no automatic crash rollback), the `doctor` check
  list, and the registry/palette relationship.
- `src/systems/coding.md` states the streaming and cleanup rules for the agent.

### deps

- `tokio` gains the `net` and `io-util` features for the SSE test server.
- `sha2` for the update checksum.
