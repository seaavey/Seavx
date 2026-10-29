//! Cancelling a running turn and the turn's state machine.
//!
//! The token is the only thing the TUI sets; the engine polls it between turns,
//! around every provider future and inside a tool call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a waiting turn re-checks the token. A poll interval keeps the
/// token a plain `Clone` handle with no task to outlive the turn.
const POLL: Duration = Duration::from_millis(20);

/// How long a killed group gets between `SIGTERM` and `SIGKILL`.
pub const TERM_GRACE: Duration = Duration::from_millis(200);
/// How long the OS gets to confirm the child is gone before the cleanup is
/// reported as unconfirmed.
pub const REAP_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    /// The last killed process tree, recorded by the tool that sent the signal
    /// so the turn can report what the OS said.
    killed: Arc<Mutex<Option<(u32, Cleanup)>>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note_cleanup(&self, pid: u32, cleanup: Cleanup) {
        *self
            .killed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((pid, cleanup));
    }

    pub fn last_cleanup(&self) -> Option<(u32, Cleanup)> {
        *self
            .killed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once the token is set, immediately if it already is.
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(POLL).await;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Running,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl TurnStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed | Self::Failed)
    }
}

/// What the OS said about a killed process tree. Only the reported value is
/// ever shown as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    Confirmed,
    Unconfirmed,
}

/// Kill the child's whole process group, then reap it. The return value is what
/// the OS confirmed: a kill that was sent but not reaped is never called clean.
pub async fn kill_tree(child: &mut tokio::process::Child, pid: u32) -> Cleanup {
    kill_tree_within(child, pid, REAP_WINDOW).await
}

/// [`kill_tree`] with the reap window as a parameter, so a test can force the
/// timeout branch without waiting two seconds.
pub async fn kill_tree_within(
    child: &mut tokio::process::Child,
    pid: u32,
    window: Duration,
) -> Cleanup {
    #[cfg(unix)]
    {
        // The child was spawned with `process_group(0)`, so the negative pid is
        // the group: the shell and everything it started.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        tokio::time::sleep(TERM_GRACE).await;
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }

    reap_or_report(
        async {
            let _ = child.wait().await;
        },
        window,
    )
    .await
}

/// The reap decision on its own: the kernel confirms every `SIGKILL`, so the
/// timeout branch is only reachable through this seam.
pub async fn reap_or_report<F: std::future::Future<Output = ()>>(
    reap: F,
    window: Duration,
) -> Cleanup {
    match tokio::time::timeout(window, reap).await {
        Ok(()) => Cleanup::Confirmed,
        Err(_) => Cleanup::Unconfirmed,
    }
}

/// The one line a user sees when the OS did not confirm the kill.
pub fn cleanup_notice(pid: u32) -> String {
    format!("cancellation could not be confirmed; pid {pid} may still be running")
}

/// The turn state machine: forward edges only, a terminal status is final.
#[derive(Debug)]
pub struct TurnState {
    status: TurnStatus,
    /// The last killed process tree: what the OS said, and which pid.
    killed: Option<(u32, Cleanup)>,
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            status: TurnStatus::Running,
            killed: None,
        }
    }
}

impl TurnState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> TurnStatus {
        self.status
    }

    pub fn cleanup(&self) -> Option<Cleanup> {
        self.killed.map(|(_, cleanup)| cleanup)
    }

    pub fn pid(&self) -> Option<u32> {
        self.killed.map(|(pid, _)| pid)
    }

    pub fn set_cleanup(&mut self, pid: u32, cleanup: Cleanup) {
        self.killed = Some((pid, cleanup));
    }

    /// `true` when the edge exists; a rejected edge leaves the status alone.
    pub fn transition(&mut self, next: TurnStatus) -> bool {
        let allowed = matches!(
            (self.status, next),
            (TurnStatus::Running, TurnStatus::Cancelling)
                | (TurnStatus::Running, TurnStatus::Completed)
                | (TurnStatus::Running, TurnStatus::Failed)
                | (TurnStatus::Cancelling, TurnStatus::Cancelled)
                | (TurnStatus::Cancelling, TurnStatus::Failed)
        );
        if allowed {
            self.status = next;
        }
        allowed
    }
}

#[cfg(test)]
#[path = "../tests/core/cancel.rs"]
mod tests;
