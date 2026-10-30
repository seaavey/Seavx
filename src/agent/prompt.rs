use std::path::Path;

pub const FALLBACK_SYSTEM_PROMPT: &str = include_str!("../systems/systems.md");
pub const SOUL_PROMPT: &str = include_str!("../systems/SOUL.md");
pub const DEVELOPER_POLICY: &str = include_str!("../systems/developer.md");
pub const CODING_POLICY: &str = include_str!("../systems/coding.md");
pub const SECURITY_POLICY: &str = include_str!("../systems/security.md");
pub const TOOLS_POLICY: &str = include_str!("../systems/tools.md");
pub const SKILLS_POLICY: &str = include_str!("../systems/skills.md");

pub const PLAN_POLICY: &str = r#"# PLAN MODE ACTIVE

You are in read-only planning mode. File modification tools and command execution are unavailable.
Use only read-only tools to inspect the codebase. Do not claim that files were changed.
Do not ask to switch modes; the user controls the mode.

Produce a final plan with:

1. Context & Architecture — files involved and current behavior
2. Proposed Changes — exact file targets and functions to modify or add
3. Step-by-Step Implementation — ordered, independently verifiable steps
4. Verification — exact tests, lints, and manual checks
5. Risks & Trade-offs"#;

pub const ASK_POLICY: &str = r#"# ASK MODE ACTIVE

You are answering questions about this repository in read-only mode. File
modification tools and command execution are unavailable.

Split what you report into three parts:

1. Verified - something you read in this turn. Cite it as `relative/path:line`
   or `relative/path:start-end`, using a line you actually saw.
2. Inference - a conclusion drawn from those facts. Name what it rests on.
3. Unknown - what you could not confirm. Say so instead of guessing.

Never invent, widen or "correct" a line number, and never cite a file you did not
read in this turn. A question that needs no repository evidence is answered
directly, without citations. Do not claim files were changed. Do not ask to
switch modes; the user controls the mode."#;

pub const REVIEW_POLICY: &str = r#"# REVIEW MODE ACTIVE

You are reviewing the change set you were given. File modification tools and
command execution are unavailable.

Report only what the diff and the files you read support. Answer with exactly
one fenced ```json block and nothing else, in this shape:

{
  "target": "<the target you were given>",
  "findings": [
    {
      "severity": "critical|high|medium|low|info",
      "title": "one line naming the defect",
      "impact": "what breaks, and for whom",
      "remediation": "the direction of the fix, not a rewritten file",
      "path": "relative/path",
      "line_start": 12,
      "line_end": 14,
      "confidence": "confirmed|hypothesis",
      "outside_diff": false
    }
  ],
  "limitations": ["what this review did not cover"]
}

`line_start`/`line_end` and `outside_diff` are optional; every other field is
required and must not be empty. An answer without findings uses an empty
`findings` list. Prose outside the block is discarded.

Prefer the changed lines. Say explicitly when a finding sits outside the diff.
Never invent a line number. Never claim a test passed or failed: no command is
run here, so tests are unrun unless the request says otherwise. If nothing is
wrong, say no findings for the named target and name the limits of the review
(context you did not read, paths withheld from you)."#;

pub fn load_system_prompt(workspace_root: &Path) -> String {
    let workspace_systems = workspace_root.join("src/systems/systems.md");
    let base_prompt = if workspace_systems.exists() {
        std::fs::read_to_string(workspace_systems)
            .unwrap_or_else(|_| FALLBACK_SYSTEM_PROMPT.to_string())
    } else {
        FALLBACK_SYSTEM_PROMPT.to_string()
    };

    // Check for optional custom local SOUL.md in .seavx/SOUL.md or SOUL.md
    let soul_candidates = [
        workspace_root.join(".seavx/SOUL.md"),
        workspace_root.join("SOUL.md"),
        workspace_root.join("src/systems/SOUL.md"),
    ];
    let custom_soul = soul_candidates
        .into_iter()
        .find(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| SOUL_PROMPT.to_string());

    let mut full_prompt = format!(
        "{custom_soul}\n\n{base_prompt}\n\n{DEVELOPER_POLICY}\n\n{CODING_POLICY}\n\n{SECURITY_POLICY}\n\n{TOOLS_POLICY}\n\n{SKILLS_POLICY}"
    );

    // Auto-inject project MEMORY.md if available
    let memory_candidates = [
        workspace_root.join(".seavx/MEMORY.md"),
        workspace_root.join("MEMORY.md"),
    ];

    for memory_file in &memory_candidates {
        if memory_file.exists()
            && let Ok(memory_content) = std::fs::read_to_string(memory_file)
        {
            full_prompt.push_str(&format!(
                "\n\n# PROJECT MEMORY ({})\n{memory_content}",
                memory_file.display()
            ));
            break;
        }
    }

    // Auto-inject global or local USER.md if available
    let user_candidates = [
        workspace_root.join(".seavx/USER.md"),
        workspace_root.join("USER.md"),
    ];
    let mut user_path = user_candidates.into_iter().find(|p| p.exists());
    if user_path.is_none()
        && let Some(home) = crate::home::dir()
    {
        let global_user = home.join(".seavx/USER.md");
        if global_user.exists() {
            user_path = Some(global_user);
        }
    }

    if let Some(user_file) = user_path
        && let Ok(user_content) = std::fs::read_to_string(user_file)
    {
        full_prompt.push_str(&format!("\n\n# USER PREFERENCES (USER.md)\n{user_content}"));
    }

    // Auto-inject lightweight skills index: one line per skill, first sentence only
    let skill_mgr = crate::agent::skills::SkillManager::new(workspace_root);
    let skills = skill_mgr.list_skills();
    if !skills.is_empty() {
        full_prompt.push_str("\n\n# AVAILABLE SKILLS (Loaded on demand)\n");
        for sk in skills {
            let summary: String = sk
                .description
                .split(". ")
                .next()
                .unwrap_or_default()
                .chars()
                .take(100)
                .collect();
            full_prompt.push_str(&format!("- {}: {}\n", sk.name, summary));
        }
    }

    full_prompt
}
