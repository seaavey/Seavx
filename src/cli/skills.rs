//! Shared skill catalogue used by the TUI `/skills` command.
//!
//! Read-only views only: `list` reports the winning copy and the roots that
//! shadow it, `path` reports the roots in precedence order.

use std::path::{Path, PathBuf};

use crate::agent::skills::SkillManager;

/// One skill, as `list` shows it: the winning copy and the roots behind it.
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub root: PathBuf,
    /// Later roots holding a copy of the same name; the first root wins.
    pub shadowed: Vec<PathBuf>,
}

pub struct SkillsReport {
    pub roots: Vec<PathBuf>,
    pub skills: Vec<SkillRow>,
}

pub fn list_report(workspace: &Path) -> SkillsReport {
    list_report_with_home(workspace, crate::home::dir().as_deref())
}

/// Testable core: `home` is injected so tests never read the real one.
pub fn list_report_with_home(workspace: &Path, home: Option<&Path>) -> SkillsReport {
    let manager = SkillManager::with_home(workspace, home);
    let mut skills: Vec<SkillRow> = Vec::new();

    for (root, held) in manager.skills_by_root() {
        for skill in held {
            match skills.iter_mut().find(|row| row.name == skill.name) {
                Some(row) => row.shadowed.push(root.to_path_buf()),
                None => skills.push(SkillRow {
                    name: skill.name,
                    description: skill.description,
                    path: skill.path,
                    root: root.to_path_buf(),
                    shadowed: Vec::new(),
                }),
            }
        }
    }

    SkillsReport {
        skills,
        roots: manager.roots().to_vec(),
    }
}

pub fn render_list(report: &SkillsReport) -> String {
    let mut text = String::from("## Installed skills");
    if report.skills.is_empty() {
        text.push_str("\nNo skills installed.\n\n");
        text.push_str(&render_roots(&report.roots));
        return text;
    }

    for row in &report.skills {
        text.push_str(&format!(
            "\n- {}: {} ({})",
            row.name,
            one_line(&row.description),
            display_path(&row.root)
        ));
        if !row.shadowed.is_empty() {
            let others: Vec<String> = row.shadowed.iter().map(|root| display_path(root)).collect();
            text.push_str(&format!(" [shadowed: {}]", others.join(", ")));
        }
    }
    text
}

pub fn render_paths(workspace: &Path) -> String {
    render_paths_with_home(workspace, crate::home::dir().as_deref())
}

/// Testable core: `home` is injected so tests never read the real one.
pub fn render_paths_with_home(workspace: &Path, home: Option<&Path>) -> String {
    render_roots(SkillManager::with_home(workspace, home).roots())
}

fn render_roots(roots: &[PathBuf]) -> String {
    let mut text = String::from("## Skill roots (highest precedence first)");
    for root in roots {
        text.push_str(&format!(
            "\n- {} ({})",
            display_path(root),
            if root.is_dir() { "found" } else { "missing" }
        ));
    }
    text
}

/// First sentence of a description, capped, so one skill is one row.
fn one_line(description: &str) -> String {
    let first = description.split(". ").next().unwrap_or(description).trim();
    match first.char_indices().nth(100) {
        Some((cut, _)) => format!("{}…", first[..cut].trim_end()),
        None => first.to_string(),
    }
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

#[cfg(test)]
#[path = "../tests/cli/skills.rs"]
mod tests;
