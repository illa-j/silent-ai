//! PulseAudio capture via `parec` for long-lived monitor streams.
//!
//! The cpal PulseAudio host can stop invoking its callback after the server
//! suspends an idle monitor source. parec is a normal recording client and
//! resumes cleanly when the sink becomes active again.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::devices::CapturePlan;
use super::{
    AudioFrame, AudioGuard, AudioSession, AudioStream, CaptureInfo, FRAME_SAMPLES, TARGET_RATE,
};
use crate::error::{Result, SilentError};

pub(crate) fn start(plan: &CapturePlan) -> Result<AudioSession> {
    let source = plan
        .device_id_string
        .strip_prefix("pulseaudio:")
        .unwrap_or(&plan.device_id_string);

    let mut child = Command::new("parec")
        .arg(format!("--device={source}"))
        .args([
            "--format=float32le",
            "--rate=16000",
            "--channels=1",
            "--raw",
            "--latency-msec=120",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            anyhow::Error::from(SilentError::Audio(format!("failed to start parec: {err}")))
        })?;

    let mut stdout = child.stdout.take().ok_or_else(|| {
        anyhow::Error::from(SilentError::Audio(
            "parec produced no stdout pipe".to_owned(),
        ))
    })?;

    let (frame_tx, frame_rx) = mpsc::channel::<AudioFrame>(64);
    let stop = CancellationToken::new();
    let thread_stop = stop.clone();

    let handle = std::thread::Builder::new()
        .name("silent-audio-parec".to_owned())
        .spawn(move || {
            let base = Instant::now();
            let bytes_per_frame = FRAME_SAMPLES * std::mem::size_of::<f32>();
            let mut pending: Vec<u8> = Vec::with_capacity(bytes_per_frame * 2);
            let mut buffer = [0_u8; 8 * 1024];
            let mut sequence = 0_u64;

            loop {
                if thread_stop.is_cancelled() {
                    break;
                }
                let read = match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(err) => {
                        tracing::warn!(%err, "parec read failed");
                        break;
                    }
                };
                pending.extend_from_slice(&buffer[..read]);

                while pending.len() >= bytes_per_frame {
                    let mut samples = Vec::with_capacity(FRAME_SAMPLES);
                    for chunk in pending[..bytes_per_frame].as_chunks::<4>().0 {
                        samples.push(f32::from_le_bytes(*chunk));
                    }
                    pending.drain(..bytes_per_frame);
                    let frame = AudioFrame {
                        samples,
                        captured_at: base + Duration::from_millis(sequence.saturating_mul(20)),
                        sequence,
                    };
                    sequence = sequence.wrapping_add(1);
                    if frame_tx.blocking_send(frame).is_err() {
                        return;
                    }
                }
            }
        })
        .map_err(|err| {
            anyhow::Error::from(SilentError::Audio(format!(
                "failed to spawn parec reader: {err}"
            )))
        })?;

    let info = CaptureInfo {
        host: "pulseaudio".to_owned(),
        device: plan.device_name.clone(),
        device_id: plan.device_id_string.clone(),
        mode: plan.mode,
        sample_rate: TARGET_RATE,
        channels: 1,
        loopback: true,
    };

    tracing::info!(
        device = %info.device,
        device_id = %info.device_id,
        "audio capture started (parec)"
    );

    Ok(AudioSession {
        stream: AudioStream {
            info,
            frames: frame_rx,
        },
        guard: AudioGuard::from_threads_and_children(stop, vec![handle], vec![child]),
    })
}
