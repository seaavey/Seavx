# Changelog

All notable changes to Seavx are recorded here, grouped by category. One entry
per landed change; no entry describes behaviour that is not in the code.

## 0.1.1-rc.3 — 2026-10-29

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
  where the OS confirmed the kill"), the Unix-only platform scope, the `doctor`
  check list, and the registry/palette relationship.
- `src/systems/coding.md` states the streaming and cleanup rules for the agent.

### deps

- `tokio` gains the `net` and `io-util` features for the SSE test server.
