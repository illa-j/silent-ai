//! A tiny OpenAI-compatible SSE server used by the benchmark and the
//! end-to-end test. Enabled only with the `test-support` feature.

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

pub struct MockLlm {
    pub base_url: String,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl MockLlm {
    pub async fn spawn(answer: impl Into<String>) -> Result<Self> {
        Self::spawn_with_delay(answer, Duration::from_millis(0)).await
    }

    /// Adds a delay before the first content token, to simulate a real
    /// provider's time-to-first-token while keeping SSE framing realistic.
    pub async fn spawn_with_delay(
        answer: impl Into<String>,
        first_token_delay: Duration,
    ) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{addr}/v1");
        let cancel = CancellationToken::new();
        let thread_cancel = cancel.clone();
        let answer = answer.into();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = thread_cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut socket, _)) = accepted else {
                            break;
                        };
                        let answer = answer.clone();
                        let first_token_delay = first_token_delay;
                        tokio::spawn(async move {
                            let mut request = vec![0u8; 16 * 1024];
                            let mut read = 0usize;
                            loop {
                                if read >= request.len() {
                                    break;
                                }
                                match socket.read(&mut request[read..]).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        read += n;
                                        if request[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                    }
                                    Err(_) => return,
                                }
                            }
                            let head = String::from_utf8_lossy(&request[..read]);
                            let is_models = head.starts_with("GET /v1/models");
                            if is_models {
                                let body = r#"{"object":"list","data":[]}"#;
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                let _ = socket.write_all(response.as_bytes()).await;
                                return;
                            }
                            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
                            if socket.write_all(response.as_bytes()).await.is_err() {
                                return;
                            }
                            let role = json!({"choices":[{"delta":{"role":"assistant"}}]});
                            let _ = socket
                                .write_all(format!("data: {role}\n\n").as_bytes())
                                .await;
                            if !first_token_delay.is_zero() {
                                tokio::time::sleep(first_token_delay).await;
                            }
                            for word in answer.split_inclusive(' ') {
                                let chunk = json!({"choices":[{"delta":{"content": word}}]});
                                if socket
                                    .write_all(format!("data: {chunk}\n\n").as_bytes())
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                let _ = socket.flush().await;
                                tokio::time::sleep(Duration::from_millis(12)).await;
                            }
                            let _ = socket.write_all(b"data: [DONE]\n\n").await;
                            let _ = socket.flush().await;
                            let _ = socket.shutdown().await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            base_url,
            cancel,
            task,
        })
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}
