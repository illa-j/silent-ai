#![cfg(feature = "test-support")]

use std::time::Duration;

use silent_ai_core::llm::{LlmClient, LlmEvent, LlmRequest};
use silent_ai_core::mock_llm::MockLlm;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn no_question_is_never_rendered() {
    let mock = MockLlm::spawn("NO_QUESTION").await.expect("mock LLM");
    let mut config = silent_ai_core::Config::default().llm;
    config.base_url = mock.base_url.clone();
    config.model = "mock".to_owned();
    config.api_key_env.clear();
    config.warmup = false;
    let client = LlmClient::new(config).expect("LLM client");
    let (tx, mut rx) = mpsc::unbounded_channel();
    client.spawn(
        LlmRequest {
            id: 7,
            question: "Can you hear me?".to_owned(),
            transcript: String::new(),
            project: String::new(),
            speech_end_epoch_ms: 0,
        },
        tx,
        CancellationToken::new(),
    );

    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        match rx.recv().await {
            Some(LlmEvent::NoQuestion { request_id }) => Ok(request_id),
            Some(LlmEvent::Token { .. }) => Err("NO_QUESTION leaked a token".to_owned()),
            Some(LlmEvent::Error { message, .. }) => Err(message),
            Some(LlmEvent::Done { .. }) => Err("unexpected Done".to_owned()),
            Some(LlmEvent::FirstToken { .. }) => Err("unexpected FirstToken".to_owned()),
            None => Err("event stream closed".to_owned()),
        }
    })
    .await
    .expect("LLM timeout");

    assert_eq!(outcome.expect("no-question outcome"), 7);
    mock.shutdown().await;
}
