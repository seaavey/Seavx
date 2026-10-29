//! Shared memory report used by `seavx memory` and the TUI `/memory`.
//!
//! The report is read-only: memory files are written by the agent's
//! `save_memory` tool, never by this module.

use std::path::{Path, PathBuf};

use crate::agent::skills::SkillManager;

/// Cap a single memory file at 200 lines or 16 KiB, whichever comes first.
const MAX_LINES: usize = 200;
const MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryView {
    All,
    Project,
    User,
    Skills,
    Path,
}

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub content: String,
    pub truncated: bool,
    pub total_lines: usize,
}

#[derive(Debug, Clone)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MemoryReport {
    pub project: Option<MemoryFile>,
    pub user: Option<MemoryFile>,
    pub skills: Vec<SkillSummary>,
}

/// Project memory candidates, highest precedence first.
pub(crate) fn project_candidates(workspace: &Path) -> Vec<PathBuf> {
    vec![
        workspace.join(".seavx/MEMORY.md"),
        workspace.join("MEMORY.md"),
    ]
}

/// User preference candidates, highest precedence first.
pub(crate) fn user_candidates(workspace: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = vec![workspace.join(".seavx/USER.md"), workspace.join("USER.md")];
    if let Some(home) = home {
        candidates.push(home.join(".seavx/USER.md"));
    }
    candidates
}

pub fn build_report(workspace: &Path) -> MemoryReport {
    build_report_with_home(workspace, crate::home::dir().as_deref())
}

/// Testable core: `home` is injected so tests never read the real home.
pub fn build_report_with_home(workspace: &Path, home: Option<&Path>) -> MemoryReport {
    let project = project_candidates(workspace)
        .iter()
        .find_map(|path| read_memory_file(path));
    let user = user_candidates(workspace, home)
        .iter()
        .find_map(|path| read_memory_file(path));

    let skills = SkillManager::new(workspace)
        .list_skills()
        .into_iter()
        .map(|skill| SkillSummary {
            name: skill.name,
            description: skill.description,
            path: skill.path,
        })
        .collect();

    MemoryReport {
        project,
        user,
        skills,
    }
}

/// Read one candidate, capping it at [`MAX_LINES`] lines or [`MAX_BYTES`]
/// bytes without splitting a UTF-8 character.
fn read_memory_file(path: &Path) -> Option<MemoryFile> {
    if !path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let total_lines = raw.lines().count();

    let mut content = String::new();
    let mut kept_lines = 0;
    let mut cut = false;

    for line in raw.lines() {
        if kept_lines >= MAX_LINES {
            cut = true;
            break;
        }

        let separator = usize::from(!content.is_empty());
        if content.len() + separator + line.len() <= MAX_BYTES {
            if separator == 1 {
                content.push('\n');
            }
            content.push_str(line);
            kept_lines += 1;
            continue;
        }

        // Take as much of this line as fits, stopping on a char boundary.
        let budget = MAX_BYTES.saturating_sub(content.len() + separator);
        let mut take = 0;
        for (index, character) in line.char_indices() {
            if index + character.len_utf8() > budget {
                break;
            }
            take = index + character.len_utf8();
        }
        if take > 0 {
            if separator == 1 {
                content.push('\n');
            }
            content.push_str(&line[..take]);
            kept_lines += 1;
        }
        cut = true;
        break;
    }

    Some(MemoryFile {
        path: path.to_path_buf(),
        content,
        truncated: cut || kept_lines < total_lines,
        total_lines,
    })
}

pub fn render_text(report: &MemoryReport, view: MemoryView) -> String {
    let mut sections = Vec::new();

    if matches!(view, MemoryView::All | MemoryView::Project) {
        sections.push(render_file_section(
            "Project memory",
            report.project.as_ref(),
            ".seavx/MEMORY.md",
        ));
    }
    if matches!(view, MemoryView::All | MemoryView::User) {
        sections.push(render_file_section(
            "User preferences",
            report.user.as_ref(),
            ".seavx/USER.md",
        ));
    }
    if matches!(view, MemoryView::All | MemoryView::Skills) {
        sections.push(render_skills(report));
    }
    if matches!(view, MemoryView::Path) {
        sections.push(render_paths(report));
    }

    sections.join("\n\n")
}

fn render_file_section(heading: &str, file: Option<&MemoryFile>, hint: &str) -> String {
    let mut text = format!("## {heading}");
    match file {
        Some(file) => {
            text.push_str(&format!(
                "\n{}\n\n{}",
                display_path(&file.path),
                file.content
            ));
            if file.truncated {
                let shown = file.content.lines().count();
                let more = file.total_lines.saturating_sub(shown);
                text.push_str(&format!("\n... truncated ({more} more lines)"));
            }
        }
        None => {
            text.push_str(&format!(
                "\n{heading}: not found\nThe agent can create one with the `save_memory` tool, or you can add {hint} manually."
            ));
        }
    }
    text
}

fn render_skills(report: &MemoryReport) -> String {
    let mut text = String::from("## Installed skills");
    if report.skills.is_empty() {
        text.push_str("\nNo skills installed.");
        return text;
    }
    for skill in &report.skills {
        text.push_str(&format!(
            "\n- {}: {} ({})",
            skill.name,
            skill.description,
            display_path(&skill.path)
        ));
    }
    text
}

fn render_paths(report: &MemoryReport) -> String {
    let mut text = String::from("## Paths");
    let mut push = |path: &Path, found: bool| {
        text.push_str(&format!(
            "\n- {} ({})",
            display_path(path),
            if found { "found" } else { "missing" }
        ));
    };

    for path in project_candidates(Path::new(".")) {
        push(
            &path,
            report
                .project
                .as_ref()
                .is_some_and(|file| file.path == path),
        );
    }
    if let Some(project) = &report.project
        && !project_candidates(Path::new(".")).contains(&project.path)
    {
        push(&project.path, true);
    }
    for path in user_candidates(Path::new("."), crate::home::dir().as_deref()) {
        push(
            &path,
            report.user.as_ref().is_some_and(|file| file.path == path),
        );
    }
    if let Some(user) = &report.user
        && !user_candidates(Path::new("."), crate::home::dir().as_deref()).contains(&user.path)
    {
        push(&user.path, true);
    }
    text
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

pub fn run() -> anyhow::Result<()> {
    println!(
        "{}",
        render_text(&build_report(Path::new(".")), MemoryView::All)
    );
    Ok(())
}

#[cfg(test)]
#[path = "../tests/cli/memory.rs"]
mod tests;
