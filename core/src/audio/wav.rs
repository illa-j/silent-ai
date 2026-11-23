use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    AudioFrame, AudioGuard, AudioSession, AudioSource, AudioStream, CaptureInfo, FRAME_SAMPLES,
    TARGET_RATE, resample_to_16k,
};
use crate::config::AudioMode;
use crate::error::{Result, SilentError};

/// Feeds a WAV file through the exact same VAD/STT/LLM pipeline as live audio,
/// paced in real time. Used by the benchmark and the end-to-end test.
pub struct WavSource {
    path: PathBuf,
    speed: f64,
    add_padding: bool,
}

impl WavSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            speed: 1.0,
            add_padding: true,
        }
    }

    pub fn with_speed(mut self, speed: f64) -> Self {
        self.speed = speed.clamp(0.25, 20.0);
        self
    }

    pub fn without_padding(mut self) -> Self {
        self.add_padding = false;
        self
    }
}

impl AudioSource for WavSource {
    fn start(&mut self) -> Result<AudioSession> {
        let (mut samples, rate) = load_wav_mono(&self.path)?;
        samples = resample_to_16k(&samples, rate)?;
        if self.add_padding {
            let mut padded = vec![0.0_f32; (TARGET_RATE as usize * 300) / 1_000];
            padded.extend(samples);
            padded.extend(vec![0.0_f32; (TARGET_RATE as usize * 800) / 1_000]);
            samples = padded;
        }
        if samples.is_empty() {
            return Err(anyhow::Error::from(SilentError::Audio(
                "WAV file is empty after resampling".to_owned(),
            )));
        }

        let (tx, rx) = mpsc::channel::<AudioFrame>(64);
        let stop = CancellationToken::new();
        let thread_stop = stop.clone();
        let speed = self.speed;
        let frame_duration = Duration::from_secs_f64(0.020 / speed);
        let duration_ms = (samples.len() as u64 * 1_000) / TARGET_RATE as u64;
        let path = self.path.clone();

        let handle = std::thread::Builder::new()
            .name("silent-wav-source".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let mut sequence = 0u64;
                for chunk in samples.chunks(FRAME_SAMPLES) {
                    if thread_stop.is_cancelled() {
                        break;
                    }
                    let mut frame_samples = chunk.to_vec();
                    if frame_samples.len() < FRAME_SAMPLES {
                        frame_samples.resize(FRAME_SAMPLES, 0.0);
                    }
                    sequence = sequence.wrapping_add(1);
                    let frame = AudioFrame {
                        samples: frame_samples,
                        captured_at: Instant::now(),
                        sequence,
                    };
                    if tx.blocking_send(frame).is_err() {
                        break;
                    }
                    let target = started + frame_duration.mul_f64(sequence as f64);
                    let now = Instant::now();
                    if target > now {
                        std::thread::sleep(target - now);
                    }
                }
                tracing::info!(file = %path.display(), duration_ms, "WAV source finished");
            })
            .map_err(|e| {
                anyhow::Error::from(SilentError::Audio(format!(
                    "failed to spawn WAV source thread: {e}"
                )))
            })?;

        Ok(AudioSession {
            stream: AudioStream {
                info: CaptureInfo {
                    host: "wav".to_owned(),
                    device: self.path.display().to_string(),
                    device_id: self.path.display().to_string(),
                    mode: AudioMode::System,
                    sample_rate: TARGET_RATE,
                    channels: 1,
                    loopback: false,
                },
                frames: rx,
            },
            guard: AudioGuard::from_threads(stop, vec![handle]),
        })
    }

    fn description(&self) -> String {
        format!("WAV file {}", self.path.display())
    }
}

fn load_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let reader = hound::WavReader::open(path).map_err(|e| {
        anyhow::Error::from(SilentError::Audio(format!(
            "failed to open WAV {}: {e}",
            path.display()
        )))
    })?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut mono = Vec::new();

    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => {
            let mut reader = reader;
            for sample in reader.samples::<f32>() {
                let sample = sample.map_err(|e| {
                    anyhow::Error::from(SilentError::Audio(format!(
                        "failed to read WAV {}: {e}",
                        path.display()
                    )))
                })?;
                mono.push(sample);
            }
        }
        (hound::SampleFormat::Int, 16) => {
            let mut reader = reader;
            for sample in reader.samples::<i16>() {
                let sample = sample.map_err(|e| {
                    anyhow::Error::from(SilentError::Audio(format!(
                        "failed to read WAV {}: {e}",
                        path.display()
                    )))
                })?;
                mono.push(sample as f32 / 32_768.0);
            }
        }
        (hound::SampleFormat::Int, 24 | 32) => {
            let mut reader = reader;
            for sample in reader.samples::<i32>() {
                let sample = sample.map_err(|e| {
                    anyhow::Error::from(SilentError::Audio(format!(
                        "failed to read WAV {}: {e}",
                        path.display()
                    )))
                })?;
                mono.push(sample as f32 / 2_147_483_648.0);
            }
        }
        (format, bits) => {
            return Err(anyhow::Error::from(SilentError::Audio(format!(
                "unsupported WAV format {format:?}/{bits}-bit; convert with \
                 `ffmpeg -i input.wav -ar 16000 -ac 1 -c:a pcm_s16le output.wav`"
            ))));
        }
    }

    if channels == 1 {
        return Ok((mono, spec.sample_rate));
    }

    let mut downmixed = Vec::with_capacity(mono.len() / channels);
    for frame in mono.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().sum();
        downmixed.push(sum / channels as f32);
    }
    Ok((downmixed, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_file() {
        let source = WavSource::new("/definitely/missing.wav");
        let mut source = source;
        assert!(source.start().is_err());
    }
}
