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
    /// Put text on the clipboard. The writer must keep its handle alive after
    /// returning: on X11 the clipboard *is* that connection.
    fn write(&self, text: &str) -> Result<(), String>;
}

type Reply = Sender<Result<Answer, String>>;

enum Answer {
    Text(String),
    Image(ClipboardImage),
    Written,
}

enum Request {
    Text(Reply),
    Image(Reply),
    Write(String, Reply),
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
    let mut open_error: Option<String> = None;
    for request in inbox {
        if handle.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => handle = Some(clipboard),
                Err(error) => open_error = Some(error.to_string()),
            }
        }
        // The host has no backend, or the one it has refused: say which.
        let unavailable = || match &open_error {
            Some(error) => format!("no clipboard: {error}"),
            None => "no clipboard on this display".to_string(),
        };
        let (reply, answer) = match request {
            Request::Text(reply) => (
                reply,
                match handle.as_mut() {
                    None => Err(unavailable()),
                    Some(clipboard) => clipboard
                        .get_text()
                        .map(Answer::Text)
                        .map_err(|error| error.to_string()),
                },
            ),
            Request::Image(reply) => (
                reply,
                match handle.as_mut() {
                    None => Err(unavailable()),
                    Some(clipboard) => clipboard
                        .get_image()
                        .map(|image| {
                            Answer::Image(ClipboardImage {
                                width: image.width as u32,
                                height: image.height as u32,
                                bytes: image.bytes.into_owned(),
                            })
                        })
                        .map_err(|error| error.to_string()),
                },
            ),
            Request::Write(text, reply) => (
                reply,
                match handle.as_mut() {
                    None => Err(unavailable()),
                    Some(clipboard) => clipboard
                        .set_text(text)
                        .map(|_| Answer::Written)
                        .map_err(|error| error.to_string()),
                },
            ),
        };
        let _ = reply.send(answer);
    }
}

impl Clipboard for SystemClipboard {
    fn text(&self) -> Result<String, String> {
        match self.ask(Request::Text)? {
            Answer::Text(text) => Ok(text),
            Answer::Image(_) | Answer::Written => Err("not available as text".to_string()),
        }
    }

    fn image(&self) -> Result<ClipboardImage, String> {
        match self.ask(Request::Image)? {
            Answer::Image(image) => Ok(image),
            Answer::Text(_) | Answer::Written => Err("not available as an image".to_string()),
        }
    }

    fn write(&self, text: &str) -> Result<(), String> {
        match self.ask(|reply| Request::Write(text.to_string(), reply))? {
            Answer::Written => Ok(()),
            Answer::Text(_) | Answer::Image(_) => {
                Err("the clipboard did not take the text".to_string())
            }
        }
    }
}

/// Copy without a clipboard. OSC 52 asks the terminal the user is looking at —
/// the one channel that survives SSH — and the file keeps the text when that
/// terminal drops the sequence. Both are written every time: a terminal that
/// ignores the sequence answers nothing, so there is no failure to fall back on.
pub(super) fn copy_without_clipboard(
    text: &str,
    root: &std::path::Path,
    out: &mut impl std::io::Write,
) -> Result<std::path::PathBuf, String> {
    use base64::Engine;
    use std::io::Write;

    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    write!(out, "\x1b]52;c;{encoded}\x07")
        .and_then(|()| out.flush())
        .map_err(|error| format!("terminal refused the write: {error}"))?;

    let dir = root.join(".seavx");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let path = dir.join("last-copy.txt");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // The block can hold anything the model printed; keep it off other accounts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
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
