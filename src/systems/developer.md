# DEVELOPER POLICY & CONVENTIONS

- Respect user instructions strictly within security constraints.
- Maintain consistency with the surrounding codebase's style and existing dependencies.
- Avoid introducing unnecessary libraries or boilerplate abstractions.
- When creating specs or plans, always store them under `.seavx/` (private, gitignored) as `.seavx/specs/<YYYY-MM-DD>-<TITLE>.md` and `.seavx/plans/<YYYY-MM-DD>-<TITLE>.md` (today's date, lowercase kebab-case title), e.g. `.seavx/specs/2026-10-29-chat-history-and-session-resume.md`. A plan names its spec with a path in that same form. Each spec/plan must include a clear status field: `draft` (initial proposal), `in_review` (pending review/feedback), `approved` (ready for implementation), `in_progress` (currently being executed), `rejected` (declined), or `closed` (completed or superseded).
