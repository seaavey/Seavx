# Security

- **Safe Workspace Jail**: Canonical path verification, plus a pre-open identity snapshot compared against the opened descriptor — a swapped symlink or an in-place edit between validation and open is refused.
- **Credential Protection**: Names and locations are refused before a read or a write:
  - directories: `.git`, `.ssh`, `.gnupg`, `.aws`
  - prefixes: `.env*`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `.netrc*`
  - names: `credentials`, `credentials.json`, `secrets.{json,yaml,yml,toml}`, `.npmrc`, `.pypirc`, `.htpasswd`
  - suffixes: `.pem`, `.key`, `.p12`, `.pfx`, `.kdbx`, `.jks`, `.keystore`, `.ppk`
  - and `config.toml` and `SOUL.md` inside `.seavx`, plus the checkpoint store and session database. `SOUL.md` is refused because it is injected into the system prompt ahead of the security policy, so a tool that could rewrite it could rewrite its own instructions. `.seavx/MEMORY.md`, `USER.md`, specs and the debug log stay readable.
- **Platform scope**: mode enforcement (the `0700`/`0600` policy on created files and directories, and the `0600` config/session files) and inode-based identity are **Unix-only**. On Windows the mode policy is a no-op, a backup is validated by length plus checksum, and a pre-open snapshot degrades to length, mode and modification time instead of `(dev, ino)`, weaker than an inode, and stated as such. Nothing on this page should be read as platform-neutral about either.
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
  - `save_memory`: Singularity-gated persistence with confidence scoring. Project memory is `~/.seavx/MEMORY.md` and preferences `~/.seavx/USER.md`; under `--debug` both stay in the checkout as `.seavx/MEMORY.md` and `.seavx/USER.md`. A global write asks in every mode, and prompt injection reads the checkout's file first, then a legacy root `MEMORY.md`, then the global one.
  - `skill_view`: On-demand progressive disclosure skill loader.
  - `skill_write`: Create or fully rewrite a skill.
  - `skill_patch`: Exact string replacement inside an existing skill.
  - `ask_user`: One structured question (single, multi, or free text) written to the store before it is shown. The turn pauses: no later tool call runs and no provider request starts until the answer arrives. Answer, skip and cancel each resolve once and produce exactly one correlated tool result; a restart restores the open question instead of inventing an interrupted result. Headless `exec` has no human, so it returns an explicit unavailable result instead of waiting.
- **Batched edits**: one `apply_patch` call writes all its files or none. Each file is capped at 16 MB, a batch at 64 files and 128 MB of staged bytes; the checkpoint store keeps the newest 200 batches and never evicts a live one. Batch atomicity holds while the process lives; a crash is reported at the next start (`batch interrupted: …`) and undone with `/undo`. Nothing is rolled back automatically. A journal that cannot be appended after a rename is a commit failure, not a staging one: the files already renamed are put back and the error says which. A checkpoint whose manifest can no longer be read is reported (`batch checkpoint … is unreadable`) instead of being skipped.
- **Autonomous Self-Improvement**: Post-task workflow evaluator with ≥ 75% confidence singularity gate, persisting reusable workflows to `.seavx/skills/`. Skills load from four roots in order — `.seavx/skills`, the repo's own `skills/`, `~/.seavx/skills`, `~/.agents/skills` — and a name found earlier shadows the same name later.
