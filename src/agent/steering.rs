//! Steering: what the user says to a turn that is already running.
//!
//! Read at a turn boundary, never mid-tool-call, and never through the
//! cancellation token: steering redirects a running turn, it does not stop it.

/// Hands back the pending instruction, if any, and forgets it.
pub trait Steering: Send + Sync {
    fn take(&self) -> Option<String>;
}

/// The default: nobody is steering.
pub struct NoSteering;

impl Steering for NoSteering {
    fn take(&self) -> Option<String> {
        None
    }
}
