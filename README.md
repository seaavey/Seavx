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

Initialize default settings:
```bash
seavx init
```

Configuration resides in `.seavx/config.toml`:
```toml
model = "zen/deepseek-v4.1-flash-free"
max_turns = 25
base_url = "https://seavx.ai.id/v1/"
api_key = "sr-live-..."
reasoning_effort = "highx"
```

### Switch Models & Reasoning
```bash
seavx model
# Or directly:
seavx model zen/deepseek-v4.1-flash-free --effort highx
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
  - `read_skill`: On-demand progressive disclosure skill loader.
- **Autonomous Self-Improvement**: Post-task workflow evaluator with ≥ 75% confidence singularity gate, persisting reusable workflows to `.seavx/skills/`.

---

## 📜 License

Distributed under the [MIT License](LICENSE). Copyright (c) 2026 Muhammad Adriansyah (Seaavey).
