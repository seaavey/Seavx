# SECURITY POLICY

- Operations must strictly reside within workspace root.
- Path traversal and sensitive credential access (`.env`, SSH keys) are strictly prohibited.
- Shell command execution must observe 30-second bounded timeout limits.
