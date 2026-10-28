# Seavx 🏺

[![CI & Release](https://github.com/seaavey/Seavx/actions/workflows/ci.yml/badge.svg)](https://github.com/seaavey/Seavx/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

**Seavx** is an autonomous, production-grade AI coding agent implemented in native Rust. Designed for fast code exploration, surgical file modifications, multi-turn reasoning, self-improving memory distillation, and clean Terminal User Interface (TUI).

---

## ⚡ Quick Start

### 1. One-Line Install
```bash
curl -fsSL https://raw.githubusercontent.com/seaavey/Seavx/master/install.sh | bash
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

---

## 🛡️ Built-in Capabilities & Security

- **Safe Workspace Jail**: Canonical path verification preventing path traversal outside workspace.
- **Credential Protection**: Hardened blacklist against sensitive patterns (`.env*`, SSH keys, `.git` metadata).
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
