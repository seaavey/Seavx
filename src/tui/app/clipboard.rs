use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Reads text off the system clipboard. A trait so tests never touch a display
/// server, and a blocking call so the deadline in [`read`] has something to
/// bound.
pub trait Clipboard: Send + Sync {
    fn text(&self) -> Result<String, String>;
}

type Reply = Sender<Result<String, String>>;

/// The real clipboard. One worker thread owns one arboard handle for the life of
/// the process: the handle is bound to the thread that opened it, and reopening
/// it per keypress is what a seat binding cannot afford.
#[derive(Default)]
pub struct SystemClipboard {
    worker: OnceLock<Sender<Reply>>,
}

impl SystemClipboard {
    fn worker(&self) -> &Sender<Reply> {
        self.worker.get_or_init(|| {
            let (requests, inbox) = std::sync::mpsc::channel::<Reply>();
            std::thread::spawn(move || serve(&inbox));
            requests
        })
    }
}

/// The worker loop: open the handle on first request and keep it, so a second
/// handle never exists while the first lives.
fn serve(inbox: &Receiver<Reply>) {
    let mut handle = None;
    for reply in inbox {
        if handle.is_none() {
            handle = arboard::Clipboard::new().ok();
        }
        let answer = match handle.as_mut() {
            Some(clipboard) => clipboard.get_text().map_err(|error| error.to_string()),
            None => Err("no clipboard on this display".to_string()),
        };
        let _ = reply.send(answer);
    }
}

impl Clipboard for SystemClipboard {
    fn text(&self) -> Result<String, String> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.worker()
            .send(reply)
            .map_err(|_| "clipboard worker stopped".to_string())?;
        answer
            .recv()
            .map_err(|_| "clipboard worker stopped".to_string())?
    }
}

/// The read happens off the UI thread and gives up after 2 s: on X11 and Wayland
/// the request goes to whatever app owns the selection, and one that never
/// answers must not freeze the frame.
pub(super) async fn read(clipboard: &Arc<dyn Clipboard>) -> Result<String, String> {
    let clipboard = Arc::clone(clipboard);
    let read = tokio::task::spawn_blocking(move || clipboard.text());
    match tokio::time::timeout(Duration::from_secs(2), read).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("did not respond".to_string()),
    }
}

#[cfg(test)]
#[path = "../../tests/tui/clipboard.rs"]
mod tests;
