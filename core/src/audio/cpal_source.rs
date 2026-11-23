use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::devices::{CapturePlan, resolve_plan};
use super::{
    AudioFrame, AudioGuard, AudioSession, AudioSource, AudioStream, CaptureInfo, FRAME_SAMPLES,
    TARGET_RATE,
};
use crate::config::AudioConfig;
use crate::error::{Result, SilentError};

pub struct CpalSource {
    config: AudioConfig,
}

impl CpalSource {
    pub fn new(config: AudioConfig) -> Self {
        Self { config }
    }
}

impl AudioSource for CpalSource {
    fn start(&mut self) -> Result<AudioSession> {
        let plan = resolve_plan(&self.config)?;

        // cpal's PulseAudio backend can stop receiving callbacks after the
        // monitor source is suspended by the server, which leaves the app
        // silently "listening" forever. `parec` keeps the recording stream
        // alive across sink suspend/resume, so prefer it for Linux monitor
        // sources and fall back to cpal if parec is unavailable.
        #[cfg(target_os = "linux")]
        if plan.loopback && plan.device_id_string.starts_with("pulseaudio:") {
            match super::parec::start(&plan) {
                Ok(session) => return Ok(session),
                Err(err) => tracing::warn!(
                    %err,
                    "parec capture unavailable; falling back to cpal"
                ),
            }
        }

        start_capture(plan, &self.config)
    }

    fn description(&self) -> String {
        format!("cpal system audio ({:?})", self.config.mode)
    }
}

fn start_capture(plan: CapturePlan, config: &AudioConfig) -> Result<AudioSession> {
    let rate = plan.config.sample_rate;
    let channels = plan.config.channels;
    let capacity = (rate as usize)
        .saturating_mul(channels as usize)
        .saturating_mul(config.ring_buffer_ms.max(500) as usize)
        / 1_000;
    let capacity = capacity.max(8_192);
    let rb = HeapRb::<f32>::new(capacity);
    let (producer, consumer) = rb.split();
    let dropped = Arc::new(AtomicU64::new(0));
    let stop = CancellationToken::new();

    let info = CaptureInfo {
        host: plan.host_id.to_string(),
        device: plan.device_name.clone(),
        device_id: plan.device_id_string.clone(),
        mode: plan.mode,
        sample_rate: rate,
        channels,
        loopback: plan.loopback,
    };

    let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<std::result::Result<(), String>>(1);

    let audio_stop = stop.clone();
    let audio_dropped = dropped.clone();
    let audio_plan = plan.clone_for_thread();
    let audio_config = plan.config;
    let sample_format = plan.sample_format;

    let audio_thread = std::thread::Builder::new()
        .name("silent-audio-cpal".to_owned())
        .spawn(move || {
            run_audio_thread(
                audio_plan,
                audio_config,
                sample_format,
                producer,
                audio_dropped,
                audio_stop,
                init_tx,
            );
        })
        .map_err(|e| {
            anyhow::Error::from(SilentError::Audio(format!(
                "failed to spawn audio thread: {e}"
            )))
        })?;

    match init_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            stop.cancel();
            let _ = audio_thread.join();
            return Err(anyhow::Error::from(SilentError::Audio(message)));
        }
        Err(err) => {
            stop.cancel();
            let _ = audio_thread.join();
            return Err(anyhow::Error::from(SilentError::Audio(format!(
                "audio device did not start within 10 s: {err}. Run --list-devices"
            ))));
        }
    }

    let (frame_tx, frame_rx) = mpsc::channel::<AudioFrame>(64);
    let processing_stop = stop.clone();
    let processing_dropped = dropped.clone();
    let processing_info = info.clone();
    let processing_thread = match std::thread::Builder::new()
        .name("silent-audio-resample".to_owned())
        .spawn(move || {
            run_processing_thread(
                consumer,
                rate,
                channels,
                frame_tx,
                processing_stop,
                processing_dropped,
                processing_info,
            );
        }) {
        Ok(handle) => handle,
        Err(e) => {
            stop.cancel();
            let _ = audio_thread.join();
            return Err(anyhow::Error::from(SilentError::Audio(format!(
                "failed to spawn resampler thread: {e}"
            ))));
        }
    };

    tracing::info!(
        host = %info.host,
        device = %info.device,
        device_id = %info.device_id,
        rate,
        channels,
        loopback = info.loopback,
        "audio capture started"
    );

    Ok(AudioSession {
        stream: AudioStream {
            info,
            frames: frame_rx,
        },
        guard: AudioGuard::from_threads(stop, vec![audio_thread, processing_thread]),
    })
}

fn run_audio_thread(
    plan: CapturePlan,
    config: StreamConfig,
    sample_format: SampleFormat,
    producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
    stop: CancellationToken,
    init_tx: std::sync::mpsc::SyncSender<std::result::Result<(), String>>,
) {
    let host = match cpal::host_from_id(plan.host_id) {
        Ok(host) => host,
        Err(err) => {
            let _ = init_tx.send(Err(format!("failed to open audio host: {err}")));
            return;
        }
    };
    let device = match host.device_by_id(&plan.id) {
        Some(device) => device,
        None => {
            let _ = init_tx.send(Err(format!(
                "audio device {} disappeared before the stream started",
                plan.device_name
            )));
            return;
        }
    };

    let stream = match build_stream(&device, &config, sample_format, producer, dropped) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = init_tx.send(Err(format!(
                "failed to build capture stream on {}: {err}. Run --list-devices and check the \
                 system-audio setup for your OS",
                plan.device_name
            )));
            return;
        }
    };

    if let Err(err) = stream.play() {
        let _ = init_tx.send(Err(format!(
            "failed to start capture on {}: {err}",
            plan.device_name
        )));
        return;
    }

    if init_tx.send(Ok(())).is_err() {
        return;
    }

    while !stop.is_cancelled() {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream);
    tracing::debug!("audio callback stopped");
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
) -> std::result::Result<cpal::Stream, String> {
    let result = match format {
        SampleFormat::F32 => build_typed::<f32>(device, config, producer, dropped),
        SampleFormat::I16 => build_typed::<i16>(device, config, producer, dropped),
        SampleFormat::U16 => build_typed::<u16>(device, config, producer, dropped),
        SampleFormat::I32 => build_typed::<i32>(device, config, producer, dropped),
        SampleFormat::I8 => build_typed::<i8>(device, config, producer, dropped),
        SampleFormat::U8 => build_typed::<u8>(device, config, producer, dropped),
        SampleFormat::F64 => build_typed::<f64>(device, config, producer, dropped),
        other => {
            return Err(format!(
                "unsupported sample format {other:?}; choose a device with f32/i16/i32 support"
            ));
        }
    };
    result.map_err(|e| e.to_string())
}

fn build_typed<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
) -> std::result::Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut scratch: Vec<f32> = Vec::with_capacity(8_192);
    let err_fn = |err: cpal::Error| {
        tracing::error!(%err, "audio capture stream error");
    };

    device.build_input_stream(
        *config,
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            if scratch.len() < data.len() {
                scratch.resize(data.len(), 0.0);
            }
            for (dst, src) in scratch.iter_mut().zip(data.iter()) {
                *dst = f32::from_sample(*src);
            }
            let pushed = producer.push_slice(&scratch[..data.len()]);
            if pushed < data.len() {
                dropped.fetch_add((data.len() - pushed) as u64, Ordering::Relaxed);
            }
        },
        err_fn,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_processing_thread(
    mut consumer: HeapCons<f32>,
    input_rate: u32,
    channels: u16,
    frame_tx: mpsc::Sender<AudioFrame>,
    stop: CancellationToken,
    dropped: Arc<AtomicU64>,
    _info: CaptureInfo,
) {
    let channels = channels.max(1) as usize;
    let mut resampler = if input_rate != TARGET_RATE {
        match Fft::<f32>::new(
            input_rate as usize,
            TARGET_RATE as usize,
            960,
            1,
            FixedSync::Input,
        ) {
            Ok(resampler) => Some(resampler),
            Err(err) => {
                tracing::error!(%err, "failed to create resampler");
                return;
            }
        }
    } else {
        None
    };

    let mut read_buf = vec![0.0_f32; 16_384];
    let mut mono_pending: VecDeque<f32> = VecDeque::with_capacity(32_768);
    let mut output_pending: VecDeque<f32> = VecDeque::with_capacity(32_768);
    let mut frame = Vec::with_capacity(FRAME_SAMPLES);
    let mut sequence: u64 = 0;
    let mut resample_input: Vec<f32> = Vec::with_capacity(4096);
    let mut resample_output: Vec<f32> = Vec::with_capacity(8192);

    while !stop.is_cancelled() {
        let mut did_work = false;
        loop {
            let read = consumer.pop_slice(&mut read_buf);
            if read == 0 {
                break;
            }
            did_work = true;
            if channels == 1 {
                mono_pending.extend(read_buf[..read].iter().copied());
            } else {
                for chunk in read_buf[..read].chunks_exact(channels) {
                    let sum: f32 = chunk.iter().copied().sum();
                    mono_pending.push_back(sum / channels as f32);
                }
            }
        }

        if let Some(resampler) = resampler.as_mut() {
            loop {
                let needed = resampler.input_frames_next();
                if mono_pending.len() < needed {
                    break;
                }
                resample_input.clear();
                resample_input.extend(mono_pending.drain(..needed));
                let out_len = resampler.output_frames_next().max(1);
                if resample_output.len() < out_len {
                    resample_output.resize(out_len, 0.0);
                }
                let input_adapter = match InterleavedSlice::new(&resample_input, 1, needed) {
                    Ok(adapter) => adapter,
                    Err(err) => {
                        tracing::error!(%err, "resampler input adapter failed");
                        return;
                    }
                };
                let mut output_adapter =
                    match InterleavedSlice::new_mut(&mut resample_output, 1, out_len) {
                        Ok(adapter) => adapter,
                        Err(err) => {
                            tracing::error!(%err, "resampler output adapter failed");
                            return;
                        }
                    };
                match resampler.process_into_buffer(&input_adapter, &mut output_adapter, None) {
                    Ok((_consumed, produced)) => {
                        output_pending.extend(resample_output[..produced].iter().copied());
                        did_work = true;
                    }
                    Err(err) => {
                        tracing::error!(%err, "resampling failed");
                        return;
                    }
                }
            }
        } else {
            output_pending.extend(mono_pending.drain(..));
        }

        while output_pending.len() >= FRAME_SAMPLES {
            frame.clear();
            frame.extend(output_pending.drain(..FRAME_SAMPLES));
            sequence = sequence.wrapping_add(1);
            let audio_frame = AudioFrame {
                samples: frame.clone(),
                captured_at: Instant::now(),
                sequence,
            };
            if frame_tx.try_send(audio_frame).is_err() {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }

        if !did_work {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    let dropped_total = dropped.load(Ordering::Relaxed);
    if dropped_total > 0 {
        tracing::warn!(
            dropped_samples = dropped_total,
            "audio samples were dropped because a consumer fell behind"
        );
    }
    tracing::debug!("audio processing thread stopped");
}

impl CapturePlan {
    fn clone_for_thread(&self) -> Self {
        Self {
            host_id: self.host_id,
            id: self.id.clone(),
            device_name: self.device_name.clone(),
            device_id_string: self.device_id_string.clone(),
            config: self.config,
            sample_format: self.sample_format,
            loopback: self.loopback,
            mode: self.mode,
        }
    }
}
