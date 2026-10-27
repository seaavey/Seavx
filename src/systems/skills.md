# ANTHROPIC SKILL-CREATOR RULES & CONVENTIONS

Source Reference: https://github.com/anthropics/skills/tree/main/skills/skill-creator

These rules govern how skills must be authored, validated, and structured in Seavx.

## 1. Anatomy of a Skill Directory
```text
.seavx/skills/<skill-name>/
├── SKILL.md          (Required)
│   ├── YAML frontmatter (name, description, confidence)
│   └── Markdown body (< 500 lines)
├── scripts/          (Optional: Executable bash/python scripts)
└── references/       (Optional: Detailed docs loaded on-demand)
```

## 2. Three-Level Progressive Disclosure
1. **Metadata Tier (Always in context)**:
   - Frontmatter `name` and `description` only.
   - `description` must contain both **WHAT** it does and **EXACT TRIGGERS** for when to load it.
2. **SKILL.md Body Tier (Loaded on trigger)**:
   - High-level workflow, prerequisites, step-by-step instructions.
   - Ideal length: < 500 lines.
3. **Bundled Resources Tier (Loaded on-demand)**:
   - Large API references or scripts under `references/` or `scripts/`.

## 3. Frontmatter Specification
Every `SKILL.md` must start with exact YAML frontmatter:
```yaml
---
name: kebab-case-skill-name
description: "Use when <trigger conditions>. <One-sentence summary of workflow>."
confidence: 85%
---
```

## 4. Writing & Hygiene Patterns
- Use imperative, concise instructions ("Run cargo test", not "You should consider running cargo test").
- No generic AI commentary or filler ("antislop" standards).
- State clear inputs, step-by-step commands, expected outputs, and known pitfalls/failure recovery.
- Do not include credentials, secrets, or environment-specific hardcoded paths.
