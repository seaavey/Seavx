# TOOL GUIDANCE

- `read_file`: Use to inspect files before editing.
- `edit_file`: Preferred for surgical updates via exact string replacement.
- `write_file`: Use when creating new files or replacing whole contents.
- `list_directory`: Discover directory structure.
- `run_command`: Run tests, git status, and cargo checks.
- `search_files`: Fast content regex and filename discovery across repo.
- `save_memory`: Persist durable lessons, conventions, or user corrections for future sessions.
- `skill_view`: Load a skill's full SKILL.md on demand (metadata index is already in context).
- `skill_write`: Create a new skill or fully rewrite an existing one.
- `skill_patch`: Surgical fix inside an existing skill via exact string replacement.
- `ask_user`: Ask one structured question (single choice, multi choice, or free text) when a decision cannot be inferred from the workspace. The turn waits for the answer; every non-answer says plainly that no value may be assumed. Headless `exec` never waits.

## Editing contracts

- `edit_file` replaces a snippet that occurs exactly once. If it occurs more than once the call fails with the count and up to three sample lines; add context or pass `replace_all: true`. An empty `old_string` is refused, and `old_string == new_string` is a no-op.
- `apply_patch` takes a unified diff (`--- a/<path>`, `+++ b/<path>`, `@@ -start,count +start,count @@`). Every hunk must match exactly at the announced line number; a mismatch, a reversed or overlapping hunk, or a `..`/absolute header refuses the whole patch and writes nothing. Creation uses `--- /dev/null` with `-0,0`; deletion is not supported. On success it reports `+N -M` per file.
- Every write is checkpointed before it happens, so `/undo` restores the previous bytes (and removes files the tool created). A batch is one unit: it lands fully, is restored to its pre-batch bytes, or reports a checkpoint path to restore by hand.
