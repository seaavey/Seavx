# Context and compaction

A request is measured before it is sent, and compacted when it would not fit.
The measurement is a **serialized-request estimate**: `ceil(chars/4)` over the
JSON of the whole request (system prompt, history, tool calls, tool results,
tool schemas, the task). It is a heuristic, not a promise — it can under-count
CJK or code-dense text by up to ~2x — so overflow is handled rather than
declared impossible.

| Source                                             | Window    | When                                              |
| -------------------------------------------------- | --------- | ------------------------------------------------- |
| the [models.dev](https://models.dev) catalog       | published | the model is listed there — the common case       |
| `claude`, `gemini`, `nemotron`                     | 1 000 000 | the catalog has no entry for this id              |
| `deepseek`, `glm`, `gpt-4o`, `gpt-4.1`, `o3`, `o4` | 128 000   | the catalog has no entry for this id              |
| anything else                                      | 64 000    | assumed — the footer and `doctor` say `(assumed)` |

- The catalog is `https://models.dev/api.json` reduced to one `limit.context`
  per model name, cached seven days in `.seavx/cache/contexts.json`. A routed
  id (`rv/deepseek-ai/DeepSeek-V4.1-Flash-1017`) is matched on its tail with
  every separator dropped, so the name a gateway lists does not have to be the
  name the catalog uses: `v4.1`, `v4-1` and `V4_1` are one model. A stale
  cache refreshes in the background at TUI start, and `seavx setup` fetches it
  once at the end; `seavx exec` and `doctor` never fetch, they read what is
  cached.
- The same cache carries the effort levels a model publishes
  (`reasoning_options` with `type = effort`), so the effort pickers offer what
  the model takes. An id the catalog misses, or one it lists with no effort
  option, keeps the static list `none`, `minimal`, `medium`, `high`, `highx`,
  `max`. It is a display list only: a saved `reasoning_effort` is never refused
  for being absent from it. The menu speaks the names the config stores: a
  published `low` is offered as `minimal` and `xhigh` as `highx`, so the row
  the picker opens on is the effort the turn runs.
- `request_budget` is 80 % of the window, `keep_recent` 60 % of that budget.
- Above the budget the older turns are summarised through one chunking rule,
  `CHUNK_CHARS = 24 000` characters. The summary enters the next request as a
  single user message with the `# SESSION CONTEXT (compacted)` banner; a turn is
  never split and a message is never truncated.
- The summary must be at least 100 characters and carry goals, decisions, files
  and pending sections. A summary that fails the gate, or a summariser error,
  drops the older turns and prints `System: compaction failed (<reason>); older
turns dropped` — a bad summary is worse than none.
- Compaction needs something to fold: fewer than `MIN_COMPACT_MESSAGES = 4`
  messages older than the recent window and the request is sent as it is
  (`auto compact skipped …`). A one-line summary of one row costs more than the
  row it replaces.
- After compaction the request is re-measured; a summary that still does not fit
  shrinks `keep_recent` by 10 % (at most three times), then every kept turn
  except the newest is dropped, with a warning. A single turn bigger than the
  budget is sent whole, and the warning names the estimate and the model.
- A request that grows inside one turn is measured before every send, not only
  at the turn's first one: a tool loop that pushes the request past the budget
  compacts again and drops older turns when the summary fails the gate. A
  summary that still leaves the request over the budget is not kept either — the
  older turns go, so the next tool step does not pay for another summary of the
  same rows. The system row and the newest turn stay whole.
- Every automatic compaction says so in the transcript, with the divider
  `----------------------------- AUTO COMPACT --------------------------------`
  and the `before → after` estimate under it (`(estimated), mid-turn` for the
  one inside a tool loop).
- `auto_compact = false` (default `true`) turns the whole thing off:
  `/config set auto_compact false` stores it, nothing is summarised and no
  turn is dropped — the over-budget request is sent as it is.
- One retry per turn, never two: a provider rejection of the replayed tool calls
  rebuilds the window at half the turns; a provider _capacity_ error (a message
  naming context length, token limit or prompt-too-long) re-compacts against 40 %
  of the budget. Either is retried once; after that the turn fails with the
  provider's message. When there is nothing older to fold, or
  `auto_compact = false`, the turn fails with that message instead of spending
  the retry on the same request. A turn that already produced output is never
  retried.
- `max_turns` is a config key and new configs default to 100. An existing file
  keeps the value it stores. Repeating the same tool call with the same
  arguments, or the same error twice in a row, ends the loop with `stopped: …`
  instead of burning the budget.
