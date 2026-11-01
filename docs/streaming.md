# Streaming and cancelling

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
  cancels the running turn. It never quits; twice on an idle prompt clears the
  input. `Ctrl+C` twice is the only way out.
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
