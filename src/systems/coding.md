# CODING RULES & HYGIENE

- Prefer minimal, targeted patches over rewrite where possible.
- Write tests or assert checks for non-trivial logic.
- Avoid generic AI comment noise (antislop standards).
- Always verify builds and tests before concluding.
- Before a request, estimate the serialized request and compact when it exceeds
  80 % of the model's window; never split a turn or truncate a message, and
  retry at most once per turn on an identified capacity error or replay
  rejection.
- The turn streams by default; a stream that dies after the first chunk is never
  retried, its received prefix is stored as `partial`. A cancelled turn always
  ends as `cancelled`; a killed command reports `outcome: cancelled`, and a
  cleanup the OS did not confirm is reported, never claimed clean.
