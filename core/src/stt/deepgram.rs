use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

use super::CloudSttEvent;
use crate::audio::AudioFrame;
use crate::config::DeepgramConfig;
use crate::error::Result;

#[derive(Debug, Deserialize)]
struct DgMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    is_final: bool,
    #[serde(default)]
    speech_final: bool,
    #[serde(default)]
    channel: Option<DgChannel>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DgChannel {
    #[serde(default)]
    alternatives: Vec<DgAlternative>,
}

#[derive(Debug, Deserialize)]
struct DgAlternative {
    #[serde(default)]
    transcript: String,
}

pub async fn run(
    mut frames: mpsc::Receiver<AudioFrame>,
    config: DeepgramConfig,
    api_key: String,
    events: mpsc::Sender<CloudSttEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut reconnect_delay = Duration::from_millis(config.reconnect_delay_ms.max(100));
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        match session(&mut frames, &config, &api_key, &events, &cancel).await {
            Ok(()) => {
                if cancel.is_cancelled() {
                    return Ok(());
                }
            }
            Err(err) => {
                let _ = events
                    .send(CloudSttEvent::Error(format!("Deepgram: {err}")))
                    .await;
            }
        }

        if cancel.is_cancelled() || frames.is_closed() {
            return Ok(());
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(reconnect_delay) => {}
        }
        reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(10));
    }
}

async fn session(
    frames: &mut mpsc::Receiver<AudioFrame>,
    config: &DeepgramConfig,
    api_key: &str,
    events: &mpsc::Sender<CloudSttEvent>,
    cancel: &CancellationToken,
) -> Result<()> {
    let keyterms = config
        .keyterms
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|term| format!("&keyterm={}", url_encode(term.trim())))
        .collect::<String>();
    let url = format!(
        "wss://api.deepgram.com/v1/listen?encoding=linear16&sample_rate=16000&channels=1\
         &model={}&language={}&smart_format={}&interim_results={}&endpointing={}&vad_events=true{}",
        config.model,
        config.language,
        config.smart_format,
        config.interim_results,
        config.endpointing_ms,
        keyterms
    );

    // Build a proper WebSocket handshake request first (Sec-WebSocket-Key,
    // Upgrade, Connection, Version), then add Deepgram's Authorization header.
    // A bare `http::Request` is rejected by the server.
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| anyhow::anyhow!("invalid Deepgram request: {e}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Token {api_key}")
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid authorization header: {e}"))?,
    );

    let (ws, _) = tokio_tungstenite::connect_async(request).await?;
    let _ = events.send(CloudSttEvent::Connected).await;
    tracing::info!("Deepgram WebSocket connected");

    let (mut write, mut read) = ws.split();
    let mut keepalive = tokio::time::interval(Duration::from_secs(5));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keepalive.tick().await; // consume the immediate first tick

    let mut last_partial = String::new();
    let mut last_final = String::new();
    // Deepgram marks phrase chunks with is_final before speech_final. Keep
    // them together so "Що таке" + "рефлексія?" is one question, not two.
    let mut utterance = String::new();

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    write.send(Message::Text(r#"{"type":"CloseStream"}"#.into())),
                ).await;
                let _ = tokio::time::timeout(Duration::from_millis(500), write.close()).await;
                return Ok(());
            }
            _ = keepalive.tick() => {
                if write.send(Message::Text(r#"{"type":"KeepAlive"}"#.into())).await.is_err() {
                    return Ok(());
                }
            }
            frame = frames.recv() => {
                let Some(frame) = frame else {
                    let _ = tokio::time::timeout(
                        Duration::from_millis(500),
                        write.send(Message::Text(r#"{"type":"CloseStream"}"#.into())),
                    ).await;
                    return Ok(());
                };
                let bytes = pcm16le(&frame.samples);
                if write.send(Message::Binary(bytes.into())).await.is_err() {
                    return Ok(());
                }
            }
            message = read.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                match message? {
                    Message::Text(text) => {
                        if let Err(err) = handle_text(
                            &text,
                            events,
                            &mut last_partial,
                            &mut last_final,
                            &mut utterance,
                        ).await {
                            tracing::debug!(%err, "failed to parse Deepgram message");
                        }
                    }
                    Message::Ping(data) => {
                        let _ = write.send(Message::Pong(data)).await;
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

async fn handle_text(
    text: &str,
    events: &mpsc::Sender<CloudSttEvent>,
    last_partial: &mut String,
    last_final: &mut String,
    utterance: &mut String,
) -> Result<()> {
    let message: DgMessage = serde_json::from_str(text)?;
    match message.kind.as_str() {
        "Results" => {
            let Some(alternative) = message
                .channel
                .as_ref()
                .and_then(|channel| channel.alternatives.first())
            else {
                return Ok(());
            };
            let transcript = alternative.transcript.trim();
            if transcript.is_empty() {
                return Ok(());
            }

            if message.is_final {
                if utterance.is_empty() {
                    utterance.push_str(transcript);
                } else if !utterance.ends_with(transcript) {
                    utterance.push(' ');
                    utterance.push_str(transcript);
                }
                last_partial.clear();
                last_partial.push_str(utterance);

                if message.speech_final {
                    let final_text = utterance.trim().to_owned();
                    utterance.clear();
                    if !final_text.is_empty() && final_text != *last_final {
                        last_final.clear();
                        last_final.push_str(&final_text);
                        let _ = events
                            .send(CloudSttEvent::Final {
                                text: final_text,
                                received_at: std::time::Instant::now(),
                            })
                            .await;
                    }
                } else {
                    let _ = events
                        .send(CloudSttEvent::Partial {
                            text: utterance.clone(),
                            received_at: std::time::Instant::now(),
                        })
                        .await;
                }
            } else {
                let full = if utterance.is_empty() {
                    transcript.to_owned()
                } else if utterance.ends_with(transcript) {
                    utterance.clone()
                } else {
                    format!("{utterance} {transcript}")
                };
                last_partial.clear();
                last_partial.push_str(&full);
                let _ = events
                    .send(CloudSttEvent::Partial {
                        text: full,
                        received_at: std::time::Instant::now(),
                    })
                    .await;
            }
        }
        "UtteranceEnd" => {
            let final_text = if !utterance.trim().is_empty() {
                std::mem::take(utterance)
            } else if !last_partial.is_empty() && last_partial != last_final {
                std::mem::take(last_partial)
            } else {
                String::new()
            };
            let final_text = final_text.trim().to_owned();
            if !final_text.is_empty() && final_text != *last_final {
                last_final.clear();
                last_final.push_str(&final_text);
                let _ = events
                    .send(CloudSttEvent::Final {
                        text: final_text,
                        received_at: std::time::Instant::now(),
                    })
                    .await;
            }
            utterance.clear();
            last_partial.clear();
        }
        "Error" => {
            let description = message
                .description
                .unwrap_or_else(|| "unknown Deepgram error".to_owned());
            let _ = events.send(CloudSttEvent::Error(description)).await;
        }
        _ => {}
    }
    Ok(())
}

fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn pcm16le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn joins_phrase_chunks_until_speech_final() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut partial = String::new();
        let mut last_final = String::new();
        let mut utterance = String::new();

        let interim = r#"{"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"Що таке"}]}}"#;
        handle_text(interim, &tx, &mut partial, &mut last_final, &mut utterance)
            .await
            .expect("interim");

        let chunk = r#"{"type":"Results","is_final":true,"speech_final":false,"channel":{"alternatives":[{"transcript":"Що таке"}]}}"#;
        handle_text(chunk, &tx, &mut partial, &mut last_final, &mut utterance)
            .await
            .expect("chunk");

        let last = r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"рефлексія?"}]}}"#;
        handle_text(last, &tx, &mut partial, &mut last_final, &mut utterance)
            .await
            .expect("final");

        let mut finals = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let CloudSttEvent::Final { text, .. } = event {
                finals.push(text);
            }
        }
        assert_eq!(finals, vec!["Що таке рефлексія?".to_owned()]);
    }

    #[test]
    fn pcm_conversion_is_little_endian() {
        let bytes = pcm16le(&[0.0, 1.0, -1.0]);
        assert_eq!(&bytes[0..2], &[0, 0]);
        assert_eq!(&bytes[2..4], &[0xff, 0x7f]);
        assert_eq!(&bytes[4..6], &[0x01, 0x80]);
    }
}
