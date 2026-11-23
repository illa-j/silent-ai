use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use webrtc_vad::{SampleRate, Vad, VadMode};

use crate::audio::{AudioFrame, FRAME_SAMPLES, TARGET_RATE, frame_duration_ms};
use crate::config::{VadConfig, WebrtcVadMode};
use crate::error::{Result, SilentError};

#[derive(Debug, Clone)]
pub enum VadEvent {
    /// First voiced frame of a new phrase.
    SpeechStarted { at: Instant },
    /// Snapshot of the phrase so far for speculative transcription.
    Partial { audio: Vec<f32>, at: Instant },
    /// Silence exceeded the endpoint threshold. `speech_end` is the estimated
    /// end of the last voiced frame, not the time the decision was made.
    Endpoint {
        audio: Vec<f32>,
        speech_end: Instant,
        at: Instant,
    },
}

pub struct Segmenter {
    vad: Vad,
    config: VadConfig,
    pre_roll: VecDeque<f32>,
    pre_roll_samples: usize,
    speech: Vec<f32>,
    pcm: Vec<i16>,
    in_speech: bool,
    voiced_ms: u32,
    silence_ms: u32,
    last_voice_end: Option<Instant>,
    last_voice_sample: usize,
    last_partial_at: Option<Instant>,
    frame_samples: usize,
}

impl Segmenter {
    pub fn new(config: VadConfig) -> Result<Self> {
        let mode = match config.webrtc_mode {
            WebrtcVadMode::Quality => VadMode::Quality,
            WebrtcVadMode::LowBitrate => VadMode::LowBitrate,
            WebrtcVadMode::Aggressive => VadMode::Aggressive,
            WebrtcVadMode::VeryAggressive => VadMode::VeryAggressive,
        };
        let vad = Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, mode);
        let pre_roll_samples = (TARGET_RATE as usize * config.pre_roll_ms as usize) / 1_000;

        Ok(Self {
            vad,
            config,
            pre_roll: VecDeque::with_capacity(pre_roll_samples + FRAME_SAMPLES),
            pre_roll_samples,
            speech: Vec::with_capacity(TARGET_RATE as usize * 12),
            pcm: vec![0_i16; FRAME_SAMPLES],
            in_speech: false,
            voiced_ms: 0,
            silence_ms: 0,
            last_voice_end: None,
            last_voice_sample: 0,
            last_partial_at: None,
            frame_samples: FRAME_SAMPLES,
        })
    }

    pub fn push(&mut self, frame: &AudioFrame) -> Vec<VadEvent> {
        let mut events = Vec::new();
        if frame.samples.is_empty() {
            return events;
        }

        // The pipeline emits 320-sample frames. Splitting here keeps the
        // segmenter correct if a test source uses another chunk size.
        let mut offset = 0;
        while offset < frame.samples.len() {
            let take = (frame.samples.len() - offset).min(self.frame_samples);
            let chunk = &frame.samples[offset..offset + take];
            let at = frame.captured_at
                + Duration::from_millis((offset as u64 * 1_000) / TARGET_RATE as u64);
            let mut padded = chunk.to_vec();
            if padded.len() < self.frame_samples {
                padded.resize(self.frame_samples, 0.0);
            }
            self.process_frame(&mut padded, at, &mut events);
            offset += take;
        }

        events
    }

    fn process_frame(&mut self, samples: &mut [f32], at: Instant, events: &mut Vec<VadEvent>) {
        debug_assert_eq!(samples.len(), self.frame_samples);
        if samples.len() != self.frame_samples {
            return;
        }

        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        let dbfs = 20.0 * (rms.max(1e-9)).log10();
        let voiced = if dbfs < self.config.min_rms_dbfs {
            false
        } else {
            for (dst, src) in self.pcm.iter_mut().zip(samples.iter()) {
                *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            }
            let webrtc = self.vad.is_voice_segment(&self.pcm).unwrap_or(false);
            let energy_fallback =
                self.config.energy_fallback_dbfs != 0.0 && dbfs >= self.config.energy_fallback_dbfs;
            webrtc || energy_fallback
        };
        let frame_ms = frame_duration_ms() as u32;
        let frame_end = at + Duration::from_millis(frame_ms as u64);
        tracing::trace!(dbfs, voiced, in_speech = self.in_speech, "VAD frame");

        if !self.in_speech {
            self.pre_roll.extend(samples.iter().copied());
            while self.pre_roll.len() > self.pre_roll_samples {
                self.pre_roll.pop_front();
            }
            if !voiced {
                return;
            }

            self.in_speech = true;
            self.voiced_ms = frame_ms;
            self.silence_ms = 0;
            self.last_voice_end = Some(frame_end);
            self.speech.clear();
            self.speech.extend(self.pre_roll.drain(..));
            self.last_voice_sample = self.speech.len();
            self.last_partial_at = Some(at);
            events.push(VadEvent::SpeechStarted { at });
            return;
        }

        self.speech.extend(samples.iter().copied());

        if voiced {
            self.voiced_ms = self.voiced_ms.saturating_add(frame_ms);
            self.silence_ms = 0;
            self.last_voice_end = Some(frame_end);
            self.last_voice_sample = self.speech.len();
        } else {
            self.silence_ms = self.silence_ms.saturating_add(frame_ms);
        }

        let should_end = self.silence_ms >= self.config.silence_ms;
        let too_long = self.speech.len()
            >= (TARGET_RATE as usize * self.config.max_segment_ms as usize) / 1_000;

        if should_end || too_long {
            if self.voiced_ms >= self.config.min_speech_ms {
                let end_at = self.last_voice_end.unwrap_or(at);
                let audio = self.trimmed_audio();
                events.push(VadEvent::Endpoint {
                    audio,
                    speech_end: end_at,
                    at,
                });
            }
            self.reset();
            return;
        }

        let partial_due = self.voiced_ms >= self.config.partial_min_speech_ms
            && self.last_partial_at.is_none_or(|last| {
                at.duration_since(last).as_millis() as u64 >= self.config.partial_interval_ms as u64
            });
        if partial_due {
            self.last_partial_at = Some(at);
            let audio = self.partial_audio();
            if audio.len() >= FRAME_SAMPLES {
                events.push(VadEvent::Partial { audio, at });
            }
        }
    }

    fn trimmed_audio(&self) -> Vec<f32> {
        // Keep 200 ms after the last voiced frame: Whisper needs the tail of
        // the final phoneme, otherwise the last word is truncated.
        let tail_samples = (TARGET_RATE as usize * 200) / 1_000;
        let cut = self
            .last_voice_sample
            .saturating_add(tail_samples)
            .min(self.speech.len());
        self.speech[..cut].to_vec()
    }

    fn partial_audio(&self) -> Vec<f32> {
        let window = (TARGET_RATE as usize * self.config.partial_window_ms as usize) / 1_000;
        let cut = self.last_voice_sample.min(self.speech.len());
        let start = cut.saturating_sub(window);
        self.speech[start..cut].to_vec()
    }

    pub fn reset_state(&mut self) {
        self.reset();
    }

    fn reset(&mut self) {
        self.in_speech = false;
        self.voiced_ms = 0;
        self.silence_ms = 0;
        self.last_voice_end = None;
        self.last_voice_sample = 0;
        self.last_partial_at = None;
        self.speech.clear();
        self.pre_roll.clear();
        self.vad.reset();
    }

    /// End the stream: emit a final phrase if one is in progress.
    pub fn flush(&mut self) -> Option<VadEvent> {
        if !self.in_speech || self.voiced_ms < self.config.min_speech_ms {
            self.reset();
            return None;
        }
        let speech_end = self.last_voice_end.unwrap_or_else(Instant::now);
        let event = VadEvent::Endpoint {
            audio: self.trimmed_audio(),
            speech_end,
            at: Instant::now(),
        };
        self.reset();
        Some(event)
    }
}

fn feed_frame(
    segmenter: &mut Segmenter,
    frame: &AudioFrame,
    events: &mpsc::Sender<VadEvent>,
) -> bool {
    for event in segmenter.push(frame) {
        let kind = match &event {
            VadEvent::SpeechStarted { .. } => "speech_started",
            VadEvent::Partial { .. } => "partial",
            VadEvent::Endpoint { .. } => "endpoint",
        };
        tracing::debug!(kind, "VAD event");
        if events.blocking_send(event).is_err() {
            tracing::debug!("VAD event receiver closed");
            return false;
        }
    }
    true
}

/// VAD runs on its own thread because `webrtc-vad` owns a C pointer and is
/// not `Send`; keeping it out of the async runtime also guarantees that a
/// slow VAD decision can never stall a Tokio worker.
pub fn spawn_vad(
    mut frames: mpsc::Receiver<AudioFrame>,
    config: VadConfig,
    events: mpsc::Sender<VadEvent>,
    cancel: CancellationToken,
    paused: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("silent-vad".to_owned())
        .spawn(move || {
            let mut segmenter = match Segmenter::new(config) {
                Ok(segmenter) => segmenter,
                Err(err) => {
                    tracing::error!(%err, "failed to initialize VAD");
                    return;
                }
            };
            let mut last_real_frame = Instant::now();
            let mut last_synthetic = Instant::now()
                .checked_sub(Duration::from_millis(20))
                .unwrap_or_else(Instant::now);
            let mut synthetic_sequence = 0_u64;

            loop {
                if cancel.is_cancelled() {
                    break;
                }
                match frames.try_recv() {
                    Ok(frame) => {
                        last_real_frame = Instant::now();
                        tracing::trace!(
                            sequence = frame.sequence,
                            samples = frame.samples.len(),
                            "VAD received frame"
                        );
                        if paused.load(Ordering::Relaxed) {
                            segmenter.reset_state();
                            continue;
                        }
                        if !feed_frame(&mut segmenter, &frame, &events) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        // PulseAudio monitor sources can stop delivering
                        // callbacks when the sink goes idle. Without synthetic
                        // silence the segmenter stays in_speech forever and the
                        // UI keeps a partial but never gets a final. Feed
                        // silence at real time once a short gap is detected.
                        if last_real_frame.elapsed() >= Duration::from_millis(120) {
                            let now = Instant::now();
                            if now.duration_since(last_synthetic) >= Duration::from_millis(20) {
                                last_synthetic = now;
                                synthetic_sequence += 1;
                                let frame = AudioFrame {
                                    samples: vec![0.0; FRAME_SAMPLES],
                                    captured_at: now,
                                    sequence: synthetic_sequence,
                                };
                                if paused.load(Ordering::Relaxed) {
                                    segmenter.reset_state();
                                } else if !feed_frame(&mut segmenter, &frame, &events) {
                                    return;
                                }
                            }
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(TryRecvError::Disconnected) => {
                        if let Some(event) = segmenter.flush() {
                            let _ = events.blocking_send(event);
                        }
                        tracing::debug!("audio stream ended; VAD stopped");
                        return;
                    }
                }
            }

            if let Some(event) = segmenter.flush() {
                let _ = events.blocking_send(event);
            }
            tracing::debug!("VAD thread cancelled");
        })
        .map_err(|e| {
            anyhow::Error::from(SilentError::Audio(format!(
                "failed to spawn VAD thread: {e}"
            )))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(amplitude: f32, voiced: bool, seq: u64) -> AudioFrame {
        // A 220 Hz tone is a reliable speech-like input for WebRTC VAD;
        // silence is exact zeros.
        let samples = (0..FRAME_SAMPLES)
            .map(|i| {
                if voiced {
                    let t = (i as f32 + seq as f32 * FRAME_SAMPLES as f32) / TARGET_RATE as f32;
                    amplitude * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                } else {
                    0.0
                }
            })
            .collect();
        AudioFrame {
            samples,
            captured_at: Instant::now(),
            sequence: seq,
        }
    }

    #[test]
    fn silence_never_starts_a_segment() {
        let mut segmenter = Segmenter::new(VadConfig::default()).expect("vad");
        for seq in 0..50 {
            assert!(segmenter.push(&frame(0.0, false, seq)).is_empty());
        }
    }

    #[test]
    fn stalled_audio_source_is_finalized_with_synthetic_silence() {
        let (frame_tx, frame_rx) = mpsc::channel(64);
        let (event_tx, mut event_rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let paused = Arc::new(AtomicBool::new(false));
        let handle = spawn_vad(
            frame_rx,
            VadConfig::default(),
            event_tx,
            cancel.clone(),
            paused,
        )
        .expect("spawn VAD");

        // A voiced phrase, then the monitor stops delivering callbacks.
        for seq in 0..30 {
            frame_tx
                .try_send(frame(0.5, true, seq))
                .expect("voiced frame");
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut endpoint = false;
        while Instant::now() < deadline {
            match event_rx.try_recv() {
                Ok(VadEvent::Endpoint { .. }) => {
                    endpoint = true;
                    break;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(20)),
                Err(TryRecvError::Disconnected) => break,
            }
        }

        cancel.cancel();
        drop(frame_tx);
        let _ = handle.join();
        assert!(endpoint, "VAD must finalize when the audio source stalls");
    }

    #[test]
    fn fixture_produces_an_endpoint() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bench/fixtures/sample_question.wav");
        let mut reader = match hound::WavReader::open(&path) {
            Ok(reader) => reader,
            Err(err) => {
                eprintln!("fixture unavailable: {err}");
                return;
            }
        };
        let samples: Vec<f32> = match reader
            .samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()
        {
            Ok(samples) => samples.into_iter().map(|s| s as f32 / 32768.0).collect(),
            Err(err) => {
                eprintln!("fixture read failed: {err}");
                return;
            }
        };
        let config = VadConfig {
            webrtc_mode: WebrtcVadMode::Quality,
            ..VadConfig::default()
        };
        let mut segmenter = Segmenter::new(config).expect("vad");
        let started = Instant::now();
        let mut events = Vec::new();
        for (index, chunk) in samples.chunks(FRAME_SAMPLES).enumerate() {
            let mut samples = chunk.to_vec();
            samples.resize(FRAME_SAMPLES, 0.0);
            let frame = AudioFrame {
                samples,
                captured_at: started + Duration::from_millis(index as u64 * 20),
                sequence: index as u64,
            };
            events.extend(segmenter.push(&frame));
        }
        if let Some(event) = segmenter.flush() {
            events.push(event);
        }
        eprintln!("events: {events:?}");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, VadEvent::Endpoint { .. })),
            "fixture must produce an endpoint"
        );
    }

    #[test]
    fn flush_without_speech_is_none() {
        let mut segmenter = Segmenter::new(VadConfig::default()).expect("vad");
        for seq in 0..25 {
            let _ = segmenter.push(&frame(0.0, false, seq));
        }
        assert!(segmenter.flush().is_none());
    }
}
