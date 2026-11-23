use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::error::{Result, SilentError};

#[derive(Debug, Clone, Copy)]
pub struct ModelProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// Downloads a ggml model atomically (`.part` + rename). The final file is
/// checked for a plausible minimum size so a truncated download is never
/// loaded by whisper.cpp.
pub async fn ensure_model(
    path: &Path,
    url: &str,
    allow_download: bool,
    cancel: &CancellationToken,
    mut progress: impl FnMut(ModelProgress),
) -> Result<PathBuf> {
    if let Ok(meta) = tokio::fs::metadata(path).await {
        if meta.len() > 1_000_000 {
            return Ok(path.to_path_buf());
        }
        return Err(anyhow::Error::from(SilentError::Model(format!(
            "{} exists but is too small ({} bytes). Delete it or point stt.whisper.model to a valid ggml file",
            path.display(),
            meta.len()
        ))));
    }

    if !allow_download {
        return Err(anyhow::Error::from(SilentError::Model(format!(
            "model {} is missing and stt.whisper.download = false. Download it from {url}",
            path.display()
        ))));
    }
    if url.trim().is_empty() {
        return Err(anyhow::Error::from(SilentError::Model(format!(
            "model {} is missing and no download URL is available for a custom model path. \
             Put the ggml file there or set stt.whisper.model_url",
            path.display()
        ))));
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| SilentError::io(parent.to_path_buf(), e))?;
    }

    let temp = path.with_extension("bin.part");
    let _ = tokio::fs::remove_file(&temp).await;

    tracing::info!(url, dest = %path.display(), "downloading Whisper model (first run only)");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| anyhow::Error::from(SilentError::Model(format!("HTTP client: {e}"))))?;

    let response = client.get(url).send().await.map_err(|e| {
        anyhow::Error::from(SilentError::Model(format!(
            "failed to download {url}: {e}. Check the network or set stt.whisper.model to a local file"
        )))
    })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let short = body.chars().take(300).collect::<String>();
        return Err(anyhow::Error::from(SilentError::Model(format!(
            "model download returned HTTP {status}: {short}"
        ))));
    }

    let total = response.content_length();
    let mut file = tokio::fs::File::create(&temp)
        .await
        .map_err(|e| SilentError::io(temp.clone(), e))?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0u64;
    let mut last_report = Instant::now();

    progress(ModelProgress { downloaded, total });

    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(anyhow::Error::from(SilentError::Cancelled));
            }
            chunk = stream.next() => chunk,
        };

        match next {
            Some(Ok(chunk)) => {
                file.write_all(&chunk)
                    .await
                    .map_err(|e| SilentError::io(temp.clone(), e))?;
                downloaded += chunk.len() as u64;
                if last_report.elapsed() >= Duration::from_millis(150) {
                    last_report = Instant::now();
                    progress(ModelProgress { downloaded, total });
                }
            }
            Some(Err(err)) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(anyhow::Error::from(SilentError::Model(format!(
                    "model download interrupted after {downloaded} bytes: {err}"
                ))));
            }
            None => break,
        }
    }

    file.flush()
        .await
        .map_err(|e| SilentError::io(temp.clone(), e))?;
    file.sync_all()
        .await
        .map_err(|e| SilentError::io(temp.clone(), e))?;
    drop(file);

    if downloaded < 1_000_000 {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(anyhow::Error::from(SilentError::Model(format!(
            "downloaded model is only {downloaded} bytes; check stt.whisper.model_url"
        ))));
    }

    if path.exists() {
        // Another process won the race; use the existing complete file.
        let _ = tokio::fs::remove_file(&temp).await;
        return Ok(path.to_path_buf());
    }

    tokio::fs::rename(&temp, path)
        .await
        .map_err(|e| SilentError::io(path.to_path_buf(), e))?;
    progress(ModelProgress { downloaded, total });
    tracing::info!(dest = %path.display(), bytes = downloaded, "model ready");
    Ok(path.to_path_buf())
}
