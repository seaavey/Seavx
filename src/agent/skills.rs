use std::path::{Path, PathBuf};

pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub struct SkillManager {
    skills_dir: PathBuf,
}

impl SkillManager {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            skills_dir: workspace_root.join(".seavx/skills"),
        }
    }

    /// List all available skills in workspace or global dir (~/.seavx/skills)
    pub fn list_skills(&self) -> Vec<SkillMetadata> {
        let mut list = Vec::new();
        self.scan_dir(&self.skills_dir, &mut list);

        if let Ok(home) = std::env::var("HOME") {
            let global_skills = PathBuf::from(home).join(".seavx/skills");
            self.scan_dir(&global_skills, &mut list);
        }

        list
    }

    fn scan_dir(&self, base: &Path, list: &mut Vec<SkillMetadata>) {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let skill_file = path.join("SKILL.md");
                if skill_file.exists() {
                    if let Ok(content) = std::fs::read_to_string(&skill_file) {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let description = extract_description(&content)
                            .unwrap_or_else(|| format!("Skill for {name}"));
                        list.push(SkillMetadata {
                            name,
                            description,
                            path: skill_file,
                        });
                    }
                }
            }
        }
    }

    /// Automatically save a synthesised skill
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

        let skill_dir = self.skills_dir.join(&sanitized_name);
        std::fs::create_dir_all(&skill_dir)
            .map_err(|e| format!("Failed to create skill directory: {e}"))?;

        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, content)
            .map_err(|e| format!("Failed to write SKILL.md: {e}"))?;

        Ok(skill_file)
    }
}

fn extract_description(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("description:") {
            return Some(
                trimmed
                    .trim_start_matches("description:")
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            );
        }
    }
    None
}
