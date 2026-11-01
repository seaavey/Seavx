# Sessions

A TUI conversation is stored in SQLite, so a session closed mid-conversation can
be resumed with its message order, tool calls and results intact. `seavx exec`
stays session-free and writes nothing.

- Location: `~/.seavx/sessions.db`, or `.seavx/sessions.db` in the workspace in
  debug mode. Debug mode is `--debug` or `SEAVX_DEBUG=1`; nothing else — a debug
  build is not a debug run, so `cargo run` keeps its data in `~/.seavx` like an
  installed binary. Every TUI run records the state it resolved as
  `SEAVX_DEBUG=true|false` in the workspace `.env`, rewriting that line and
  leaving the rest of the file alone. The file is `0600` and its directory
  `0700`; a looser mode left by an older install is tightened on start.
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
seavx sessions show svx-2f124b69 --events  # transcript plus the stored tool/turn trace
seavx sessions export svx-2f124b69 > session.json
seavx sessions resume svx-2f124b69 # open the TUI with that session
seavx resume                 # list them and pick a number
seavx resume svx-2f124b69    # same as sessions resume
```

Skills have a CLI surface too; the interactive picker stays in the TUI (`/skills install <url>`):

```bash
seavx skills                          # every skill your roots can load, with the owning root
seavx skills install https://github.com/anthropics/skills   # what that repository holds
seavx skills install https://github.com/anthropics/skills pdf docx   # install two of them
seavx skills add ../my-skill          # copy a local directory that has SKILL.md
seavx skills remove pdf --yes         # immediate, refuses a symlinked copy
```

Retention: sessions are not pruned (roughly 1 KB per turn). To delete, stop
Seavx and remove the database: `rm ~/.seavx/sessions.db*`. To back up, copy
`sessions.db` with its `-wal` and `-shm` files while Seavx is closed, or run
`PRAGMA wal_checkpoint(TRUNCATE)` first. `/clear` starts a new session; the old
one stays listed and resumable.
