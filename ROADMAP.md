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

### Current state

There is no MCP configuration, client, transport, or registry in the codebase.
`AgentTool` in `src/agent/tools.rs` currently requires static tool names and
descriptions, and `ToolRegistry` owns the built-in tools. The permission matrix
already has an `execute` action and agent events support user-facing `Notice`s.
M2 follows M1 because it reads the config; it does not start until the provider
config migration is in place.

### End state

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/project"]
env = { LOG_LEVEL = "warn" }
```

- Transport: stdio only in this milestone. Launch `command` with `args` directly,
  without a shell. Speak line-delimited JSON-RPC on stdin/stdout; reserve stderr
  for diagnostics. HTTP/SSE is out of scope.
- Starting a server is a separate trust decision. Before spawning each configured
  command, require a dedicated one-shot yes/no confirmation for this runtime,
  showing the server name and redacted command details. It has no remember or
  category action. Config values alone do not grant launch approval, even when
  they come from a workspace config. Do not persist this approval or let
  `permitted_actions`, `tool_permissions = "allow"`, or Auto mode bypass it.
  `tool_permissions = "deny"` and `seavx exec` skip startup without launching a
  process. A denial emits one `Notice`. Apply the regular permission gate to each
  later tool call.
- Start each approved server once per agent runtime. Negotiate a supported MCP
  protocol version in `initialize`, send `notifications/initialized`, then
  request `tools/list` and follow its cursor until complete. Register tools only
  after a successful handshake and schema check. Bound startup, frame, and list
  sizes. A server that fails to start, handshake, or list emits one `Notice` and
  is skipped; one broken server never blocks a turn. Drain stderr separately
  with bounded, redacted diagnostics.
- Each tool becomes an `AgentTool` named `mcp__<server>__<tool>`. Both
  `AgentTool::name` and `AgentTool::description` must return runtime `&str`
  values rather than require `&'static str`. Reject invalid names and collisions
  with built-in or already registered tools; never replace an existing tool.
- Pass each valid MCP `inputSchema` through to the genai tool definition without
  converting it through `schemars`. Require a JSON object schema; skip and report
  malformed entries. The built-in tools keep their `schemars` path.
- Calls use `tools/call`, a fresh JSON-RPC request id, the active turn's cancel
  token, and a bounded per-call timeout. On cancellation, timeout, or runtime
  drop, stop the server process and its descendants. Mark the server unavailable
  and remove its tools from subsequent model requests; do not restart it during
  that runtime. A new runtime must pass the startup gate again. Return text
  content to the model, reject unsupported non-text result blocks explicitly,
  and map MCP's `isError` result to a tool failure. A server error is not a
  runtime crash.
- `doctor` never starts configured commands. It reports configured server names,
  stdio transport, and `not probed`; it does not report tool counts because
  obtaining them requires launching each server. No dedicated `seavx mcp`
  command.

### Decisions

- An MCP server runs a user-configured executable with the host user's
  privileges and process environment; `env` overlays configured values. Stdio
  avoids opening a listener, but does not sandbox the process or limit its
  filesystem, network, or environment access. Startup approval is required even
  when the config is workspace-local.
- Server startup always needs an explicit one-shot confirmation in an interactive
  run. The config declares what to launch, not permission to launch it. Startup
  approval is per server and runtime, is never stored in `permitted_actions` or
  `tool_permissions`, and is unavailable in `seavx exec`. This prevents a
  workspace config from running a command silently.
- MCP tool calls are `execute`-kind and use the existing permission matrix. In
  the default `ask` mode, Seavx asks before each call. The existing
  `permitted_actions = ["execute"]` lifts that category; `tool_permissions = "allow"`
  and Auto mode follow their existing behavior, while `tool_permissions = "deny"`
  refuses calls in every mode. Startup approval does not grant tool-call
  permission.
- No per-server tool allowlist in v1; the permission matrix is the allowlist.
  Add a narrower filter only if real tool namespaces crowd the prompt.
- Resources, prompts, roots, sampling, server-initiated requests, and dynamic
  tool-list refresh are out of scope. Advertise only client capabilities this
  milestone implements.

### Proof

- A fixture MCP server in `src/tests/` speaks JSON-RPC over stdio and covers the
  initialize/initialized/list sequence, call, tool error, malformed schema,
  malformed JSON, oversized frame/list, early exit, and a server that never
  answers. Verify stderr cannot corrupt protocol output and captured diagnostics
  are bounded and redacted.
- Verify startup and call timeouts, cancellation, process-tree cleanup, one
  failed server alongside one healthy server, and removal of dead server tools
  from subsequent model requests without an automatic restart.
- Verify startup confirmation is one-shot and not persisted,
  `tool_permissions = "deny"` skips launch, `tool_permissions = "allow"`,
  permitted actions, and Auto mode cannot bypass confirmation, and `seavx exec`
  never launches a configured server. Verify tool calls still follow `execute`
  permissions, including the Auto-mode behavior in the existing matrix.
- Verify collisions between two servers and with a built-in tool; neither may
  replace an existing registration. Verify both dynamic names and descriptions
  reach the provider tool definition.
- Verify a call is denied in `ask`/`deny` as configured and that explicit
  `permitted_actions = ["execute"]` lifts only the execute category.
- `cargo clippy` must be clean. Do not unwrap or trust unvalidated server output.

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

Independent of M1–M3 because it does not read the config. It touches the CLI
arguments and dispatch in `src/main.rs`, the updater and its tests, the verifier
dependency and lockfile, the publish workflow, CDN worker, both installers,
`docs/security.md`, README, CHANGELOG, and the matching Seavx skill. Those docs
ship with the implementation milestone.

### Current state

`src/cli/update.rs` has only `--force`, reads a fixed
`latest/<os>/<arch>.json` pointer, and supports Linux x86_64 and Windows x86_64.
It verifies `sha256` when present and warns when it is absent; it does not verify
a signature. CI currently publishes `latest-*` pointers on both master and tag
pushes. `worker/index.js` has no channel route. `install.sh` downloads the raw
latest binary, and `install.ps1` writes the raw download directly to `seavx.exe`;
neither verifies a checksum or signature. This milestone replaces that
baseline. Do not describe the current updater or installers as authenticated.

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

The pointer object carries the fields the client must authenticate:

```json
{
  "channel": "stable",
  "target": "linux-x86_64",
  "sha": "06a4060",
  "key": "builds/06a4060/3e92be…/seavx-linux-x86_64",
  "version": "0.1.3",
  "built_at": "2026-10-31T16:41:34Z",
  "sha256": "3e92be…",
  "sig": "<minisign signature>"
}
```

- CI signs a canonical manifest containing `channel`, `target`, `sha`, `key`,
  `version`, `built_at`, and `sha256`, in that fixed order. The signed UTF-8
  bytes begin with `SEAVX-UPDATE-v1` followed by LF, then one `name=value` line
  per field in that order, then a final LF. `sig` is not part of the signed
  bytes. Values are validated as `stable|edge`, one of
  `linux-x86_64|windows-x86_64|macos-arm64`, a seven-character lowercase hex
  commit, `builds/<sha>/<sha256>/seavx-<target>` (with `.exe` on Windows), SemVer,
  a UTC RFC 3339 timestamp, and a 64-character lowercase SHA-256 digest. JSON
  key order and escaping therefore do not affect verification.
- Before using `key`, the client requires `channel` to equal the requested
  channel and `target` to equal the running binary's detected platform. A valid
  signature for another channel or target is still refused.
- The manifest signature authenticates the artifact route and its SHA-256. The
  client verifies the signature before downloading, hashes the streamed bytes,
  probes the temporary binary with `--version`, checks that version against the
  signed version, then replaces the executable. Any failed step stops the update
  and removes the temporary file. Network requests and the version probe have
  bounded timeouts.
- A new pointer must include a valid signature and SHA-256. Missing or invalid
  values fail closed. `--force` bypasses only version/up-to-date checks; it never
  bypasses signature, checksum, target, or probe validation.
- The default channel is stable and uses `latest-<os>-<arch>.json`; tag builds
  publish it. `--channel edge` uses `edge-<os>-<arch>.json`; master builds publish
  it. Before the first stable tag, `latest-*` continues to point at master so
  existing installs keep working.
- Build artifacts use digest-addressed keys. CI may reuse an existing key only
  when its bytes match the digest; it must fail rather than overwrite a mismatch.
  The CDN can then cache those paths as immutable for a year.
- Self-update supports Linux x86_64, Windows x86_64, and macOS arm64. CI's target
  name is `aarch64`; the pointer and installer use `arm64`.
- Both installers download into a temporary file, validate the pointer and
  artifact before replacing the installed binary, and remove the temporary file
  on failure. A bad or incomplete download must leave an existing installation
  untouched.
- `docs/security.md` gains an **Updates** section: the client accepts the update
  only when the trusted signing key approves the manifest and the downloaded
  bytes match its digest. This does not prove the binary is harmless, prevent a
  compromised signing key or CI, protect an already compromised machine, or
  prevent replay of an older valid signed pointer. Version ordering rejects a
  downgrade on either channel unless `--force` is used. A replay of an older
  signed pointer with the same version is not reliably detectable; same-version
  edge freshness is not guaranteed.

### Decisions

- **minisign, not a hand-rolled Ed25519 scheme.** CI signs with the `minisign`
  CLI (`-S -W`, no passphrase, so the step is non-interactive) and the client
  verifies with the `minisign-verify` crate, which only verifies. The public-key
  set is pinned in the updater and both installers; never accept a key from the
  pointer or CDN. The secret key lives in a GitHub secret and is materialised as
  a file for the one signing step. Read both crate versions from `Cargo.lock`
  before coding. Rotate the updater and both installers together: ship a build
  accepting old and new keys first, then drop the old key in a later build.
- **Sign the manifest, not only the binary.** Signing only binary bytes would let
  a forged pointer relabel the version or route the client to a different
  legitimately signed build. Signing the channel, target, commit, artifact key,
  version, timestamp, and digest binds those claims to the bytes the client
  downloads. This does not prevent replay of a previously valid manifest.
- **The signature and checksum both fail closed.** The signed manifest contains a
  required SHA-256. The updater refuses a pointer with no signature or checksum,
  including an old unsigned pointer. CI publishes signed pointers before a client
  that requires them ships, so legacy pointer compatibility is not needed after
  rollout.
- **`--force` never skips verification.** It can reinstall the signed artifact
  selected by the channel even when the version/commit looks current, but the
  signature, digest, target, and probe remain mandatory.
- **The probe is preflight validation, not rollback.** A candidate that cannot
  print the expected version never reaches the target path. Unix has no automatic
  recovery if a replacement later proves unusable; the user reinstalls or
  restores the prior binary manually. Windows keeps the rename-aside dance to
  restore the old file when replacement fails, not as an automatic post-update
  rollback.
- **Use `semver` for version ordering.** Refuse an invalid signed version rather
  than treating it as equal. Skip a downgrade on either channel unless `--force`
  is set. For the same version, compare commit ids so edge builds can update
  while a matching build is reported up to date.
- **`SEAVX_UPDATE_BASE` exists so the flow can be tested.** Default
  `https://cdn.seavx.com`; tests and manual checks point it at a local server.
  It changes the origin, not the verification rules.
- **Artifact keys are digest-addressed.** Store each build at
  `builds/<sha>/<sha256>/<binary-name>` so reruns cannot replace a different
  binary behind a cached key. If an object already exists, CI verifies its bytes
  against the path digest and fails on a mismatch; the worker's year-long cache
  remains valid for those immutable paths.
- **Installers verify the digest and pin the signer.** Both installers fetch and
  validate the pointer, require the platform and stable channel to match its
  signed fields, and verify SHA-256. If `minisign` is installed, they verify with
  the same pinned public-key set as the updater. Otherwise, they warn that the
  checksum only detects transfer corruption, not a forged pointer or publisher;
  a compromised CDN could replace both pointer and artifact. The updater itself
  always requires a signature.

### What the current code forces

- `src/main.rs` owns the `Update` arguments and dispatch. Add `--channel edge`
  there and cover parsing/dispatch in CLI tests; `src/cli/update.rs` alone cannot
  expose the flag.
- `Latest` (`src/cli/update.rs`) currently reads `sha`, `key`, `version`, and
  optional `sha256`. It has no `channel`, `target`, or `sig`. Add the signed
  manifest fields and validate the signature, requested channel, and detected
  target before using `key` or `version`.
- The updater already streams the artifact into `.seavx-update-<pid>.tmp` beside
  the executable and hashes it while writing. Reuse that path and digest; verify
  the manifest before download and remove the temp file on every failure path.
- The current platform match accepts Linux x86_64 and Windows x86_64. CI already
  builds macOS arm64 as `aarch64-apple-darwin`; `install.sh` uses `macos/arm64`.
  Add that target to the updater and bind every target to its expected artifact
  name.
- `replace_exe` already takes explicit temp and executable paths. Keep that seam
  for tests that prove a failed verification leaves the target untouched.
- CI currently writes `latest-*` pointers on both master and tag pushes. It has
  no signing step or edge pointer. Add signing and channel-specific publication
  without breaking the old `latest-*` pointer during rollout.
- CI currently uploads artifacts to `builds/$SHORT_SHA/<binary>` with ordinary
  `aws s3 cp`, so a rerun can overwrite a key. Move to
  `builds/$SHORT_SHA/$SHA256/<binary>` and reject an existing object whose bytes
  do not match the digest. The worker already caches build paths for a year as
  immutable, so keep that policy only for the digest-addressed artifacts.
- `worker/index.js` currently routes only `/latest/<os>/<arch>` and its `.json`
  form. Add the edge route and map each channel to its pointer object; continue
  serving immutable build artifacts through their existing keys.
- Both installers currently download a binary directly. Change them to fetch and
  validate the pointer, check the artifact digest, and verify the manifest
  signature with the same pinned public keys when minisign is available. Keep
  the explicit checksum-only warning when it is not.
- Every master push currently republishes `latest-*` under the same unreleased
  version. Keep that behavior only until the signed-client rollout reaches its
  first tag; after that, master writes `edge-*` and tags write stable `latest-*`.

### Proof

- CLI tests cover default stable behavior, `--channel edge`, invalid channel
  values, and dispatch to the expected pointer path.
- Signature tests use a checked-in fixture key pair that signs no release:
  valid manifest accepted, changed channel/target/version/key/digest refused,
  validly signed metadata for the wrong requested channel or platform refused
  before artifact download, one flipped artifact byte refused, signature from
  another key refused, missing signature refused with its reason, and malformed
  signature refused. The verifier key never comes from the pointer.
- Installer checks prove both scripts stage before replacement, a bad pointer or
  artifact leaves an existing binary untouched, temporary files are cleaned up,
  the same pinned public-key set is used by both scripts, a checksum mismatch
  fails, a minisign verification failure fails, and missing minisign emits the
  documented checksum-only warning.
- Ordering tests prove an invalid signature or checksum leaves the target
  untouched and removes the temporary file. Also cover cleanup after a failed
  transfer, failed sync, failed probe, and failed replacement.
- Version tests cover equal, newer, older, `0.1.3-rc.1` against `0.1.3`, invalid
  SemVer, same-version commit changes, and `--force` bypassing only version
  checks. A forced update with a bad signature must still fail.
- Probe tests cover non-zero exit, timeout, and a binary whose reported version
  differs from the signed manifest.
- End-to-end against a local server through `SEAVX_UPDATE_BASE`: current build,
  stable and edge updates, signed update, tampered artifact, modified pointer,
  wrong channel, wrong target, unsigned pointer, and forced reinstall. No test
  needs a real signing key or CDN write.
- CI publishes artifacts under commit-and-digest keys. A rerun may reuse an
  existing object only after its digest matches; a mismatch must fail. Verify
  the worker routes both channels to their intended pointer filenames.
- Manual: run `seavx -d update` against the real CDN and confirm the published
  channel and signature. Perform tamper tests only against the local test server.

### Rollout

Three states, in this order, because a client that requires a signature must not
be the first thing to reach an unsigned pointer:

1. CI publishes signed `edge-*` pointers for master and signed `latest-*` pointers
   for the current master build. Old clients ignore the additive fields.
2. Ship the client that verifies signed metadata, probes the candidate, orders
   versions, accepts `--channel edge`, and supports macOS arm64. Keep writing
   `latest-*` from master until a stable tag exists.
3. The first tag publishes the stable `latest-*` pointer. Later master pushes
   update only `edge-*`; the default channel then remains on the latest tag.

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
