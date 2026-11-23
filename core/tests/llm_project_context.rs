use std::time::Duration;

use silent_ai_core::Config;
use silent_ai_core::llm::{LlmClient, LlmRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn project_context_is_sent_to_the_llm() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let (request_tx, request_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4_096];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut chunk))
                .await
                .expect("request timeout")
                .expect("request read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request);
            if text.contains("Latest utterance") && text.contains("fn main()") {
                break;
            }
        }
        let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());

        let response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: text/event-stream\r\n",
            "Connection: close\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });

    let mut config = Config::default().llm;
    config.base_url = format!("http://{address}/v1");
    config.model = "mock".to_owned();
    config.api_key_env.clear();
    config.api_key_file = None;
    config.warmup = false;
    config.timeout_ms = 5_000;
    let client = LlmClient::new(config).expect("client");
    let (tx, _rx) = mpsc::unbounded_channel();
    client.spawn(
        LlmRequest {
            id: 9,
            question: "What does main do?".to_owned(),
            transcript: "The teacher asked about the demo project.".to_owned(),
            project: "Project folder: demo\n--- src/main.rs ---\nfn main() {}".to_owned(),
            speech_end_epoch_ms: 0,
        },
        tx,
        CancellationToken::new(),
    );

    let request = request_rx.await.expect("captured request");
    assert!(request.contains("Project folder: demo"), "{request}");
    assert!(request.contains("fn main()"), "{request}");
    assert!(request.contains("What does main do?"), "{request}");
}
