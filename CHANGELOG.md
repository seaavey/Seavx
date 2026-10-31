# Changelog

All notable changes to Seavx are recorded here, grouped by category. One entry
per landed change; no entry describes behaviour that is not in the code.

## 0.1.3 — 2026-10-30

### feature

- Every tool call has a typed lifecycle event (`queued`, `running`, `completed`,
  `failed`, `cancelled`) correlated by session, turn and provider call id, and
  stored in one append-only `agent_events` table (schema v3). Summaries are
  redacted and capped at 512 characters; a permission denial is never recorded as a
  completed call.
- `/status` shows the current state, running tool, pending permission or
  question and the active goal. It works mid-turn and never dumps event history
  or raw arguments.
- `/ask <question>` runs exactly one turn in the read-only Ask mode without
  changing the standing Plan/Build selection. Answers cite `path:line`, and a
  citation is only accepted when this turn's `read_file`/`search_files`
  actually returned those lines.
- `/review [working-tree|staged|commit <ref>|base <ref>]` resolves the diff
  host-side (fixed arguments, no shell, 30 s timeout, 1 MiB cap, protected and
  symlinked paths withheld and counted) and reviews it in a read-only mode. It
  never stages, commits or writes.
- Goals are durable (schema v4): `/goal <objective>`, `/goal edit`, `criteria
  add|remove`, `task add|remove`, `approve`, `show`, `pause`, `resume`,
  `cancel`, and `/tasks`. One active-or-blocked goal per workspace; completion
  needs every criterion satisfied with evidence or waived with a reason.
- `ask_user` gives the model one structured question (single, multi, free text).
  The question is stored before it is shown (schema v5), survives a restart as
  pending, and the recovery pass never fabricates an interrupted result for it.
- `/steer <text>` reaches the running turn at its next safe boundary without
  touching the cancellation token; `/queue <prompt>` holds one prompt until the
  turn succeeds and drops it on cancel or failure. Both refuse a second request
  while one is pending.
- `/test` proposes the command the changed files' manifest declares (`cargo
  test` for a Cargo manifest, `npm test --silent` for a package manifest), shows
  it, and runs it only after the permission matrix agrees: `deny` refuses, an
  already-permitted category runs, and only `ask` prompts. Pass/fail comes from
  the exit status; Cargo counts are parsed when present and reported as
  unavailable when the output was truncated or unrecognised.
- `/sessions fork <id> [--at <n>]`, `archive <id>`, `unarchive <id>` and
  `find <text> [--all]` (schema v6: `parent_session_id`, `fork_seq`,
  `archived_at`), plus `seavx sessions fork|archive|unarchive|search`. A fork
  copies a completed prefix into an independent session; archiving hides a row
  without deleting anything.
- `/preset [name]` applies static prompt text from `.seavx/prompts/<name>.md`,
  falling back to `~/.seavx/prompts/<name>.md`. Project presets shadow user
  presets; traversal names, symlinks, non-regular and oversized files are
  refused. A preset only fills the next prompt. `/preset add <file.md>` copies
  a Markdown file into the project presets under its stem, accepting the `@`
  picker's `[file:…]` tag as the path and refusing an existing name instead of
  overwriting it.

### fix

- The `/sessions` picker reads the store on the blocking pool instead of the
  event thread, like every other store call.
- `/goal criteria satisfy <n> <evidence>` and `/goal criteria waive <n>
  <reason>`: a goal carrying criteria could not be completed before, because
  nothing in the shipped commands could move a criterion out of `pending`.
- `/test` runs through the permission matrix with the command's real kind, so
  `tool_permissions = "deny"` refuses it instead of prompting and running.
- A goal task is completed by a turn that succeeded, blocked by a failed turn
  and cancelled by a cancel; `/tasks` no longer reports a failure as finished.
- `fork` requires the end of a completed turn and defaults to the newest one, so
  a fork never copies a tool call without its result or an unanswered prompt.
- `/status` reports a pending clarification, not only a pending permission.
- `/steer` is stored as the user row the model received, so a resumed session
  never replays an answer to an instruction it does not contain.
- `/review` fails when Git output could not be read to the end instead of
  reviewing a truncated diff, and kills the Git process group on timeout.
- Ask only annotates `path:line` tokens that name a file, so a clock time or a
  URL port is left alone.
- `/preset` opens a searchable picker: typing filters the names, the selected
  preset's first lines are previewed, and Enter puts it in the next prompt.
  `/preset <name>` still applies one directly.
- `/test` proposes a suite only when the change is one that suite covers: a
  `.rs` file or `Cargo.toml` in a Cargo workspace, a script file or
  `package.json` in a package workspace. A README-only change proposes nothing
  instead of running the whole suite.

### tests

- Session migrations are covered from the v1, v2, v3, v4 and v5 schemas and
  from a database a real 0.1.2 binary wrote.

## 0.1.2 — 2026-10-30

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
- Debug mode is `--debug` or `SEAVX_DEBUG=1|true`, and nothing else: a debug
  build no longer counts, so `cargo run` keeps its data in `~/.seavx` like an
  installed binary. With neither, the run is not a debug run.
- Every TUI run writes the state it resolved into the workspace `.env` as
  `SEAVX_DEBUG=true|false` — the file is created when missing, the line is
  rewritten in place, and every other line is left byte-for-byte. `seavx exec`
  still writes nothing.
- A repository can ship its skills in the open: `skills/<category>/<name>/SKILL.md`
  is now a load root, second after `.seavx/skills` and ahead of the user's own
  skills, so a repo skill shadows a user skill of the same name. `skill_write`
  still targets `.seavx/skills`.
- The `/model` picker is a search box: typing narrows the list to names
  containing the query (case-insensitive), Backspace widens it, and the title
  shows `matched/total`. Letters are text now, so `j`/`k` no longer navigate and
  the `/models` refresh moved to `ctrl+r`. The wheel moves the picker's cursor
  instead of scrolling the transcript behind it.
- `seavx setup` and a bare `seavx model` ask with a list instead of a typed
  name: the endpoint's own models first (arrows move, typing filters
  case-insensitively, ten rows at a time with `↑/↓ N more`, the configured model
  preselected), then the reasoning-effort levels as a short arrow list that needs
  no search box. Esc exits non-zero and writes nothing. A piped run keeps the
  typed prompts, so scripted setup is unchanged. The live form also shows the
  protocols inferred for the endpoint and the model as soon as it is chosen.
- A base URL is accepted only when it is a web endpoint: `http` or `https` with
  a host. A bare `host:port` is read as `https`; `ftp://`, `file://`, a path such
  as `/v1/`, or a value containing whitespace is refused with the scheme named —
  never the rest of the input, which may carry a token — before any request is
  made. `HTTPS://host` is no longer prefixed into a broken URL.

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
- The `/model` picker refreshes off the UI thread: the refresh key used to await
  the gateway on the event loop, which froze the terminal for up to the request
  timeout. The title now shows a spinner while the fetch runs, the result lands
  as an event, and a second `ctrl+r` is ignored until the list arrives.
- `/sessions` opens the session picker (it used to print a numbered list) and
  still resumes a number directly; `/resume` is its alias, same arguments and
  same effect.
- Resuming a session (the `/sessions` picker or `/sessions <n>`) replaces the
  screen instead of appending to it: the previous transcript, compaction
  summary, read-only flag and unsent draft no longer carry into the loaded
  session.
- `/clear` and `/new` reset the footer's token counter: the new session showed
  the previous session's usage until the first reply landed.
- The model and session pickers scroll with the cursor and paint the selected
  row with a background. A list longer than the box stayed pinned to its first
  rows, so the cursor walked off the bottom and moving down looked dead.
- A streamed turn no longer draws its reasoning twice. The provider's captured
  reasoning repeats the deltas already sent, and the engine re-emitted it after
  the answer, so the TUI appended a second gray block below the reply; the
  captured copy now only feeds the unstreamed path.
- `/model` with no argument opens the picker on a stale cache instead of
  reporting "No cached models found". `get_cached_models` still returns `None`
  past its 24 h TTL so `seavx model` keeps refreshing; the picker reads the new
  `get_cached_models_allow_stale`.
- `/model <number>` outside the cached list is refused with the list size,
  instead of being saved as a model name — a cold cache plus `/model 3` used to
  write `model = "3"` to `config.toml`.

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
- `skills/autonomous-agents/seavx/SKILL.md` documents the agent as shipped: CLI,
  the debug rule and both data locations, slash commands, config keys, the
  permission model, sessions and undo.
- `skills/autonomous-agents/seavx-dev/SKILL.md` states the rule this repository
  works under: any coding session runs with `-d` or `SEAVX_DEBUG=true` so the
  data lands in the repo `.seavx/`, and every feature ships with a skill entry.

### deps

- `tokio` gains the `net` and `io-util` features for the SSE test server.
- `sha2` for the update checksum.
