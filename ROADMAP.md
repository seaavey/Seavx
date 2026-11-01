# Roadmap

Targets that are not built yet. Each milestone states the shape of the end
state, the decisions already made, what the current code forces, and how it is
proved. Everything here is a contract: if a milestone changes the config file,
the commands or the defaults, README, CHANGELOG and the matching skill entry are
part of that milestone, not a follow-up.

Order is by dependency, not by size. M1 first because M2 and M3 both read the
config.

---

## M1 — multiple providers in one config

### End state

```toml
# ~/.seavx/config.toml or ./.seavx/config.toml
default_provider = "seavx"

[providers.seavx]
base_url = "https://gateway.example/v1/"
api_key = "..."
model = "gpt-5.1"
reasoning_effort = "high"

[providers.deepseek]
base_url = "https://api.deepseek.com/v1/"
api_key = "..."
model = "deepseek-chat"
reasoning_effort = "medium"

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-6"
```

Everything lives under one `providers` key: one `[providers.<name>]` table per
provider, exactly as many as the user configures. Written out flat, that is the
YAML shape `providers: { seavx: { base_url = ... } }` — TOML just spells the same
tree with table headers.

- Name is the routing key, lowercase, no spaces.
- Per provider: `base_url`, `api_key`, `api_key_env`, `model`,
  `reasoning_effort`, `stream`. Nothing else until something needs it.
- Selecting: `model = "anthropic/claude-sonnet-4-6"` picks the provider by
  prefix; a bare `model = "gpt-5.1"` means `default_provider`.
- `seavx model seavx/gpt-5.1` and the `/model` picker write the prefix, so the
  provider a turn runs on is visible in the status line.
- Doctor and `/config` report every provider, the selected one, and which file
  supplied each.

### Decisions

- **TOML, not YAML.** The deciding factor is not taste: every config write in
  this repo goes through `toml_edit`, which edits one key and leaves comments,
  order and the other tables byte-identical. With N providers a write must touch
  one `[providers.<name>]` without dropping the others or their `api_key`, and
  `toml_edit` is the tool that does it; YAML has no equivalent surgical editor
  in Rust — `serde_yaml` reads, mutates and rewrites the whole document and
  loses comments (and the crate is archived). A second file format also means a
  second discovery path to document in README and `doctor`. YAML buys
  indentation here and nothing else.
- **One client per provider, not one client with N endpoints.** Today
  `AgentRuntime::new` registers every adapter `normalize_base_url` infers
  (`Anthropic` + `OpenAI` for an unknown host) and `route_to_configured` sends
  any unregistered verdict to `registered.first()`. With N providers that
  function becomes "send the turn to whichever provider happens to be first",
  which is the same class of bug as the default-host one it replaced. A provider
  gets its own `Client` built only from its own adapters; the mapper and
  `route_to_configured` are deleted rather than generalised.
- **Flat keys are legacy, not a second syntax.** A file with only `model`,
  `base_url`, ... keeps working: it is read as one provider named `default`,
  selected automatically. A file with both flat keys and `[providers.*]` uses
  the tables and prints where the ignored flat keys are. No file is rewritten
  on read.
- **`api_key_env` before `api_key`.** The key in the file wins only when the
  variable is unset, so a CI environment can override a checked-in file without
  editing it.
- **Env overrides apply to the selected provider only.** `SEAVX_PROVIDER`
  selects, `SEAVX_MODEL` / `SEAVX_BASE_URL` / `SEAVX_API_KEY` /
  `SEAVX_REASONING_EFFORT` override that provider's values. They never create a
  provider.

### What the current code forces

- `src/cli/config.rs` writers (`write_agent_config`, `set_model_and_effort`,
  `set_value`) serialise a whole `AgentConfig`. With N providers that drops every
  provider the writer did not load — the same shape as the bug fixed in
  `f3e3a01`, where a write landed in a config the loader does not read, or lost
  the key. All writers move to `toml_edit` targeted edits on the one table, and
  the round-trip test set grows to cover "edit provider B, provider A unchanged
  byte for byte".
- The workspace file still shadows the global file entirely. Merging is a
  different decision and stays out.
- `seavx setup providers` becomes a list of configured providers plus
  `seavx setup providers <name>` to add or edit one. The `/models` probe,
  `model_gate` and `prompt::pick` in `src/cli/setup.rs` are reused unchanged.

### Proof

- Parse tests: tables only, flat only, both (tables win, note printed), missing
  `default_provider`, unknown `default_provider`, duplicate name.
- Writer tests: edit one provider, other providers and the key survive.
- Routing test: a turn built for `anthropic/*` reaches the anthropic client's
  endpoint, never the primary one.
- Manual: `seavx -d doctor` lists both providers; `seavx exec` under each.

---

## M2 — MCP servers as tools

### End state

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/project"]
env = { LOG_LEVEL = "warn" }
```

- Transport: stdio only in this milestone. HTTP/SSE is a second transport behind
  the same client seam when someone needs it.
- Startup: the registry asks each server for `initialize` then `tools/list`;
  each tool becomes an `AgentTool` named `mcp__<server>__<tool>` so two servers
  cannot collide and a name tells you where it came from.
- A server that fails to start, handshake or list is reported once as a `Notice`
  and skipped. One broken server never blocks a session.
- Schema: MCP's JSON Schema is passed through as-is into the genai tool
  definition; the repo's `schemars` path stays for built-in tools.
- Calls: `tools/call` with the turn's cancel token; a timeout per call, and the
  child process is killed on drop so a crashed session leaves no orphans.

### Decisions

- MCP tools are `execute`-kind for the permission model: `ask` by default,
  liftable by `permitted_actions`, never lifted globally. A tool that can run
  `npx` is not a read.
- No tool allowlist per server in v1; the permission gate is the allowlist.
  Add one only when the tool namespace gets big enough to crowd the prompt.
- `seavx doctor` gains a section: server name, transport, status, tool count.
  No dedicated `seavx mcp` command.

### Proof

- A fixture MCP server in `src/tests/` (a small Rust binary speaking JSON-RPC
  over stdio, built by the test harness): handshake, list, call, tool error,
  server that exits during handshake, server that never answers.
- Name collision between two servers; a tool named like a built-in one.
- `cargo clippy` clean with the JSON Schema passthrough — no `unwrap` on
  untrusted server output.

---

## M3 — Telegram gateway

### End state

```
seavx gateway setup      # write [gateway.telegram], verified against the API
seavx gateway run        # long-poll, one turn per message
seavx gateway status     # identity, allowed chats, last error
```

```toml
[gateway.telegram]
token_env = "SEAVX_TELEGRAM_TOKEN"
allowed_chat_ids = [123456789]
```

- `gateway setup` asks for the token (masked), calls `getMe` so a wrong token
  fails in the wizard instead of in the loop, then polls recent updates and
  offers the chats that wrote as the allowlist — typing a numeric chat id by
  hand is the step everyone gets wrong.
- `gateway run` is one `teloxide::Dispatcher` over `Update::filter_message()`:
  one session (`svx-…`) per chat, reused across messages, so `/undo` and
  `seavx resume` work on gateway conversations like any other.
- Reply: `send_chat_action(Typing)`, then stream into a single message edited at
  most every 1.5 s, split at 4096 characters. Streaming is already an event, so
  this is renderer code, not a second engine. `ApiError::MessageNotModified` is
  success, not a failure to log.
- Slash commands that map cleanly (`/new`, `/model`, `/undo`, `/sessions`) are
  branches on `filter_command`; everything else is prompt text.

### Decisions

- **teloxide, not hand-rolled `getUpdates`.** Long polling, the offset, retries
  with backoff on 429 and the per-chat rate limit are `Dispatcher` plus the
  `throttle` adaptor. Writing that by hand is several hundred lines whose only
  job is to be a worse copy of a maintained crate. Pin the version in
  `Cargo.toml` and read it from `Cargo.lock` before coding — 0.17 speaks Bot API
  9.1, and a newer update kind arrives as `UpdateKind::Error`, logged and
  dropped rather than silently lost.
- **`allowed_chat_ids` is the only auth boundary.** A message from any other chat
  is dropped and logged at `info`. Matching a username is not identity.
- **No human at the keyboard: permissions default to `deny`.** As with
  `seavx exec`, writes and shell commands are refused unless the config lifts
  them. Inline-keyboard approval is deliberately out of this milestone; it turns
  the gateway into a remote shell and deserves its own spec.
- The token comes from `token_env` when set, else `token`. `/config` and
  `gateway status` never print it, matching how `api_key` is handled.
- One workspace per gateway process (its cwd), not per chat. Multi-root routing
  is a different feature.
- `[gateway.telegram]` not `[gateway]`: a second platform is a sibling table,
  and the config shape does not need to change for it.
- Failure policy: a network error retries inside teloxide and is surfaced in
  `gateway status`; a 401 stops the dispatcher and says the token is dead
  instead of spinning.

### Proof

- Update→message mapping, the 4096 split and the slash-command table are pure
  functions with unit tests (empty text, entities, photo without caption, edit,
  channel post, command addressed to another bot).
- The dispatcher loop is run against a local scripted HTTP server via
  `Bot::set_api_url`, with a placeholder token: two messages, one disallowed
  chat, one 429, one message-not-modified edit.
- `cargo check` with a placeholder token; no real token is ever needed to
  compile, and none is pasted into a test.
- Manual: `seavx -d gateway setup`, then `seavx -d gateway run` with a real bot,
  with `.seavx/` proving where the data landed.

---

## M4 — a self-update that can be trusted

Independent of M1–M3: it touches the CLI updater, the publish workflow, the two
installers and `docs/security.md`, and nothing that reads the config.

### End state

```
seavx update                 # the released build
seavx update --channel edge  # the newest master build
seavx update --force         # reinstall whatever the channel points at
```

```
$ seavx update
Checking for updates... (current version: v0.1.3)
Downloading v0.1.3 (06a4060)...
  signature  ok (key RWQ8F3A…)
  checksum   ok
Successfully updated SEAVX v0.1.3 (06a4060, /home/seaavey/.local/bin/seavx)
```

The pointer object grows one field, written by CI:

```json
{"sha":"06a4060","key":"builds/06a4060/seavx-linux-x86_64","version":"0.1.3",
 "built_at":"2026-10-31T16:41:34Z","sha256":"3e92be…","sig":"untrusted comment: …\nRWQ…"}
```

- `sig` is a minisign signature over the binary bytes, inline in the pointer so
  the signed object and the object that routes to it travel together.
- Verification runs in one order and stops at the first failure: signature,
  checksum, probe (`<temp binary> --version` exits 0), then replace.
- A missing or wrong signature refuses the update. A missing checksum stays a
  warning, so a pointer published before this milestone still installs.
- `--channel edge` reads `edge-<os>-<arch>.json`. The default `latest-*` comes
  from a tag push only.
- macOS joins the platforms that can self-update.
- `docs/security.md` gains an **Updates** section: what the signature proves,
  what it does not (a compromised private key, a machine already running
  attacker code), and that the updater is a trust boundary, not a sandbox.

### Decisions

- **minisign, not a hand-rolled Ed25519 scheme.** CI signs with the `minisign`
  CLI (`-S -W`, no passphrase, so the step is non-interactive) and the client
  verifies with the `minisign-verify` crate, which only verifies. The public key
  is a `const` in the updater; the secret key lives in a GitHub secret and is
  materialised as a file for the one signing step. Read both crate versions from
  `Cargo.lock` before coding. Rotation is a `const` array of accepted keys, not a
  single key: a build that trusts the old and the new key ships first, the next
  one drops the old. The alternative — `ed25519-dalek` plus a signature
  envelope of our own — means owning a canonical byte string, a key encoding and
  a rotation story to save one dependency.
- **The signature covers the binary, not the pointer.** The bytes that execute
  are the bytes that are verified; a forged pointer can relabel `version` or
  point at another key, and both outcomes are harmless: the first changes a
  printed string, the second yields a 404 or different signed content.
- **The signature fails closed, the checksum does not.** `verify_digest` returns
  `Ok(false)` for a pointer with no checksum because that field arrived later
  than the client. The signature has no such history and must not inherit the
  shape: a pointer without `sig` is refused with a message that says why.
- **No `.old` rollback on Unix.** The probe is the rollback: a binary that
  cannot print its own version never reaches the target path. Windows keeps its
  rename-aside dance because the OS refuses to overwrite a running image.
- **`semver` for ordering, and only when it parses.** Equality cannot answer
  "which is newer", which is why `0.1.3-rc.1` in `Cargo.toml` today already
  invites a silent downgrade to the published `0.1.3`. A pointer version that
  does not parse falls back to the equality check with a warning: a metadata
  glitch must not block every install.
- **`SEAVX_UPDATE_BASE` exists so the flow can be tested.** Default
  `https://cdn.seavx.com`; tests and manual checks point it at a local server.
  It is not a bypass — unsigned bytes are refused whatever the origin.
- **`install.sh` verifies what it already downloads past.** It parses `sha256`
  out of the pointer and fails on a mismatch, which is the same guarantee the
  updater gives. The signature is checked there too when `minisign` is on the
  machine, and skipped with a printed note when it is not: a curl-pipe installer
  cannot require a tool it has not installed yet.

### What the current code forces

- `Latest` (`src/cli/update.rs`) deserialises four fields; `sig` is additive and
  the client that ignores it keeps working, so the field can ship before the
  client that requires it.
- The download already streams into `.seavx-update-<pid>.tmp` beside the exe and
  hashes while writing. The signature and the probe read that same file, so
  there is no second temp path and no whole-file buffer in memory.
- `update.rs:48` refuses macOS while CI builds and publishes `macos-arm64`; the
  fix is one match arm. `std::env::consts::ARCH` says `aarch64`, the CDN path
  says `arm64`, and `install.sh` already spells that target `macos/arm64`.
- `replace_exe` already takes both paths as arguments, so "nothing was replaced"
  is assertable in a test without touching the running binary.
- CI writes the pointer with a `printf` and two `aws s3 cp`s; signing is one
  `minisign -S` step before the `printf`, and the edge channel is a second
  pointer object guarded by the same `github.ref` check that already exists.
- `worker/index.js` matches `/latest/<os>/<arch>` literally. The channel becomes
  one group in that regex and one filename template — the pointer already flows
  through untouched, so the `sig` field needs no worker change at all.
- Every master push currently republishes under the same unreleased version,
  which is why `same_commit` exists. Channels do not replace it; they decide
  which pointer a client reads.

### Proof

- Signature tests with a checked-in fixture key pair, marked as signing nothing:
  valid signature accepted, one flipped byte refused, signature from another key
  refused, pointer without `sig` refused naming the reason, malformed base64
  refused.
- Ordering tests: a failed signature leaves the target untouched and removes the
  temp file, using `replace_exe`'s explicit paths.
- Version tests: equal, newer, older, `0.1.3-rc.1` against `0.1.3`, unparseable
  pointer, and `--force` bypassing all of it.
- Probe test: a script that exits non-zero aborts before any rename.
- End-to-end against a local server through `SEAVX_UPDATE_BASE`: up to date,
  update applied, tampered bytes, unsigned pointer. This is the first test in
  the repo that speaks HTTP to a server it started itself; it is the reason the
  seam exists.
- Manual: tamper the object behind the pointer once, confirm the refusal, then
  `seavx -d update` against the real CDN.

### Rollout

Three states, in this order, because a client that requires `sig` must never be
the first thing to reach a pointer that lacks it:

1. CI signs, and writes `edge-*` alongside the `latest-*` it still produces from
   master. Old clients ignore the field.
2. The client verifies, probes, orders versions, accepts `--channel` and macOS.
3. The first tag flips `latest-*` to tag-only. Before that tag exists, the
   default channel keeps resolving to master because CI is still writing it.

---

## Not in any milestone

- Provider merging between workspace and global config (shadowing stays total).
- MCP servers over HTTP, MCP resources and prompts.
- Telegram inline approval, group chats, other platforms.
- Any config key nothing reads yet.
- Update mirrors, delta updates, background update checks at TUI start, an
  `seavx rollback` command.
- Signed pointer metadata with a threshold of keys or an offline key ceremony
  (TUF-shaped); one key that rotates by accepting two for one release is enough
  for a single-author project.
- Authenticode and notarisation of the published Windows and macOS binaries.
