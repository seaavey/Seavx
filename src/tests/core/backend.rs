use crate::agent::backend::{ChatBackend, GenaiBackend, StreamDelta};
use genai::chat::{ChatMessage, MessageContent};

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
