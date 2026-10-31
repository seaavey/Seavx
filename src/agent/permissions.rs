//! One permission matrix, the answer types and the seam that asks a human.
//!
//! Every tool call is decided here, and only here: the tool declares what it can
//! do, the matrix turns that plus the configured mode into allow/ask/deny, and a
//! [`Confirmer`] answers the asks. A denial is a tool result, never a crash.

use std::path::{Path, PathBuf};

use crate::agent::security::WorkspaceSecurity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Read,
    Write,
    GlobalWrite,
    Execute,
    Network,
    Destructive,
}

impl ActionKind {
    /// The config spelling, so errors and prompts name the same word.
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::Read => "read",
            ActionKind::Write => "write",
            ActionKind::GlobalWrite => "global-write",
            ActionKind::Execute => "execute",
            ActionKind::Network => "network",
            ActionKind::Destructive => "destructive",
        }
    }

    /// One-line effect sentence for the prompt, or for a denial message.
    pub fn effect(self, detail: &str) -> String {
        match self {
            ActionKind::Read => format!("reads {detail}"),
            ActionKind::Write => format!("writes {detail} inside the workspace"),
            ActionKind::GlobalWrite => format!("writes {detail} outside the workspace"),
            ActionKind::Execute => format!("runs the command {detail}"),
            ActionKind::Network => format!("reaches the network: {detail}"),
            ActionKind::Destructive => format!("could destroy data: {detail}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Once,
    Category,
    Unrestricted,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub kind: ActionKind,
    pub tool: String,
    pub detail: String,
    pub effect: String,
}

impl PermissionRequest {
    pub fn new(kind: ActionKind, tool: &str, detail: &str) -> Self {
        let detail = crate::agent::security::redact(detail);
        Self {
            kind,
            tool: tool.to_string(),
            effect: kind.effect(&detail),
            detail,
        }
    }
}

#[async_trait::async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, request: PermissionRequest) -> Answer;
}

/// `seavx exec` has no human to ask, so every ask resolves as a denial.
pub struct DenyingConfirmer;

#[async_trait::async_trait]
impl Confirmer for DenyingConfirmer {
    async fn confirm(&self, _request: PermissionRequest) -> Answer {
        Answer::Deny
    }
}

/// The matrix. Destructive is refused unconditionally, a global write is asked
/// in every mode, and a workspace write asks like a command: `permitted` lifts
/// write, execute and network out of `ask`, never out of `deny` or the floor.
pub fn decide(mode: PermissionMode, permitted: &[ActionKind], kind: ActionKind) -> Verdict {
    match kind {
        ActionKind::Destructive => Verdict::Deny,
        ActionKind::GlobalWrite => match mode {
            PermissionMode::Deny => Verdict::Deny,
            PermissionMode::Ask | PermissionMode::Allow => Verdict::Ask,
        },
        ActionKind::Read => Verdict::Allow,
        ActionKind::Write | ActionKind::Execute | ActionKind::Network => match mode {
            PermissionMode::Deny => Verdict::Deny,
            PermissionMode::Allow => Verdict::Allow,
            PermissionMode::Ask => {
                if permitted.contains(&kind) {
                    Verdict::Allow
                } else {
                    Verdict::Ask
                }
            }
        },
    }
}

/// Ask the human for one action and honour the answer, including the two
/// "remember this" answers. This is the half after [`decide`] said `Ask`: the
/// agent loop and `/test` share it, so a category approval from either one
/// records the same thing and a refusal reads the same.
pub async fn approve(
    confirm: &dyn Confirmer,
    kind: ActionKind,
    tool: &str,
    detail: &str,
    config_path: &Path,
) -> Result<(), String> {
    let denied = || format!("Permission denied: {}", kind.effect(detail));
    let answer = confirm
        .confirm(PermissionRequest::new(kind, tool, detail))
        .await;
    match answer {
        Answer::Once => Ok(()),
        Answer::Category => match crate::cli::config::append_permitted(config_path, kind) {
            Ok(kinds) => {
                tracing::info!(action = %kind.name(), ?kinds, "permitted action recorded");
                Ok(())
            }
            // The user answered; a config that cannot record it must not turn
            // an approval into a denial.
            Err(error) => {
                tracing::warn!(%error, action = %kind.name(), "could not record permitted action");
                Ok(())
            }
        },
        Answer::Unrestricted => {
            if let Err(error) = crate::cli::config::set_value(
                config_path,
                crate::cli::config::ConfigKey::ToolPermissions,
                "allow",
            ) {
                tracing::warn!(%error, "could not persist tool_permissions");
            }
            Ok(())
        }
        Answer::Deny => Err(denied()),
    }
}

pub fn parse_mode(raw: &str) -> Result<PermissionMode, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "deny" => Ok(PermissionMode::Deny),
        "ask" => Ok(PermissionMode::Ask),
        "allow" => Ok(PermissionMode::Allow),
        other => Err(format!(
            "tool_permissions must be one of deny, ask, allow (got `{other}`)"
        )),
    }
}

/// Only write, execute and network are liftable. `global-write` and the
/// destructive floor are not, so a config naming them is an error rather than a
/// decision.
pub fn parse_permitted(raw: &[String]) -> Result<Vec<ActionKind>, String> {
    let mut kinds = Vec::new();
    for entry in raw {
        let kind = match entry.trim().to_ascii_lowercase().as_str() {
            "write" => ActionKind::Write,
            "execute" => ActionKind::Execute,
            "network" => ActionKind::Network,
            other => {
                return Err(format!(
                    "permitted_actions accepts write, execute, network; `{other}` is not liftable"
                ));
            }
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

/// The only roots a write may leave the workspace for: memory files and skills.
pub fn global_root_for(home: Option<&Path>, path: &Path) -> Option<PathBuf> {
    let home = home?;
    let memory = [home.join(".seavx/MEMORY.md"), home.join(".seavx/USER.md")];
    if let Some(root) = memory.into_iter().find(|root| root == path) {
        return Some(root);
    }
    [home.join(".seavx/skills"), home.join(".agents/skills")]
        .into_iter()
        .find(|root| path.starts_with(root))
}

pub fn classify_target(security: &WorkspaceSecurity, path: &Path) -> Result<ActionKind, String> {
    classify_target_with_home(crate::home::dir().as_deref(), security, path)
}

/// Injected-home form, so tests never depend on the process `HOME`.
pub fn classify_target_with_home(
    home: Option<&Path>,
    security: &WorkspaceSecurity,
    path: &Path,
) -> Result<ActionKind, String> {
    if path.starts_with(security.root()) {
        return Ok(ActionKind::Write);
    }
    if global_root_for(home, path).is_some() {
        return Ok(ActionKind::GlobalWrite);
    }
    Err(format!(
        "Refusing to write outside the workspace: {}",
        path.display()
    ))
}
