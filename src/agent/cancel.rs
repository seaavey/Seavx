//! Cancelling a running turn and the turn's state machine.
//!
//! The token is the only thing the TUI sets; the engine polls it between turns,
//! around every provider future and inside a tool call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a waiting turn re-checks the token. A poll interval keeps the
/// token a plain `Clone` handle with no task to outlive the turn.
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
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

/// The turn state machine: forward edges only, a terminal status is final.
#[derive(Debug)]
pub struct TurnState {
    status: TurnStatus,
    cleanup: Option<Cleanup>,
}

impl Default for TurnState {
    fn default() -> Self {
        Self {
            status: TurnStatus::Running,
            cleanup: None,
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
        self.cleanup
    }

    pub fn set_cleanup(&mut self, cleanup: Cleanup) {
        self.cleanup = Some(cleanup);
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
