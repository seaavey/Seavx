# Changelog

All notable changes to Seavx are recorded here, grouped by category. One entry
per landed change; no entry describes behaviour that is not in the code.

## Unreleased

### feat

- `seavx update` verifies the release pointer before it touches the binary: the
  minisign signature over the pointer's canonical `SEAVX-UPDATE-v1` bytes
  against keys compiled into the build, the channel the pointer was signed for,
  the target this binary was compiled for, the `builds/{sha}/{sha256}/{binary}`
  route, the version ordering, the SHA-256 of the downloaded bytes, and finally
  the candidate's own `--version` output. A failure at any step leaves the
  installed binary byte-identical and deletes the temp file, and `--force`
  skips the version ordering and nothing else: a forced downgrade downloads
  through the same checks as an upgrade. The default channel is stable (a tagged
  release); `--channel edge` reads the master pointer, one build ahead of the
  pointer a tag moves. The client and both installers now accept
  macOS arm64 beside linux x86_64 and windows x86_64 — CI already built that
  target and `latest/macos/arm64.json` already answered, so what is new is the
  update path installing it. The release key is pinned in the client and both
  installers, and CI signs the pointer on every master build, so the update path
  runs against a signed pointer on all three targets.

- The reasoning-effort pickers offer what the model publishes instead of one
  fixed list: `seavx model`, `seavx setup` and the TUI modal read
  `reasoning_options[type = effort]` from the same models.dev catalog that
  answers context windows, and keep the static list (`none`, `minimal`,
  `medium`, `high`, `highx`, `max`) only for an id the catalog does not know or
  lists with no effort option of its own.
  Providers clamp one model differently, so the entry is the union of their
  levels in menu order. Nothing validates a saved effort against it: a gateway
  can take a level the catalog does not list.
- Automatic compaction is a visible step now: the transcript shows the divider
  `----------------------------- AUTO COMPACT --------------------------------`
  with the `before → after` estimate under it, and the one that happens inside a
  tool loop says `(estimated), mid-turn`. A summary nobody sees is a summary
  nobody can trust. It also refuses to run for nothing: fewer than
  `MIN_COMPACT_MESSAGES = 4` messages older than the recent window and the
  request is sent as it is (`auto compact skipped …`) instead of replacing one
  or two rows with a one-line restatement.
- `auto_compact` is a config key (default `true`). `/config set auto_compact
  false` writes `auto_compact = false` as a TOML boolean, and the engine then
  summarises nothing and drops nothing: the over-budget request goes as it is
  with `auto_compact is off: N tokens over the M budget` in the transcript.
  `/config` and `/config get auto_compact` read the same value.
- A request that grows inside one turn is measured again before every send:
  the tool loop appends results after `prepare_request` ran, so a long read or
  build loop could exceed the window with nothing to catch it before the
  provider refused. The grown request now compacts at the same 80 % budget
  (the same `AUTO COMPACT` divider, marked `mid-turn`) and falls back to
  dropping older turns when the summary fails the gate — the system row and the
  newest turn stay whole.
- An image on the clipboard is a turn attachment: `Ctrl+V` reads it on a worker
  thread, encodes the pixels to PNG once, refuses anything over 24 MP or 4 MiB,
  dedupes by SHA-256, labels it in the transcript as
  `[image #1 1280×720 png 214 KiB]`, and sends it as a `Binary` part of the same
  user message the session stores. The model sees the image; a resumed session
  replays the same line. What waits on the next Enter is drawn on its own strip
  above the prompt (newest three, clipped before the prompt box), never in the
  transcript.
- A drag over the transcript selects text: the covered characters are painted on
  a background block and released straight to the clipboard, with a
  `copied N lines` toast. The block is stored as painted rows and columns, so the
  wheel and auto-scroll move it with the text instead of leaving it behind at
  screen coordinates; `Esc` drops it, and so does a resize, which retires the
  rows it was mapped against. A click copies nothing, and the composer, footer
  and popups keep the pointer they had. The read-only `Clipboard` trait grew
  `write`, serving from the same worker thread that owns the single `arboard`
  handle — a handle dropped after writing loses the X11 selection.
- The model picker says it can refresh: row 0 is always
  `↻ Refresh models from <host>`, reachable with the arrows and Enter and running
  the fetch `ctrl+r` already ran. A filter that matches nothing used to paint an
  empty box under `0/88`; it now says `no match for "<query>" · ctrl+r refreshes`.
  Opening the picker on a cache past its 24 h TTL refreshes it in the
  background — stale rows stay on screen behind the spinner — so a model added at
  the gateway today is not invisible for a day.

### breaking

- `/model` and the model picker no longer save anything: they switch the model
  for the turns that follow and leave the config file's default alone, which only
  `seavx model [name]` (or `/config set model …`) changes. A run that used to
  re-point the default now starts from it again next time; the transcript says
  `Model for this session: …` instead of `Saved to …`.
- `Esc` no longer exits and `Ctrl+D`/`Ctrl+Q` are not shortcuts any more:
  `Ctrl+C` twice within 1.5 s is the only way out of the TUI. `Esc` still closes
  the ghost, an open menu and the running turn, and twice on an idle prompt it
  clears the line.
- A config that names no model is refused instead of calling a built-in one.
  `model` is now optional, `seavx init` writes no `model` key, and a config that
  relied on the built-in `openai/gpt-4o-mini` fails its first turn with
  ``no model configured: run `seavx setup` or `seavx model <name>``.
  `/config`, `/status`, `doctor` and the footer say `not set` instead of an id.

### fix

- A new session shows its own title instead of `new session` until the model
  names it. The title the store already had — the prompt's first line — was
  never sent to the window, so the status line waited on a second request that
  could fail without a trace. It is now shown as soon as the session row
  exists, and the model's shorter title replaces it when it arrives. That
  request also stopped being silent: it asks with the streaming call the turn
  uses, because a gateway that only implements `stream: true` refuses the
  non-stream shape this used to send, and a failure is logged with its cause
  instead of dropped.
- `Esc` needs a second press to cancel a running turn: the first one says so in
  a toast, so a stray key cannot throw away a turn that is costing tokens.
  `Ctrl+C` remains the one-press stop. The footer names both per state — while a
  turn runs it reads `esc ×2 stop · ctrl+c stop`, and `esc ×2 clear ·
  ctrl+c exit` when idle. The generic reminders (`↑↓ history`, `tab complete`,
  `⏎ send`) are gone: they spent the row on keys a user already knows — or, for
  `tab`, on a suggestion the screen is already showing — and the token counter
  lost width to them. On a pane too narrow for both, the way out stays and the
  rest goes. Cancelling no longer goes missing either: a submitted
  prompt used to keep the ghost suggestion it grew while typing, so the first
  `Esc` cleared that invisible tail and cancelling took a press more than the
  keys promised — the ghost now goes with the prompt it belonged to. Both key
  paths are covered end to end in a real pty against a gateway that never
  answers.
- `Ctrl+C` stops a running turn instead of leaving the session: the key cancels
  the turn (as `Esc` does) and only an idle prompt treats it as the way out,
  where it still needs its second press. The footer says which of the two it is
  — `ctrl+c stop` while a turn runs, `ctrl+c exit` when idle — and its clear
  hint reads `esc ×2 clear` instead of the doubled-looking `esc esc clear`.
- Copying from the transcript works on a host with no clipboard. A drag whose
  write to the system clipboard fails now goes out as an OSC 52 sequence — the
  terminal the user is looking at copies it, which is the one channel that
  survives SSH — and to `.seavx/last-copy.txt` (mode 0600, gitignored), so a
  terminal that drops the sequence still leaves the text somewhere the user can
  read it. The toast names where it went instead of only saying the copy failed,
  and the failure itself now carries arboard's own error rather than a fixed
  sentence that hid whether there was no backend, no display, or a refused
  connection.
- The background skill evaluator retries only a call that never completed, a
  rate limit or a server fault: an answer the provider gave with a status is
  final, so a `400`/`401`/`404` no longer spends a second call on the same
  refusal. Its notice now names that status — the provider's cause line, without
  the request body — instead of the model line and a pointer at the log, and the
  sentence no longer ends in a doubled period.
- `seavx setup` acts on a typed key once on Windows: the three CLI raw-mode
  readers (`prompt::pick`, `prompt::pick_skills` and the masked key prompt) now
  drop the key-release record that crossterm 0.28 also reports as a character,
  so the key is no longer inserted twice — which made every attempt answer `401`
  — and an arrow no longer moves two rows. The wizard offers the key in effect,
  meaning an exported `SEAVX_API_KEY`/`OPENAI_API_KEY`/`ANTHROPIC_API_KEY` or the
  key in the workspace file, instead of resetting the prompt to `none`. A `401`
  reaches the same "save this endpoint anyway" prompt as any other failure
  rather than re-asking the key in a loop with no exit, and an empty base URL
  reports one clause instead of `invalid base URL: base_url is empty`.
- The Windows build signs the pointer with upstream minisign 0.12, pinned by
  digest in the workflow. Chocolatey ships 0.8, which cannot load a passwordless
  secret key — `Unsupported key derivation function` — so the pointer object the
  Windows leg publishes was the one build that never carried a signature.
- Both installers verify the release pointer before the binary lands: the
  channel, the target, the same field shapes the client enforces, the
  `builds/{sha}/{sha256}/{binary}` route and the minisign signature over the
  same canonical bytes, with the download staged in a temp location and renamed
  into place — `<exe>.tmp` beside the destination in `install.ps1`, a `mktemp -d`
  copy through a `.new` beside it in `install.sh` — so a failed check or a short
  write leaves the working binary untouched. An installer pins the same key as the
  client and refuses a pointer whose signature does not verify, instead of
  trusting a checksum that arrived unsigned. One that has the key but no
  `minisign` on `PATH` warns and verifies the checksum only, which detects
  transfer corruption and not a forged pointer.
- Catalog lookups ignore the separators a gateway spells a model with:
  `deepseek-v4-1-flash-1017`, `deepseek-v4.1-flash-1017:free` and a routed
  `rv/deepseek-ai/DeepSeek-V4.1-Flash-1017` are one key, so a gateway that
  writes `v4-1` for a model the catalog writes `v4.1` no longer loses the
  context window and the effort levels to the fallback. A cache written under
  the old keys is re-keyed on load instead of waiting a week for the refresh,
  and two spellings of one model merge: larger window, union of the levels.
- The context window comes from the public models.dev catalog instead of a
  guess from the model name. `deepseek` meant 128 000 for the whole family, so
  `DeepSeek-V4.1-Flash` — a 1M window — compacted at 102 400 tokens and refused
  the rest of it. The catalog (`limit.context` per model name, cached seven days
  in `.seavx/cache/contexts.json`, refreshed in the background at TUI start and
  once at the end of `seavx setup`) answers first; the family table stands
  behind it for ids the catalog does not list, and `(assumed)` now means the
  catalog missed too. A headless `exec` on a fresh install used to have no cache
  at all, so the 1M window it was meant to get never arrived.
- The effort pickers speak the names the config stores: a level the catalog
  publishes as `low` or `xhigh` is offered as `minimal` or `highx`, one row
  each. Before, a saved `highx` was not in a menu that said `xhigh`, so the
  picker opened on `none` and Enter silently downgraded the session.
- A mid-turn compaction is measured before it is kept: a summary that still
  leaves the request over the budget drops the older turns instead of being
  sent, so one oversized turn cannot make every later tool step of the same turn
  pay for another summary of the same rows.
- A capacity error with nothing older to fold (`auto_compact = false`, or a
  window that holds one turn) fails the turn with the provider's message
  instead of spending the turn's one retry on the request that just failed.
- The compaction prompt now asks for the headings the gate requires. The gate
  accepts a summary only when it carries `# Goals`, `# Decisions`, `# Files` and
  `# Pending`, while the prompt asked for "short headings" and listed six topics
  — so a summary written as bold labels (`**Goals:**`) or a numbered list failed
  validation and the older turns were dropped with no summary at all. Both
  paths (`/compact` and the automatic one) share that prompt, so one prompt now
  names the four headings verbatim.
- `save_memory` keeps a release run's memory in `~/.seavx`: project memory is
  `~/.seavx/MEMORY.md` and preferences `~/.seavx/USER.md`, so a lesson saved in
  one checkout is there in the next. A `--debug` run keeps both files inside the
  workspace (`.seavx/MEMORY.md`, `.seavx/USER.md`) instead of writing the real
  store. Before, a release run wrote `<workspace>/MEMORY.md` — a path `prompt`
  and `seavx memory` only read as a fallback behind `.seavx/MEMORY.md` and one
  `.gitignore` does not cover, so a saved lesson surfaced as an untracked file at
  the repository root and never reached the global store. Prompt injection,
  `seavx memory`, `/memory` and `seavx doctor` now read the global file too.
- `seavx setup` re-asks the API key when `/models` answers `401` or `403`: the
  endpoint refused that credential, so the wizard says so and loops back to the
  key prompt instead of offering `Save this endpoint anyway?`. Saving an endpoint
  the check just rejected is what let a keyless provider be written, and the
  model step then arrived with no key and an empty model list. `Save anyway?`
  stays for every other failure, where a key cannot be the fix.
- A model or effort change writes the config the loader actually reads: the
  workspace file when one exists, the global `~/.seavx/config.toml` otherwise.
  Before, `/model`, the model picker, `seavx model` and a permission approval all
  created a workspace `.seavx/config.toml`, which shadows the global config
  entirely — the `api_key` stayed behind and the next turn failed with
  `Resolver error … ApiKeyEnvNotFound { env_name: "ANTHROPIC_API_KEY" }`.
  `/config set` and `/config unset` without `--global` follow the same rule, and
  the saved-file notice names the path it wrote.
- `seavx setup --workspace` writes this repo's `.seavx/config.toml` instead of
  the global one; without it the wizard keeps writing `~/.seavx/config.toml`,
  and a workspace target is pre-filled from the config in effect so the key is
  offered again rather than asked for from scratch.
- A stream that dies on a chunk the adapter cannot parse keeps the turn's one
  retry when all it had sent was whitespace: only non-whitespace text counts as
  output. The failure now reads as
  `the provider sent a stream chunk this client cannot parse (cause logged).
  Retry the turn; if it repeats, switch model or turn streaming off.` — the raw
  JSON pointer stays in the log instead of the transcript.
- The background skill evaluator retries once when its model call never
  completed (transport, DNS, timeout) instead of skipping at the first failure,
  and its notice says how many attempts it made, that the turn is unaffected,
  and that the provider error is in the log.
- Background requests no longer inherit the turn's reasoning budget: a session
  title, a compaction summary and the skill evaluator are short transforms, and
  a proxy adapter answers extended thinking without the block `signature` genai
  requires, which failed the whole request. The evaluator's failure notice is
  also one bounded line now, not the serialized request.
- One key press runs once: `KeyEventKind::Release` is no longer translated into
  an action, so `Ctrl+V` stopped pasting twice and toggling twice.
- `Ctrl+V` and `Alt+V` paste text off the clipboard on every platform, read
  on a worker thread with a 2 s deadline so an unresponsive owner app cannot
  freeze the frame; an empty or failing clipboard says so instead of doing
  nothing.
- A paste lands in the composer instead of running as keystrokes: bracketed
  paste arrives as one string, CRLF is normalized, C0 controls are stripped, and
  text past the 256 KiB cap is truncated with a notice. Pasting three lines no
  longer submits three turns.
- Routing is decided by the config, never by the model id: only the adapters
  `normalize_base_url` registers can be reached, so an id genai does not
  recognise goes to the configured primary instead of the Ollama adapter's
  `localhost:11434`, and a client that fails to build is reported instead of
  being replaced by genai's built-in provider set.
- A key with no endpoint is no longer a provider. `OPENAI_API_KEY` and
  `ANTHROPIC_API_KEY` bind their own adapter; `SEAVX_API_KEY` alone, or a key
  read from a file, refuses the first turn with
  `no provider configured: run `seavx setup``.
- The setup wizard stops clearing the console between steps. `ClearType::All`
  fills the whole buffer on Windows CMD, scrollback included, so each repaint
  erased the steps just answered — it read as "setup jumps straight to the model
  step". The wizard now prints linearly, names the step it is on
  (`seavx setup providers · endpoint (1/3)`, `model (2/3)`, `effort (3/3)`,
  `saved`), and prints the config path from platform components, so Windows shows
  `.seavx` and `config.toml` separated by a backslash.
- `run_command` no longer throws away what it captured when it hits its ceiling,
  and the ceiling is 120 s instead of 30 s. A cold `npx skills add …` needs ~57 s:
  the old limit killed it every time, the result was one line — `Command execution
  timed out (30s limit)` — with no evidence of what the command was doing, and the
  repeat-error guard then stopped the turn. The timeout error now carries the
  stdout and stderr already drained (same caps and redaction as a completed
  result), so "slow but progressing" is distinguishable from "hung".

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
- The standing mode is one of four, cycled with `Ctrl+Tab` (`Shift+Tab` where
  the terminal does not report it): build, plan, accept-edits and auto.
  Accept-edits lifts the question for a write inside the workspace, auto lifts
  every ask, and the matrix keeps its two floors in both: a write outside the
  workspace still asks, a destructive call is still refused, and a configured
  `deny` is never lifted.
- A tool call reads as a verb and its object (`⚡ run  cargo clippy
--all-targets`), not as the argument list the model wrote, on the live
  transcript, the session line and `sessions show`. The permission prompt names
  the category the `a` key records and states the effect in words instead of
  repeating the arguments after "wants to runs the command".
- The session line carries the session title, a five-cell orb from
  `seaavey/thinking-orbs-rs` and one state word (`working`, `thinking`,
  `streaming`), never the command being run: the transcript shows the call and
  `/status` names it. The orb is still while idle.
- `/review` reads the paths Git reported literally, so a file named `:(top).env`
  cannot pull a withheld `.env` into the diff; a target whose changes are all
  withheld says so and counts them, and reviewing a root commit includes the
  changes that commit made.
- Resume — `seavx resume`, `seavx sessions resume` and the TUI picker — rebuilds
  a clarification's result from the stored answer instead of interrupting it,
  and leaves a question that is still pending alone.
- `fork` refuses a prefix in which an assistant row has a tool call without its
  result, not only a boundary row.
- Esc on `ask_user` cancels the turn: the remaining calls in the same response
  never run.
- A turn stopped by the same error twice records that call's failed event and
  its result before returning the summary.
- `/test` and every `/goal` operation hold the TUI busy until they finish, so a
  second prompt cannot run beside them; the goal prompt starts once the slot is
  free.
- `/test <extra>` is a usage error instead of running the proposed suite.
- Rebuilding the `agent_events` table keeps each event's `goal_id` instead of
  nulling it.

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
