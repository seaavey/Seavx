use std::path::{Path, PathBuf};

pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub struct SkillManager {
    /// Highest priority first: project (debug), installed, then shared agent skills.
    roots: Vec<PathBuf>,
}

impl SkillManager {
    pub fn new(workspace_root: &Path) -> Self {
        let mut roots = vec![workspace_root.join(".seavx/skills")];
        if let Some(home) = crate::home::dir() {
            roots.push(home.join(".seavx/skills"));
            roots.push(home.join(".agents/skills"));
        }
        Self { roots }
    }

    /// List all available skills; earlier roots shadow later ones with the same name
    pub fn list_skills(&self) -> Vec<SkillMetadata> {
        let mut list = Vec::new();
        for root in &self.roots {
            scan_dir(root, &mut list, 0);
        }
        list
    }

    /// Automatically save a synthesised skill into the project skills dir
    pub fn save_skill(&self, name: &str, content: &str) -> Result<PathBuf, String> {
        let sanitized_name: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();

        if sanitized_name.is_empty() {
            return Err("Invalid skill name".to_string());
        }

        let skill_dir = self.roots[0].join(&sanitized_name);
        std::fs::create_dir_all(&skill_dir)
            .map_err(|e| format!("Failed to create skill directory: {e}"))?;

        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, content)
            .map_err(|e| format!("Failed to write SKILL.md: {e}"))?;

        Ok(skill_file)
    }
}

fn scan_dir(base: &Path, list: &mut Vec<SkillMetadata>, depth: usize) {
    // Shared skill dirs group entries by category one level deeper (<category>/<name>/SKILL.md).
    if depth > 2 {
        return;
    }

    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Skip hidden dirs, build artifacts and dependencies like every other walk.
        if crate::util::is_skipped(
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        ) {
            continue;
        }

        let skill_file = path.join("SKILL.md");
        if skill_file.exists()
            && let Ok(content) = std::fs::read_to_string(&skill_file)
        {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if list.iter().any(|s| s.name == name) {
                continue;
            }
            let description =
                extract_description(&content).unwrap_or_else(|| format!("Skill for {name}"));
            list.push(SkillMetadata {
                name,
                description,
                path: skill_file,
            });
        } else {
            scan_dir(&path, list, depth + 1);
        }
    }
}

fn extract_description(content: &str) -> Option<String> {
    crate::util::frontmatter_value(content, "description")
}
