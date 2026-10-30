use ratatui::layout::Rect;

use super::{App, AtPopupLayout, new_input};

const MAX_COMPLETION_RESULTS: usize = 20;

pub fn scan_workspace_entries(query: &str) -> Vec<(bool, String)> {
    scan_workspace_entries_at(std::path::Path::new("."), query)
}

/// Root-aware core of [`scan_workspace_entries`]. Tests scan an isolated tree
/// without changing the process working directory.
pub(super) fn scan_workspace_entries_at(
    root: &std::path::Path,
    query: &str,
) -> Vec<(bool, String)> {
    let clean_query = query.trim_start_matches('@').to_lowercase();
    let mut results = Vec::new();

    fn walk_dir(
        dir: &std::path::Path,
        root: &std::path::Path,
        results: &mut Vec<(bool, String)>,
        query: &str,
    ) {
        if results.len() >= MAX_COMPLETION_RESULTS {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if results.len() >= MAX_COMPLETION_RESULTS {
                break;
            }
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if crate::util::is_skipped(name) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            // `DirEntry::file_type` does not follow symlinks: a linked directory
            // is listed as a candidate but never traversed, so it cannot expose
            // names outside the workspace or walk a cycle.
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if query.is_empty() || rel.to_lowercase().contains(query) {
                results.push((is_dir, rel));
            }
            if is_dir {
                walk_dir(&path, root, results, query);
            }
        }
    }

    walk_dir(root, root, &mut results, &clean_query);
    results.sort_by(|a, b| a.1.cmp(&b.1));
    results
}

/// Cached `@` query and its matches, so rendering never walks the workspace.
pub(super) type AtCache = Option<(String, Vec<(bool, String)>)>;

impl App<'_> {
    /// Geometry of the `@` reference popup, shared by rendering and mouse hit-testing.
    pub fn at_popup_layout(&self, area: Rect, match_count: usize) -> AtPopupLayout {
        const MAX_VISIBLE: usize = 8;

        let menu_height = (match_count.min(MAX_VISIBLE) as u16 + 2).min(10);
        let menu_width = 70.min(area.width.saturating_sub(4));
        // Prompt box starts where the footer (1) and prompt (input_height) leave room.
        let prompt_y = area.height.saturating_sub(self.input_height() + 2);
        let menu_y = prompt_y.saturating_sub(menu_height);
        let selected = self.at_selected.min(match_count.saturating_sub(1));

        AtPopupLayout {
            area: Rect::new(area.x + 2, menu_y, menu_width, menu_height),
            window_start: selected.saturating_sub(MAX_VISIBLE - 1),
            max_visible: MAX_VISIBLE,
        }
    }

    /// Trailing `@word` in the prompt, if any.
    pub fn at_query(&self) -> Option<String> {
        self.input_text()
            .split_whitespace()
            .last()
            .filter(|word| word.starts_with('@'))
            .map(str::to_string)
    }

    /// Workspace entries matching the trailing `@word`, scanned once per query
    /// so neither key handling nor rendering repeats the tree walk.
    pub fn at_matches(&self) -> Vec<(bool, String)> {
        let Some(query) = self.at_query() else {
            return Vec::new();
        };
        if let Some((cached_query, matches)) = self.at_cache.borrow().as_ref()
            && *cached_query == query
        {
            return matches.clone();
        }
        let matches = scan_workspace_entries(&query);
        *self.at_cache.borrow_mut() = Some((query, matches.clone()));
        matches
    }

    /// Slash commands matching the prompt; empty unless it is a bare `/word`.
    pub fn slash_matches(&self) -> Vec<(&'static str, &'static str)> {
        let input = self.input_text();
        if !input.starts_with('/') || input.contains(' ') {
            return Vec::new();
        }
        crate::tui::commands::palette()
            .into_iter()
            .filter(|(cmd, _)| cmd.starts_with(&input))
            .collect()
    }

    /// Whether the user closed the popups for exactly this prompt text; typing
    /// anything else brings them back.
    pub fn popups_dismissed(&self) -> bool {
        self.dismissed_popup.as_deref() == Some(self.input_text().as_str())
    }

    /// Hide the slash and `@` popups until the prompt text changes.
    pub fn dismiss_popups(&mut self) {
        self.dismissed_popup = Some(self.input_text());
    }

    /// Whether any autocomplete popup is showing, so Esc closes it first.
    pub fn popup_open(&self) -> bool {
        !self.popups_dismissed()
            && (!self.at_matches().is_empty() || !self.slash_matches().is_empty())
    }

    /// Replace the trailing `@word` with a `[folder:…]` or `[file:…]` tag.
    pub fn insert_at_tag(&mut self, is_dir: bool, path: &str) {
        let tag = if is_dir {
            format!("[folder:{path}] ")
        } else {
            format!("[file:{path}] ")
        };
        let full_input = self.input_text();
        if let Some(last_at_idx) = full_input.rfind('@') {
            let mut new_text = full_input[..last_at_idx].to_string();
            new_text.push_str(&tag);
            // The ghost tail belongs to the prompt being replaced; keeping the
            // index would let the next keypress truncate the new tag.
            self.ghost_typed = None;
            self.input = new_input();
            self.input.insert_str(new_text);
        }
    }
}
