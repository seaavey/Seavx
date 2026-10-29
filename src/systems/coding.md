# CODING RULES & HYGIENE

- Prefer minimal, targeted patches over rewrite where possible.
- Write tests or assert checks for non-trivial logic.
- Avoid generic AI comment noise (antislop standards).
- Always verify builds and tests before concluding.
- Before a request, estimate the serialized request and compact when it exceeds
  80 % of the model's window; never split a turn or truncate a message, and
  retry at most once per turn on an identified capacity error or replay
  rejection.
