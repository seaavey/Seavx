use crate::agent::backend::{ChatBackend, GenaiBackend, StreamDelta};
use genai::Client;
use genai::chat::{ChatMessage, ChatRequest, MessageContent};
use genai::resolver::{AuthData, Endpoint};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The production backend is only exercised through the seam here: a real
/// request needs a provider, and the engine tests use the fake.
#[test]
fn the_backend_names_the_two_stream_deltas() {
    assert_ne!(
        StreamDelta::Token("x".into()),
        StreamDelta::Thinking("x".into())
    );
}

#[test]
fn a_genai_backend_is_constructible_from_a_client() {
    let backend = GenaiBackend::new(genai::Client::new().unwrap());
    // The trait object is what the engine holds.
    let _: &dyn ChatBackend = &backend;
}

#[test]
fn an_unconfigured_client_fails_without_a_network_call() {
    // A provider with no endpoint cannot be resolved: the error comes back
    // through the trait, not as a panic.
    let backend = GenaiBackend::new(genai::Client::new().unwrap());
    let request = genai::chat::ChatRequest::default()
        .append_message(ChatMessage::assistant(MessageContent::from_text("x")));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(backend.send("no-such-model-xyz", request, None));
    assert!(
        result.is_err(),
        "an unknown model is an error, not an answer"
    );
}

/// One SSE frame carrying one assistant text delta.
fn text_frame(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n")
}

fn request() -> ChatRequest {
    ChatRequest::new(vec![ChatMessage::user("hi")])
}

/// A one-request SSE server answering with `payload`, written in `piece`-byte
/// slices so a test can split a frame across any boundary it likes.
async fn serve(payload: String, piece: usize) -> (GenaiBackend, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint::from_owned(format!("http://{}/v1/", listener.local_addr().unwrap()));
    let client = Client::builder()
        .with_service_target_resolver_fn(move |mut target: genai::ServiceTarget| {
            target.endpoint = endpoint.clone();
            target.auth = AuthData::from_single("test");
            Ok(target)
        })
        .build()
        .unwrap();

    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request head, then its body, so the client is done sending.
        let mut head = Vec::new();
        let mut buf = [0u8; 4096];
        let body_start = loop {
            let n = socket.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "client closed before sending a request");
            head.extend_from_slice(&buf[..n]);
            if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let text = String::from_utf8_lossy(&head[..body_start]).to_lowercase();
        let length: usize = text
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .map_or(0, |value| value.trim().parse().unwrap());
        while head.len() < body_start + length {
            let n = socket.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "client closed mid-body");
            head.extend_from_slice(&buf[..n]);
        }

        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for chunk in payload.as_bytes().chunks(piece) {
            socket.write_all(chunk).await.unwrap();
            socket.flush().await.unwrap();
        }
    });

    (GenaiBackend::new(client), task)
}

async fn collect(backend: &GenaiBackend) -> (Result<String, String>, Vec<StreamDelta>) {
    let mut deltas = Vec::new();
    let result = backend
        .stream("gpt-4o-mini", request(), None, &mut |delta| {
            deltas.push(delta)
        })
        .await;
    let result = result
        .map(|response| response.texts().join(""))
        .map_err(|error| error.to_string());
    (result, deltas)
}

#[tokio::test]
async fn the_stream_assembles_text_split_at_every_boundary() {
    let payload = format!(
        "{}{}{}data: [DONE]\n\n",
        text_frame("hel"),
        text_frame("lo, "),
        text_frame("world")
    );
    let one_write = payload.len();

    for piece in [one_write, 1, 7] {
        let (backend, server) = serve(payload.clone(), piece).await;
        let (text, deltas) = collect(&backend).await;
        server.await.unwrap();

        assert_eq!(text.unwrap(), "hello, world", "piece = {piece}");
        assert_eq!(
            deltas,
            vec![
                StreamDelta::Token("hel".into()),
                StreamDelta::Token("lo, ".into()),
                StreamDelta::Token("world".into()),
            ],
            "piece = {piece}"
        );
    }
}

#[tokio::test]
async fn a_multi_line_data_event_and_a_done_marker_are_handled() {
    // One event whose JSON is spread over two `data:` lines, then [DONE].
    let payload = "data: {\"choices\":[{\"index\":0,\ndata: \"delta\":{\"content\":\"joined\"}}]}\n\ndata: [DONE]\n\n".to_string();
    let (backend, server) = serve(payload, 5).await;
    let (text, deltas) = collect(&backend).await;
    server.await.unwrap();

    assert_eq!(text.unwrap(), "joined");
    assert_eq!(deltas, vec![StreamDelta::Token("joined".into())]);
}

#[tokio::test]
async fn an_error_event_and_an_abrupt_end_are_errors() {
    let payload =
        "data: {\"error\":{\"message\":\"boom\",\"type\":\"server_error\"}}\n\n".to_string();
    let (backend, server) = serve(payload, 3).await;
    let (text, _) = collect(&backend).await;
    server.await.unwrap();
    let error = text.unwrap_err();
    assert!(error.contains("boom"), "{error}");

    // A complete frame, then a frame the server never finishes.
    let payload = format!("{}data: {{\"choices\":[{{\"index\":0,", text_frame("hello"));
    let (backend, server) = serve(payload, 4).await;
    let (text, deltas) = collect(&backend).await;
    server.await.unwrap();
    let error = text.unwrap_err();
    assert!(
        error.contains("hello"),
        "the received prefix travels with the error: {error}"
    );
    assert_eq!(deltas, vec![StreamDelta::Token("hello".into())]);
}

#[tokio::test]
async fn a_streamed_response_carries_usage_and_tool_calls() {
    let payload = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4,\"total_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let (backend, server) = serve(payload, 11).await;

    let mut deltas = Vec::new();
    let response = backend
        .stream("gpt-4o-mini", request(), None, &mut |delta| {
            deltas.push(delta)
        })
        .await
        .unwrap();
    server.await.unwrap();

    assert!(deltas.is_empty(), "tool calls are not text deltas");
    let calls = response.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].fn_name, "read_file");
    assert_eq!(calls[0].fn_arguments["path"], "a.rs");
    assert_eq!(response.usage.total_tokens, Some(7));
}

/// The reported gateway bug, replayed against the real Anthropic adapter: a
/// `input_json_delta` for a stopped `tool_use` block arrives while a text block
/// is open, and the whitespace before it is the only output.
#[tokio::test]
async fn a_bad_anthropic_chunk_arrives_as_a_readable_error_after_its_prefix() {
    let payload = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"\\n\\n\\n\\n\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
    )
    .to_string();
    let (backend, server) = serve(payload, 7).await;

    let mut deltas = Vec::new();
    let error = backend
        .stream("claude-sonnet-4-6", request(), None, &mut |delta| {
            deltas.push(delta)
        })
        .await
        .unwrap_err()
        .to_string();
    server.await.unwrap();

    assert_eq!(deltas, vec![StreamDelta::Token("\n\n\n\n".to_string())]);
    assert!(error.contains("cannot parse"), "{error}");
    assert!(
        !error.contains("JSON value extension error"),
        "the JSON pointer stays out of the transcript: {error}"
    );
}

/// The raw error is a JSON pointer: it names nothing the user can act on, so
/// the adapter's parse failure is rewritten and the cause stays in the log.
#[test]
fn an_unparseable_stream_chunk_reads_as_a_message_not_a_json_pointer() {
    let serde_error = serde_json::from_str::<serde_json::Value>("nope").unwrap_err();
    let error: super::BackendError = genai::Error::StreamParse {
        model_iden: genai::ModelIden::new(genai::adapter::AdapterKind::Anthropic, "m"),
        serde_error,
    }
    .into();

    let mapped = super::readable_stream_error(error).to_string();
    assert!(mapped.contains("cannot parse"), "{mapped}");
    assert!(mapped.contains("Retry the turn"), "{mapped}");
    assert!(
        !mapped.contains("Failed to parse stream data"),
        "the JSON pointer stays out of the transcript: {mapped}"
    );
}

#[test]
fn an_ordinary_stream_error_is_passed_through() {
    let error: super::BackendError = std::io::Error::other("connection reset").into();
    assert_eq!(
        super::readable_stream_error(error).to_string(),
        "connection reset"
    );
}
