//! End-to-end WAV → VAD → Whisper → LLM test.
//!
//! Requires a local ggml model. Point `SILENT_AI_TEST_MODEL` at one, e.g.
//! `SILENT_AI_TEST_MODEL=/path/ggml-small-q5_1.bin \
//!  cargo test -p silent-ai-core --features test-support \
//!  --test wav_question -- --nocapture`.
//!
//! Without the variable the test reports a skip instead of downloading a
//! model or failing CI.

#![cfg(feature = "test-support")]

use std::path::PathBuf;
use std::time::Duration;

use silent_ai_core::Config;
use silent_ai_core::audio::wav::WavSource;
use silent_ai_core::mock_llm::MockLlm;
use silent_ai_core::pipeline::{self, Command, PipelineEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wav_question_gets_an_answer() {
    let Some(model) = std::env::var_os("SILENT_AI_TEST_MODEL").map(PathBuf::from) else {
        eprintln!("skip: set SILENT_AI_TEST_MODEL to a ggml Whisper model");
        return;
    };
    if !model.exists() {
        eprintln!(
            "skip: SILENT_AI_TEST_MODEL={} does not exist",
            model.display()
        );
        return;
    }

    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bench/fixtures/sample_question.wav");
    assert!(fixture.exists(), "missing fixture {}", fixture.display());

    let mock = MockLlm::spawn(
        "Because of Rayleigh scattering. Air molecules scatter shorter wavelengths more strongly.",
    )
    .await
    .expect("mock LLM");

    let mut config = Config::default();
    config.stt.whisper.model = model.display().to_string();
    config.stt.whisper.language = "en".to_owned();
    config.stt.whisper.download = false;
    config.stt.whisper.warmup = true;
    config.stt.whisper.adaptive_audio_ctx = true;
    config.stt.whisper.threads = 8;
    config.stt.whisper.gpu = false;
    config.llm.base_url = mock.base_url.clone();
    config.llm.model = "mock".to_owned();
    config.llm.api_key_env.clear();
    config.llm.warmup = false;
    config.llm.timeout_ms = 5_000;

    let (events_tx, mut events_rx) = mpsc::channel(128);
    let (commands_tx, commands_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let mut pipeline = tokio::spawn(pipeline::run(
        config,
        Box::new(WavSource::new(&fixture)),
        events_tx,
        commands_rx,
        cancel.clone(),
        String::new(),
    ));

    let collection = tokio::time::timeout(Duration::from_secs(30), async {
        let mut answer = String::new();
        let mut done = false;
        let mut error = None;
        while let Some(event) = events_rx.recv().await {
            match event {
                PipelineEvent::AnswerDelta { text, .. } => answer.push_str(&text),
                PipelineEvent::AnswerDone {
                    full_text,
                    incomplete,
                    error: stream_error,
                    ..
                } => {
                    if !full_text.is_empty() {
                        answer = full_text;
                    }
                    if incomplete {
                        error = stream_error.or_else(|| Some("incomplete answer".to_owned()));
                    }
                    done = true;
                    break;
                }
                PipelineEvent::Error {
                    fatal: true,
                    message,
                    ..
                } => {
                    error = Some(message);
                    break;
                }
                _ => {}
            }
        }
        (answer, done, error)
    })
    .await;

    let _ = commands_tx.send(Command::Shutdown).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), &mut pipeline).await;
    mock.shutdown().await;

    let (answer, done, error) = collection.expect("pipeline timed out");
    assert!(done, "no AnswerDone event");
    assert!(error.is_none(), "pipeline error: {error:?}");
    assert!(
        answer.to_lowercase().contains("rayleigh"),
        "unexpected answer: {answer:?}"
    );
}
