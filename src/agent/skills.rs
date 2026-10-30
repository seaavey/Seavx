use std::path::{Path, PathBuf};

pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub struct SkillManager {
    /// Highest priority first: the project (hidden, then open), installed, then
    /// shared agent skills.
    roots: Vec<PathBuf>,
}

impl SkillManager {
    pub fn new(workspace_root: &Path) -> Self {
        Self::with_home(workspace_root, crate::home::dir().as_deref())
    }

    /// Testable core: `home` is injected so tests never read the real one.
    pub fn with_home(workspace_root: &Path, home: Option<&Path>) -> Self {
        let mut roots = vec![
            // The hidden project dir stays first: `save_skill` writes here.
            workspace_root.join(".seavx/skills"),
            // A repo can ship its skills in the open as
            // `skills/<category>/<name>/SKILL.md`. Still a project root, so it
            // outranks the user's own skills rather than being shadowed by them.
            workspace_root.join("skills"),
        ];
        if let Some(home) = home {
            roots.push(home.join(".seavx/skills"));
            roots.push(home.join(".agents/skills"));
        }
        Self { roots }
    }

    /// Precedence order: the first root owns the copy `list_skills` returns.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Every root with the skills it holds. A name may repeat across roots;
    /// [`Self::list_skills`] hides that, `/skills list` shows it.
    pub fn skills_by_root(&self) -> Vec<(&Path, Vec<SkillMetadata>)> {
        self.roots
            .iter()
            .map(|root| {
                let mut list = Vec::new();
                scan_dir(root, &mut list, 0);
                (root.as_path(), list)
            })
            .collect()
    }

    /// List all available skills; earlier roots shadow later ones with the same name
    pub fn list_skills(&self) -> Vec<SkillMetadata> {
        let mut list: Vec<SkillMetadata> = Vec::new();
        for (_, skills) in self.skills_by_root() {
            for skill in skills {
                if list.iter().any(|existing| existing.name == skill.name) {
                    continue;
                }
                list.push(skill);
            }
        }
        list
    }

    /// File a new skill would be written to, resolvable before it exists.
    pub fn skill_path(&self, name: &str) -> Result<PathBuf, String> {
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

        Ok(self.roots[0].join(sanitized_name).join("SKILL.md"))
    }

    /// Automatically save a synthesised skill into the project skills dir
    pub fn save_skill(&self, name: &str, content: &str) -> Result<PathBuf, String> {
        let skill_file = self.skill_path(name)?;
        let skill_dir = skill_file
            .parent()
            .ok_or_else(|| "Invalid skill name".to_string())?;
        std::fs::create_dir_all(skill_dir)
            .map_err(|e| format!("Failed to create skill directory: {e}"))?;

        crate::atomic_file::write_atomic(
            &skill_file,
            content.as_bytes(),
            crate::atomic_file::ModePolicy::Preserve,
        )
        .map_err(|e| format!("Failed to write SKILL.md: {e}"))?;

        Ok(skill_file)
    }
}

/// Every skill under `base`, at most two categories deep, in directory order.
pub(crate) fn scan_dir(base: &Path, list: &mut Vec<SkillMetadata>, depth: usize) {
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

#[cfg(test)]
#[path = "../tests/core/skills.rs"]
mod tests;
