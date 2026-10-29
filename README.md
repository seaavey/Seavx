# Seavx 🏺

[![CI & Release](https://github.com/seaavey/Seavx/actions/workflows/ci.yml/badge.svg)](https://github.com/seaavey/Seavx/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

**Seavx** is an autonomous, production-grade AI coding agent implemented in native Rust. Designed for fast code exploration, surgical file modifications, multi-turn reasoning, self-improving memory distillation, and clean Terminal User Interface (TUI).

---

## ⚡ Quick Start

### 1. One-Line Install (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/seaavey/Seavx/master/install.sh | bash
```

### 1b. Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/seaavey/Seavx/master/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Programs\seavx` and adds it to your user `PATH`. Manual alternative: download
`seavx-windows-x86_64.exe` from the [latest release](https://github.com/seaavey/Seavx/releases/latest) or
`cargo install --path .`.

### 1c. Prebuilt builds (sha-tagged)

Every push to master publishes artifacts named after its commit and mirrors them to the CDN:

```bash
curl -sI https://cdn.seavx.com/latest/linux/x86_64   # 302 → newest linux build
curl -sIL https://cdn.seavx.com/latest/windows/x86_64
curl -O https://cdn.seavx.com/builds/<sha>/seavx-linux-x86_64
```

### 2. Run Interactive Agent

```bash
seavx
```

### 3. Headless CLI Execution

```bash
seavx exec "Inspect Cargo.toml and list dependencies"
```

---

## 🧭 Modes

- **Build** (default): all tools, including file writes and command execution.
- **Plan**: read-only exploration; write and command tools are disabled.

Use `/plan` to enter Plan Mode, `/plan off` to return to Build,
or toggle with `Ctrl+Tab` (`Shift+Tab` on terminals that do not report Ctrl+Tab).

---

## ⌨️ Prompt Input & History

While you type, the prompt box completes from what you already wrote:

- `/` opens the command menu, `@` searches the workspace. `↑`/`↓` moves the
  selection, `Tab` or `Enter` takes it.
- `↑`/`↓` on a single-line prompt recalls earlier prompts that start with the
  typed prefix; going forward past the newest entry restores your draft.
- The newest match also grows back as a dimmed tail. `Tab`, `→` or `Enter`
  accepts it, and typing replaces it.
- `Esc` closes the ghost suggestion, then an open menu, then cancels the
  running turn; with nothing open or running it exits. `Ctrl+C` twice also
  exits.

Prompts are kept per workspace in `.seavx/svx_history` (gitignored, newest 500),
so recall survives restarts and picks up prompts written by another session.

---

## 🗂️ Sessions

A TUI conversation is stored in SQLite, so a session closed mid-conversation can
be resumed with its message order, tool calls and results intact. `seavx exec`
stays session-free and writes nothing.

- Location: `~/.seavx/sessions.db`, or `.seavx/sessions.db` in the workspace in
  debug mode. The file is `0600` and its directory `0700`; a looser mode left by
  an older install is tightened on start.
- `journal_mode=WAL` with `synchronous=NORMAL`: a final message lost in an
  unclean shutdown is acceptable, a whole session is not. Move to `FULL` if that
  trade ever matters.
- Prompts and assistant text are stored verbatim — the database is conversation
  history, not a log. Tool output is redacted before it is stored; checkpoint
  backups stay byte-for-byte.
- Resume replays stored messages, newest turns up to 80 000 bytes, cut at a user
  row; anything older becomes one `System: N older messages omitted` line. No
  summarisation and no network call on start.
- `payload_format` records the writer (`genai-chat-v1`). A session written by
  another format opens read-only: it renders, lists and exports, and new turns
  are refused with the reason.
- A turn that never finished reads `pending`, never `interrupted`. rc.3 has no
  lease, so a live writer and a dead one look alike; `interrupted` means a
  recovery pass already wrote the synthetic tool result.
- An unanswered prompt is shown once, prefilled in the input, and excluded from
  the next request by the replay rule; pressing Enter sends one new row, never a
  duplicate.
- Every session is named `svx-` plus eight hex digits at creation, and that name
  is what `list`, `show`, `export`, `resume` and the pickers print. It is random,
  not sequential, so it does not leak how many sessions exist. A bare number from
  an older build still resolves to the same row.

```bash
seavx sessions list          # slug, age, messages, state, title
seavx sessions show svx-2f124b69   # full transcript
seavx sessions export svx-2f124b69 > session.json
seavx sessions resume svx-2f124b69 # open the TUI with that session
seavx resume                 # list them and pick a number
seavx resume svx-2f124b69    # same as sessions resume
```

Retention: sessions are not pruned (roughly 1 KB per turn). To delete, stop
Seavx and remove the database: `rm ~/.seavx/sessions.db*`. To back up, copy
`sessions.db` with its `-wal` and `-shm` files while Seavx is closed, or run
`PRAGMA wal_checkpoint(TRUNCATE)` first. `/clear` starts a new session; the old
one stays listed and resumable.

---

## 🧠 Context & Compaction

A request is measured before it is sent, and compacted when it would not fit.
The measurement is a **serialized-request estimate**: `ceil(chars/4)` over the
JSON of the whole request (system prompt, history, tool calls, tool results,
tool schemas, the task). It is a heuristic, not a promise — it can under-count
CJK or code-dense text by up to ~2x — so overflow is handled rather than
declared impossible.

| Model family                                       | Window    | Source                                            |
| -------------------------------------------------- | --------- | ------------------------------------------------- |
| `claude`, `gemini`, `nemotron`                     | 1 000 000 | known                                             |
| `deepseek`, `glm`, `gpt-4o`, `gpt-4.1`, `o3`, `o4` | 128 000   | known                                             |
| anything else                                      | 64 000    | assumed — the footer and `doctor` say `(assumed)` |

- `request_budget` is 80 % of the window, `keep_recent` 60 % of that budget.
- Above the budget the older turns are summarised through one chunking rule,
  `CHUNK_CHARS = 24 000` characters. The summary enters the next request as a
  single user message with the `# SESSION CONTEXT (compacted)` banner; a turn is
  never split and a message is never truncated.
- The summary must be at least 100 characters and carry goals, decisions, files
  and pending sections. A summary that fails the gate, or a summariser error,
  drops the older turns and prints `System: compaction failed (<reason>); older
turns dropped` — a bad summary is worse than none.
- After compaction the request is re-measured; a summary that still does not fit
  shrinks `keep_recent` by 10 % (at most three times), then every kept turn
  except the newest is dropped, with a warning. A single turn bigger than the
  budget is sent whole, and the warning names the estimate and the model.
- One retry per turn, never two: a provider rejection of the replayed tool calls
  rebuilds the window at half the turns; a provider _capacity_ error (a message
  naming context length, token limit or prompt-too-long) re-compacts against 40 %
  of the budget. Either is retried once; after that the turn fails with the
  provider's message. A turn that already produced output is never retried.
- `max_turns` is a config key and new configs default to 100. An existing file
  keeps the value it stores. Repeating the same tool call with the same
  arguments, or the same error twice in a row, ends the loop with `stopped: …`
  instead of burning the budget.

---

## 🌊 Streaming & Cancelling

The answer streams by default: tokens appear in the transcript as the provider
produces them, rather than after the whole response arrives.

- `stream = true` (default) streams; `stream = false` sends one non-streaming
  request per turn. `false` is the documented fallback for an endpoint that
  refuses to stream, and it is the only setting that changes.
- A stream that fails **before the first chunk** is retried once as a
  non-streaming request. That retry is the turn's single allowed retry: if it
  also fails, the turn fails with that error.
- A stream that fails **after a chunk arrived** is never retried. The received
  prefix stays on screen and is stored with `partial` status, so a resumed
  session shows it and continues from there.
- `Esc` cancels: it closes the ghost suggestion first, then an open menu, then
  cancels the running turn, and only exits when nothing is open or running.
  `Ctrl+C` twice still quits immediately.
- A cancelled turn never looks complete. It ends with `System: cancelled`, the
  text received so far is kept as `partial` (or the row is `cancelled` when
  nothing arrived), and no further request is sent for it.
- A cancelled `run_command` kills the whole process tree — the child and
  everything it started — and its result carries `outcome: cancelled` while
  staying `complete`, so the transcript is not rewritten.
- Cleanup is reported, not assumed: no orphan process or task **where the OS
  confirmed the kill**. When the kill was sent but not confirmed within two
  seconds, the turn still ends as cancelled and prints
  `System: cancellation could not be confirmed; pid <pid> may still be running`.
  An unconfirmed cleanup is never described as clean.

---

## ⚙️ Configuration

Configure a provider once per machine (base URL, API key, model, reasoning effort):

```bash
seavx setup providers
```

This writes `~/.seavx/config.toml` with owner-only permissions. Before saving it calls the endpoint's `/models` and reports reachability, so a wrong URL or rejected key shows up immediately. A workspace `.seavx/config.toml` still takes precedence.

Initialize project-local settings:

```bash
seavx init
```

Configuration resides in `.seavx/config.toml`:

```toml
model = "openai/gpt-4o-mini"
max_turns = 100
base_url = "https://api.openai.com/v1/"
api_key = "sk-..."
reasoning_effort = "highx"
stream = true
```

`base_url` accepts any OpenAI-compatible endpoint. When a gateway routes several providers, prefix the model with its route (`<provider>/<model>`) and point `base_url` at that gateway.

### Switch Models & Reasoning

```bash
seavx model
# Or directly:
seavx model openai/gpt-4o-mini --effort highx
```

### TUI Slash Commands

Type `/` in the interactive prompt to list and run these commands:

| Command                                       | Description                                                                                                                                                                                                               |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/model [model] [effort]`                     | Switch model and reasoning effort, or open the picker.                                                                                                                                                                    |
| `/compact [focus]`                            | Summarize the session and keep the summary as context.                                                                                                                                                                    |
| `/config`                                     | Show the effective configuration and where each value comes from.                                                                                                                                                         |
| `/config path`                                | Show workspace and global config paths and which one is active.                                                                                                                                                           |
| `/config get <key>`                           | Show one effective value with its source.                                                                                                                                                                                 |
| `/config set [--global] <key> <value>`        | Validate and persist one key.                                                                                                                                                                                             |
| `/config unset [--global] <key>`              | Remove one key.                                                                                                                                                                                                           |
| `/plan [on\|off]`                             | Enter or leave Plan Mode.                                                                                                                                                                                                 |
| `/clear`                                      | Clear the transcript and any compacted context.                                                                                                                                                                           |
| `/new`                                        | Alias for `/clear`.                                                                                                                                                                                                       |
| `/help`                                       | Show help and the command palette.                                                                                                                                                                                        |
| `/init`                                       | Create `.seavx/config.toml` with defaults.                                                                                                                                                                                |
| `/doctor [--offline]`                         | Check config, key, endpoint, cache, workspace, memory, and skills. The session check opens the database read-only, never creates it, and warns when its schema version or payload format is not the one this build reads. |
| `/memory [show\|project\|user\|skills\|path]` | Inspect persistent memory and installed skills.                                                                                                                                                                           |
| `/undo`                                       | Restore files from the newest checkpoint and remove it; repeat to walk back. A file edited after the agent wrote it is left alone and named, so undo never overwrites newer work.                                         |
| `/sessions [number]`                          | List this workspace's sessions and resume one by its listed number.                                                                                                                                                       |
| `/resume`                                     | Pick a session from this workspace's list with the arrow keys and open it.                                                                                                                                                |
| `/exit`                                       | Quit the TUI.                                                                                                                                                                                                             |

Permission keys, written by `/config set` and by the permission prompt itself:

```toml
tool_permissions = "ask"          # deny | ask | allow
permitted_actions = ["execute"]   # write, execute, network — only these are liftable
```

`/config` never writes `api_key`; use `seavx setup providers` to change keys. Config writes preserve comments and unknown keys and are atomic.

The palette, `/help` and this table are generated from one registry in
`src/tui/commands.rs`, so a command cannot be suggested without being
parseable — and a test fails when this table and the registry disagree.

`seavx doctor` (and `/doctor`) reports one line per check: `binary`, `config`,
`config-recovery`, `secret`, `context`, `permissions`, `stream`, `endpoint`,
`cache`, `workspace`, `session`, `memory`, `skills`. The storage check prints the
session database path, its schema version, payload format and writability
without writing to it; a pending `.bak` conflict is named instead of logged; and
the endpoint probe is skipped with `--offline`. No check prints an API key, a URL
query or userinfo.

---

## 🛡️ Built-in Capabilities & Security

- **Safe Workspace Jail**: Canonical path verification, plus a pre-open identity snapshot compared against the opened descriptor — a swapped symlink or an in-place edit between validation and open is refused.
- **Credential Protection**: Names and locations are refused before a read or a write:
  - directories: `.git`, `.ssh`, `.gnupg`, `.aws`
  - prefixes: `.env*`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `.netrc*`
  - names: `credentials`, `credentials.json`, `secrets.{json,yaml,yml,toml}`, `.npmrc`, `.pypirc`, `.htpasswd`
  - suffixes: `.pem`, `.key`, `.p12`, `.pfx`, `.kdbx`, `.jks`, `.keystore`, `.ppk`
  - and `config.toml` and `SOUL.md` inside `.seavx`, plus the checkpoint store and session database. `SOUL.md` is refused because it is injected into the system prompt ahead of the security policy, so a tool that could rewrite it could rewrite its own instructions. `.seavx/MEMORY.md`, `USER.md`, specs and the debug log stay readable.
- **Platform scope**: mode enforcement (the `0700`/`0600` policy on created files and directories, and the `0600` config/session files) and inode-based identity are **Unix-only**. On Windows the mode policy is a no-op, a backup is validated by length plus checksum, and a pre-open snapshot degrades to length, mode and modification time instead of `(dev, ino)` — weaker than an inode, and stated as such. Nothing in this README should be read as platform-neutral about either.
- **Limits stated, not implied**: there is no encryption at rest — sessions, memory files and checkpoints are plain text on disk, protected by file modes only. Nothing rolls a crashed run back automatically: file writes are staged and checkpointed so `/undo` restores them, but recovery after a crash is a manual `/undo`, never a silent rewrite.
- **Permission Matrix**: every tool call is decided by one matrix. Writes inside the workspace are ungated because the path policy confines them and checkpoints make them reversible; anything that can leave the workspace asks.

  | `tool_permissions` | read  | write (in workspace) | global write | allowlisted command | other command | network | destructive |
  | ------------------ | ----- | -------------------- | ------------ | ------------------- | ------------- | ------- | ----------- |
  | `deny`             | allow | deny                 | deny         | allow               | deny          | deny    | deny        |
  | `ask` (default)    | allow | **ask**              | **ask**      | allow               | ask           | ask     | deny        |
  | `allow`            | allow | allow                | **ask**      | allow               | allow         | allow   | deny        |
  - **Global writes ask in every mode**, `allow` included: memory files (`~/.seavx/MEMORY.md`, `~/.seavx/USER.md`) and skills under `~/.seavx/skills` or `~/.agents/skills`. They are checkpointed before the write, so `/undo` restores them. The shell reaches the same floor: a `cp`/`mv`/`tee`/`dd`/redirection destination inside those roots asks, and one inside the checkpoint store is refused.
  - The **destructive floor** (`rm -rf`, `mkfs`, `dd of=/dev/…`, `shutdown`, `reboot`, fork bombs) is refused unconditionally; no key or config lifts it.
  - **A workspace write asks in the default mode.** `write_file`, `edit_file` and the shell's write destinations change nothing until you answer, so the agent cannot edit your tree silently. `allow` mode stops the asking; `permitted_actions = ["write"]` lifts that one kind.
  - `permitted_actions = ["write", "execute", "network"]` lifts those kinds from ask to allow in `ask` mode only. `global-write` and `destructive` are not accepted there — naming one is a config error.
  - Answers: **once** (this call), **category** (appends to `permitted_actions`), **unrestricted** (`tool_permissions = "allow"`). Quitting while a question is pending counts as a denial; `seavx exec` always denies and says so in the tool result.
  - A tool that picks its own destination is judged by that destination, not by what it calls itself: `skill_write` and `save_memory` write under the global roots, so they ask like any other global write.
  - Rule-of-thumb: a key command that cannot be proven read-only asks. Any `rg`/`grep` that walks a directory asks, and its output is filtered line by line afterwards (`[N lines withheld by policy]`). A read operand outside the workspace asks too (`cat ../x`, `cat /etc/passwd`, `ln -s`), because the shell follows the path and a name check is not containment. `cargo test` and `cargo build` ask: both run the workspace's own code and build scripts.

- **Redaction**: tool output, tool errors, permission prompts and log lines are masked (`api_key=…`, `Bearer …`, `sk-`/`ghp_`/`xoxb-`/`ntn_`/`AKIA…`, private-key bodies). User prompts and assistant text stay verbatim; a tool call's arguments are masked in the transcript and the session but sent to the provider unchanged; checkpoint backups are byte-for-byte.
- **Not prevented** (stated, not implied): deliberate shell obfuscation (`eval`, base64, `curl | sh`, a path assembled inside a script), exfiltration by an allowed command, timing channels, and any OS-level containment. This is a gate, not a sandbox; closing that gap needs `openat2(RESOLVE_BENEATH)`, namespaces or seccomp.
- **Unix-only**: file-mode policy and hard-link refusal are enforced on Unix; on Windows the mode policy is a no-op and backup identity degrades to length plus checksum.
- **Coding Tools**:
  - `read_file`: Line pagination (offset/limit) & truncation guards.
  - `write_file`: Safe directory creation and atomic file writing.
  - `edit_file`: Precise search-and-replace. A snippet that occurs more than once is refused with the count and up to three samples; pass `replace_all: true` to change every occurrence.
  - `apply_patch`: Strict unified diff over one or more files, applied in memory first and committed as one unit. Exact context only — no fuzz, no 3-way merge. A create patch (`--- /dev/null`) is refused when the target already exists: splicing new lines in front of the user's content is not a create.
  - `search_files`: Fast in-process grep & filename globbing (ignores `.git`, `target`, `node_modules`). The walk never follows a symlink, and a file over 2 MB is skipped rather than read whole.
  - `run_command`: Bounded execution with a 30 s timeout. Output is drained against a fixed two-second window, so a child the shell left behind (`sleep 30 &`) cannot hold the turn open; a timeout reports an unconfirmed cleanup with its pid.
  - `save_memory`: Singularity-gated persistence with confidence scoring.
  - `skill_view`: On-demand progressive disclosure skill loader.
  - `skill_write`: Create or fully rewrite a skill.
  - `skill_patch`: Exact string replacement inside an existing skill.
- **Batched edits**: one `apply_patch` call writes all its files or none. Each file is capped at 16 MB, a batch at 64 files and 128 MB of staged bytes; the checkpoint store keeps the newest 200 batches and never evicts a live one. Batch atomicity holds while the process lives; a crash is reported at the next start (`batch interrupted: …`) and undone with `/undo`. Nothing is rolled back automatically. A journal that cannot be appended after a rename is a commit failure, not a staging one: the files already renamed are put back and the error says which. A checkpoint whose manifest can no longer be read is reported (`batch checkpoint … is unreadable`) instead of being skipped.
- **Autonomous Self-Improvement**: Post-task workflow evaluator with ≥ 75% confidence singularity gate, persisting reusable workflows to `.seavx/skills/`.

---

## 📜 License

Distributed under the [MIT License](LICENSE). Copyright (c) 2026 Muhammad Adriansyah (Seaavey).
