use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// What a clipboard hands over: decoded pixels, the only shape arboard produces.
/// The normalizer encodes them once.
#[derive(Clone)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Reads the system clipboard. A trait so tests never touch a display server,
/// and a blocking call so the deadline in [`read`] has something to bound.
pub trait Clipboard: Send + Sync {
    fn text(&self) -> Result<String, String>;
    fn image(&self) -> Result<ClipboardImage, String>;
}

type Reply = Sender<Result<Answer, String>>;

enum Answer {
    Text(String),
    Image(ClipboardImage),
}

enum Request {
    Text(Reply),
    Image(Reply),
}

/// The real clipboard. One worker thread owns one arboard handle for the life of
/// the process: the handle is bound to the thread that opened it, and reopening
/// it per keypress is what a seat binding cannot afford.
#[derive(Default)]
pub struct SystemClipboard {
    worker: OnceLock<Sender<Request>>,
}

impl SystemClipboard {
    fn worker(&self) -> &Sender<Request> {
        self.worker.get_or_init(|| {
            let (requests, inbox) = std::sync::mpsc::channel::<Request>();
            std::thread::spawn(move || serve(&inbox));
            requests
        })
    }

    fn ask(&self, request: impl FnOnce(Reply) -> Request) -> Result<Answer, String> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.worker()
            .send(request(reply))
            .map_err(|_| "clipboard worker stopped".to_string())?;
        answer
            .recv()
            .map_err(|_| "clipboard worker stopped".to_string())?
    }
}

/// The worker loop: open the handle on first request and keep it, so a second
/// handle never exists while the first lives.
fn serve(inbox: &Receiver<Request>) {
    let mut handle: Option<arboard::Clipboard> = None;
    for request in inbox {
        if handle.is_none() {
            handle = arboard::Clipboard::new().ok();
        }
        let (wants_image, reply) = match request {
            Request::Text(reply) => (false, reply),
            Request::Image(reply) => (true, reply),
        };
        let answer = match handle.as_mut() {
            None => Err("no clipboard on this display".to_string()),
            Some(clipboard) if wants_image => clipboard
                .get_image()
                .map(|image| {
                    Answer::Image(ClipboardImage {
                        width: image.width as u32,
                        height: image.height as u32,
                        bytes: image.bytes.into_owned(),
                    })
                })
                .map_err(|error| error.to_string()),
            Some(clipboard) => clipboard
                .get_text()
                .map(Answer::Text)
                .map_err(|error| error.to_string()),
        };
        let _ = reply.send(answer);
    }
}

impl Clipboard for SystemClipboard {
    fn text(&self) -> Result<String, String> {
        match self.ask(Request::Text)? {
            Answer::Text(text) => Ok(text),
            Answer::Image(_) => Err("not available as text".to_string()),
        }
    }

    fn image(&self) -> Result<ClipboardImage, String> {
        match self.ask(Request::Image)? {
            Answer::Image(image) => Ok(image),
            Answer::Text(_) => Err("not available as an image".to_string()),
        }
    }
}

/// One clipboard read, off the UI thread and bounded to 2 s: on X11 and Wayland
/// the request goes to whatever app owns the selection, and one that never
/// answers must not freeze the frame.
pub(super) async fn read<T, F>(clipboard: &Arc<dyn Clipboard>, call: F) -> Result<T, String>
where
    T: Send + 'static,
    F: Fn(&dyn Clipboard) -> Result<T, String> + Send + 'static,
{
    let clipboard = Arc::clone(clipboard);
    let task = tokio::task::spawn_blocking(move || call(clipboard.as_ref()));
    match tokio::time::timeout(Duration::from_secs(2), task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("did not respond".to_string()),
    }
}

#[cfg(test)]
#[path = "../../tests/tui/clipboard.rs"]
mod tests;
