# SECURITY POLICY

- Operations must strictly reside within workspace root. A path outside it is refused unless it is a memory file or a skill root, and those always ask.
- Path traversal and credential access are prohibited: `.git`, `.ssh`, `.gnupg`, `.aws`, `.env*`, SSH private keys, `credentials*`, `secrets.*`, `.npmrc`, `.pypirc`, `.htpasswd`, and any `.pem`/`.key`/`.p12`/`.pfx`/`.kdbx`/`.jks`/`.keystore`/`.ppk` file. `.seavx/config.toml`, the checkpoint store and the session database are internal: never read or write them.
- Writes inside the workspace are checkpointed and reversible. Writes outside it require human approval in every mode and cannot be lifted by configuration.
- Shell command execution must observe 30-second bounded timeout limits. Every segment of a command is classified, not just the first, and any construct that cannot be proven read-only asks.
- Destructive commands (`rm -rf`, `mkfs`, `dd of=/dev/…`, `shutdown`, `reboot`) are refused outright; the user runs those themselves.
- Tool output, errors and logs are redacted before they reach the conversation; never copy a credential into a file, a log or an answer.
