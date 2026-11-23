//! Platform audio capture.
//!
//! The cpal callback only pushes samples into a lock-free ring buffer. A
//! dedicated processing thread downmixes, resamples to 16 kHz mono, and emits
//! 20 ms frames to the async pipeline.

mod cpal_source;
mod devices;
#[cfg(target_os = "linux")]
mod parec;
pub mod wav;

use std::time::Instant;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::AudioMode;
use crate::error::Result;

pub use cpal_source::CpalSource;
pub use devices::{DeviceInfo, format_device_list, list_devices};

/// Whisper and the VAD both consume 16 kHz mono f32.
pub const TARGET_RATE: u32 = 16_000;
/// 20 ms at 16 kHz. WebRTC VAD works with 10/20/30 ms frames.
pub const FRAME_SAMPLES: usize = 320;

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub captured_at: Instant,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureInfo {
    pub host: String,
    pub device: String,
    pub device_id: String,
    pub mode: AudioMode,
    pub sample_rate: u32,
    pub channels: u16,
    pub loopback: bool,
}

#[derive(Debug)]
pub struct AudioStream {
    pub info: CaptureInfo,
    pub frames: mpsc::Receiver<AudioFrame>,
}

/// Owns the audio threads. Cancel it and await `shutdown` for a clean stop.
pub struct AudioGuard {
    stop: CancellationToken,
    handles: Vec<std::thread::JoinHandle<()>>,
    children: Vec<std::process::Child>,
}

impl AudioGuard {
    pub fn from_threads(
        stop: CancellationToken,
        handles: Vec<std::thread::JoinHandle<()>>,
    ) -> Self {
        Self {
            stop,
            handles,
            children: Vec::new(),
        }
    }

    pub fn from_threads_and_children(
        stop: CancellationToken,
        handles: Vec<std::thread::JoinHandle<()>>,
        children: Vec<std::process::Child>,
    ) -> Self {
        Self {
            stop,
            handles,
            children,
        }
    }

    pub fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }

    pub async fn shutdown(self) {
        self.stop.cancel();
        let AudioGuard {
            handles,
            mut children,
            ..
        } = self;
        for child in &mut children {
            let _ = child.kill();
        }
        if handles.is_empty() {
            return;
        }
        let _ = tokio::task::spawn_blocking(move || {
            for handle in handles {
                let _ = handle.join();
            }
        })
        .await;
    }
}

pub struct AudioSession {
    pub stream: AudioStream,
    pub guard: AudioGuard,
}

impl AudioSession {
    pub fn into_parts(self) -> (AudioStream, AudioGuard) {
        (self.stream, self.guard)
    }
}

/// Every capture backend hides behind this trait. `WavSource` in the bench
/// crate implements it too, so the pipeline is identical in tests.
pub trait AudioSource: Send {
    fn start(&mut self) -> Result<AudioSession>;
    fn description(&self) -> String;
}

pub(crate) fn frame_duration_ms() -> u64 {
    (FRAME_SAMPLES as u64 * 1_000) / TARGET_RATE as u64
}

/// Offline resampling for WAV fixtures and tests. The streaming path uses the
/// resampler inside `cpal_source.rs`; this function is allowed to allocate.
pub fn resample_to_16k(input: &[f32], input_rate: u32) -> Result<Vec<f32>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if input_rate == TARGET_RATE {
        return Ok(input.to_vec());
    }
    use rubato::audioadapter_buffers::direct::InterleavedSlice;
    use rubato::{Fft, FixedSync, Resampler};

    let mut resampler = Fft::<f32>::new(
        input_rate as usize,
        TARGET_RATE as usize,
        1_024,
        1,
        FixedSync::Input,
    )
    .map_err(|e| anyhow::anyhow!("failed to create resampler: {e}"))?;
    let adapter = InterleavedSlice::new(input, 1, input.len())
        .map_err(|e| anyhow::anyhow!("invalid resampler input: {e}"))?;
    let output = resampler
        .process_all(&adapter, input.len(), None)
        .map_err(|e| anyhow::anyhow!("resampling failed: {e}"))?;
    Ok(output.take_data())
}
