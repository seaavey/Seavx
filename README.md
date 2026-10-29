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
- `Esc` closes the ghost suggestion, then an open menu; with nothing open it
  exits. `Ctrl+C` twice also exits.

Prompts are kept per workspace in `.seavx/svx_history` (gitignored, newest 500),
so recall survives restarts and picks up prompts written by another session.

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
max_turns = 25
base_url = "https://api.openai.com/v1/"
api_key = "sk-..."
reasoning_effort = "highx"
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

| Command | Description |
| --- | --- |
| `/model [model] [effort]` | Switch model and reasoning effort, or open the picker. |
| `/compact [focus]` | Summarize the session and keep the summary as context. |
| `/config` | Show the effective configuration and where each value comes from. |
| `/config path` | Show workspace and global config paths and which one is active. |
| `/config get <key>` | Show one effective value with its source. |
| `/config set [--global] <key> <value>` | Validate and persist one key. |
| `/config unset [--global] <key>` | Remove one key. |
| `/plan [on\|off]` | Enter or leave Plan Mode. |
| `/clear` | Clear the transcript and any compacted context. |
| `/help` | Show help and the command palette. |
| `/init` | Create `.seavx/config.toml` with defaults. |
| `/doctor [--offline]` | Check config, key, endpoint, cache, workspace, memory, and skills. |
| `/memory [show\|project\|user\|skills\|path]` | Inspect persistent memory and installed skills. |
| `/undo` | Restore files from the newest checkpoint and remove it; repeat to walk back. |
| `/exit` | Quit the TUI. |

Permission keys, written by `/config set` and by the permission prompt itself:
```toml
tool_permissions = "ask"          # deny | ask | allow
permitted_actions = ["execute"]   # execute, network — only these are liftable
```

`/config` never writes `api_key`; use `seavx setup providers` to change keys. Config writes preserve comments and unknown keys and are atomic.

---

## 🛡️ Built-in Capabilities & Security

- **Safe Workspace Jail**: Canonical path verification, plus a pre-open identity snapshot compared against the opened descriptor — a swapped symlink or an in-place edit between validation and open is refused.
- **Credential Protection**: Names and locations are refused before a read or a write:
  - directories: `.git`, `.ssh`, `.gnupg`, `.aws`
  - prefixes: `.env*`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `.netrc*`
  - names: `credentials`, `credentials.json`, `secrets.{json,yaml,yml,toml}`, `.npmrc`, `.pypirc`, `.htpasswd`
  - suffixes: `.pem`, `.key`, `.p12`, `.pfx`, `.kdbx`, `.jks`, `.keystore`, `.ppk`
  - and `config.toml` inside `.seavx`, plus the checkpoint store and session database. `.seavx/MEMORY.md`, `USER.md`, specs and the debug log stay readable.
- **Permission Matrix**: every tool call is decided by one matrix. Writes inside the workspace are ungated because the path policy confines them and checkpoints make them reversible; anything that can leave the workspace asks.

  | `tool_permissions` | read | write (in workspace) | global write | allowlisted command | other command | network | destructive |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | `deny` | allow | deny | deny | allow | deny | deny | deny |
  | `ask` (default) | allow | allow | **ask** | allow | ask | ask | deny |
  | `allow` | allow | allow | **ask** | allow | allow | allow | deny |

  - **Global writes ask in every mode**, `allow` included: memory files (`~/.seavx/MEMORY.md`, `~/.seavx/USER.md`) and skills under `~/.seavx/skills` or `~/.agents/skills`. They are checkpointed before the write, so `/undo` restores them. The shell reaches the same floor: a `cp`/`mv`/`tee`/`dd`/redirection destination inside those roots asks, and one inside the checkpoint store is refused.
  - The **destructive floor** (`rm -rf`, `mkfs`, `dd of=/dev/…`, `shutdown`, `reboot`, fork bombs) is refused unconditionally; no key or config lifts it.
  - `permitted_actions = ["execute", "network"]` lifts those two kinds from ask to allow in `ask` mode only. `global-write` and `destructive` are not accepted there — naming one is a config error.
  - Answers: **once** (this call), **category** (appends to `permitted_actions`), **unrestricted** (`tool_permissions = "allow"`). Quitting while a question is pending counts as a denial; `seavx exec` always denies and says so in the tool result.
  - Rule-of-thumb: a key command that cannot be proven read-only asks. Any `rg`/`grep` that walks a directory asks, and its output is filtered line by line afterwards (`[N lines withheld by policy]`).
- **Redaction**: tool output, tool errors, permission prompts and log lines are masked (`api_key=…`, `Bearer …`, `sk-`/`ghp_`/`xoxb-`/`ntn_`/`AKIA…`, private-key bodies). User prompts and assistant text stay verbatim; checkpoint backups are byte-for-byte.
- **Not prevented** (stated, not implied): deliberate shell obfuscation (`eval`, base64, `curl | sh`, a path assembled inside a script), exfiltration by an allowed command, timing channels, and any OS-level containment. This is a gate, not a sandbox; closing that gap needs `openat2(RESOLVE_BENEATH)`, namespaces or seccomp.
- **Unix-only**: file-mode policy and hard-link refusal are enforced on Unix; on Windows the mode policy is a no-op and backup identity degrades to length plus checksum.
- **Coding Tools**:
  - `read_file`: Line pagination (offset/limit) & truncation guards.
  - `write_file`: Safe directory creation and atomic file writing.
  - `edit_file`: Precise search-and-replace patching.
  - `search_files`: Fast in-process grep & filename globbing (ignores `.git`, `target`, `node_modules`).
  - `run_command`: Bounded execution with 30s timeout.
  - `save_memory`: Singularity-gated persistence with confidence scoring.
  - `skill_view`: On-demand progressive disclosure skill loader.
  - `skill_write`: Create or fully rewrite a skill.
  - `skill_patch`: Exact string replacement inside an existing skill.
- **Autonomous Self-Improvement**: Post-task workflow evaluator with ≥ 75% confidence singularity gate, persisting reusable workflows to `.seavx/skills/`.

---

## 📜 License

Distributed under the [MIT License](LICENSE). Copyright (c) 2026 Muhammad Adriansyah (Seaavey).
