//! Shared skill catalogue used by the TUI `/skills` command.
//!
//! Read-only views only: `list` reports the winning copy and the roots that
//! shadow it, `show` prints one body, `path` reports the roots in precedence
//! order.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::agent::skills::{SkillManager, scan_dir};
use crate::cli::memory;

/// One skill, as `list` shows it: the winning copy and the roots behind it.
pub struct SkillRow {
    pub name: String,
    pub description: String,
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

/// The winning `SKILL.md` of one skill, cut like a memory file so a body
/// cannot flood the screen. An unknown name is an error, never a blank page.
pub fn render_show(workspace: &Path, name: &str) -> Result<String, String> {
    render_show_with_home(workspace, crate::home::dir().as_deref(), name)
}

/// Testable core: `home` is injected so tests never read the real one.
pub fn render_show_with_home(
    workspace: &Path,
    home: Option<&Path>,
    name: &str,
) -> Result<String, String> {
    let path = SkillManager::with_home(workspace, home)
        .list_skills()
        .into_iter()
        .find(|skill| skill.name == name)
        .map(|skill| skill.path)
        .ok_or_else(|| format!("No skill named {name}."))?;
    let file = memory::read_memory_file(&path)
        .ok_or_else(|| format!("Cannot read {} for {name}.", display_path(&path)))?;

    let mut text = format!("## {name}\n{}\n\n{}", display_path(&path), file.content);
    if file.truncated {
        let shown = file.content.lines().count();
        let more = file.total_lines.saturating_sub(shown);
        text.push_str(&format!("\n... truncated ({more} more lines)"));
    }
    Ok(text)
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
pub(crate) fn one_line(description: &str) -> String {
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

/// Install a skill from a local directory into the workspace's own root.
///
/// The name is the source directory's basename, because that is how the loader
/// names skills. Nothing is overwritten: an existing destination is an error,
/// and a symlink anywhere in the source is refused rather than followed, so an
/// install can never copy files from outside the directory the user pointed at.
pub fn install(workspace: &Path, source: &Path) -> Result<PathBuf, String> {
    if !source.is_dir() {
        return Err(format!("{} is not a directory.", source.display()));
    }
    if !source.join("SKILL.md").is_file() {
        return Err(format!("{} has no SKILL.md.", source.display()));
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    if name.is_empty()
        || name.contains('/')
        || name.contains("..")
        || crate::util::is_skipped(&name)
    {
        return Err(format!("{name:?} is not a usable skill name."));
    }

    let destination = workspace.join(".seavx/skills").join(&name);
    if destination.exists() {
        return Err(format!("{} already exists.", destination.display()));
    }

    // A copy that fails halfway must not leave a half-installed skill behind.
    if let Err(error) = copy_tree(source, &destination) {
        let _ = std::fs::remove_dir_all(&destination);
        return Err(error);
    }
    Ok(destination)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destination)
        .map_err(|error| format!("Cannot create {}: {error}", destination.display()))?;

    let entries = std::fs::read_dir(source)
        .map_err(|error| format!("Cannot read {}: {error}", source.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| format!("Cannot inspect {}: {error}", from.display()))?;
        if kind.is_symlink() {
            return Err(format!(
                "{} is a symlink; refusing to copy it.",
                from.display()
            ));
        }
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|error| format!("Cannot copy {}: {error}", from.display()))?;
        }
    }
    Ok(())
}

/// One skill found in a fetched repository, before the owner picks it.
#[derive(Debug)]
pub struct Candidate {
    pub name: String,
    // The picker shows this; `/skills install` is what fills it, next task.
    pub description: String,
    pub source: PathBuf,
}

/// A fetched repository and the skills inside it. The checkout lives as long
/// as this value, so a candidate can still be copied after the scan.
#[derive(Debug)]
pub struct Fetched {
    // Held only for its `Drop`: the candidates' sources live inside it.
    _checkout: Checkout,
    pub candidates: Vec<Candidate>,
}

impl Fetched {
    /// Copy candidate `index` in, under [`install`]'s rules.
    pub fn install(&self, workspace: &Path, index: usize) -> Result<PathBuf, String> {
        let candidate = self
            .candidates
            .get(index)
            .ok_or_else(|| format!("There is no skill at position {}.", index + 1))?;
        install(workspace, &candidate.source)
    }
}

/// A clone that deletes itself, so a fetched SKILL.md never outlives the pick.
#[derive(Debug)]
struct Checkout {
    dir: PathBuf,
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Clone `url` shallowly and list every `SKILL.md` inside it.
///
/// `git` does the fetch: one feature does not buy a dependency. A URL that is
/// not a repository is an error; a repository with no skill is an empty list.
pub fn fetch(url: &str) -> Result<Fetched, String> {
    let checkout = Checkout {
        dir: std::env::temp_dir().join(format!(
            "seavx-skills-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )),
    };

    let mut child = std::process::Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(&checkout.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run git for {url}: {error}"))?;

    // A hung clone must not hang the picker: 60 s, then kill it.
    // shortcut: stderr is drained only after exit, so a clone that floods the
    // pipe waits out the budget and reports a timeout; read it on a thread if
    // that ever happens.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Fetching {url} timed out after 60 s."));
            }
            Err(error) => return Err(format!("Cannot wait for git: {error}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("Cannot read git's output: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut found = Vec::new();
    scan_dir(&checkout.dir, &mut found, 0);
    let mut candidates: Vec<Candidate> = found
        .into_iter()
        .filter_map(|skill| {
            Some(Candidate {
                name: skill.name,
                description: skill.description,
                source: skill.path.parent()?.to_path_buf(),
            })
        })
        .collect();
    candidates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Fetched {
        _checkout: checkout,
        candidates,
    })
}

/// Remove the winning copy of a skill.
///
/// A symlinked skill directory is refused: the shared roots are full of links on
/// some machines, and `remove_dir_all` would delete the target, not the link.
pub fn remove(workspace: &Path, name: &str) -> Result<PathBuf, String> {
    remove_with_home(workspace, crate::home::dir().as_deref(), name)
}

/// Testable core: `home` is injected so tests never read the real one.
pub fn remove_with_home(
    workspace: &Path,
    home: Option<&Path>,
    name: &str,
) -> Result<PathBuf, String> {
    let path = SkillManager::with_home(workspace, home)
        .list_skills()
        .into_iter()
        .find(|skill| skill.name == name)
        .map(|skill| skill.path)
        .ok_or_else(|| format!("No skill named {name}."))?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory.", path.display()))?
        .to_path_buf();

    let kind = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("Cannot inspect {}: {error}", dir.display()))?;
    if kind.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; remove it where it points instead.",
            dir.display()
        ));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|error| format!("Cannot remove {}: {error}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
#[path = "../tests/cli/skills.rs"]
mod tests;
