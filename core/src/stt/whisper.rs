use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use super::{JobKind, SttEvent, SttJob, SttResult, clean_transcript};
use crate::config::WhisperConfig;
use crate::error::{Result, SilentError};

pub struct WhisperWorker {
    jobs: Option<mpsc::Sender<SttJob>>,
    preempt: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    cancel: CancellationToken,
}

impl WhisperWorker {
    pub fn spawn(
        model_path: PathBuf,
        config: WhisperConfig,
        events: mpsc::Sender<SttEvent>,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let (job_tx, job_rx) = mpsc::channel::<SttJob>(4);
        let preempt = Arc::new(AtomicBool::new(false));
        let thread_preempt = preempt.clone();
        let thread_cancel = cancel.clone();
        let threads = if config.threads == 0 {
            num_cpus::get_physical().max(1)
        } else {
            config.threads
        };
        let mut config = config;
        config.threads = threads;

        let handle = std::thread::Builder::new()
            .name("silent-whisper".to_owned())
            .spawn(move || {
                run_worker(
                    model_path,
                    config,
                    job_rx,
                    events,
                    thread_cancel,
                    thread_preempt,
                );
            })
            .map_err(|e| {
                anyhow::Error::from(SilentError::Stt(format!(
                    "failed to spawn Whisper thread: {e}"
                )))
            })?;

        Ok(Self {
            jobs: Some(job_tx),
            preempt,
            handle: Some(handle),
            cancel,
        })
    }

    /// Abort a running partial so a queued final can start immediately.
    pub fn preempt(&self) {
        self.preempt.store(true, Ordering::SeqCst);
    }

    pub async fn submit_final(&self, job: SttJob) -> Result<()> {
        self.preempt();
        let jobs = self.jobs.as_ref().ok_or_else(|| {
            anyhow::Error::from(SilentError::Stt("Whisper worker is shut down".to_owned()))
        })?;
        jobs.send(job)
            .await
            .map_err(|_| anyhow::Error::from(SilentError::Stt("Whisper worker stopped".to_owned())))
    }

    pub fn submit_partial(&self, job: SttJob) {
        if let Some(jobs) = self.jobs.as_ref()
            && jobs.try_send(job).is_err()
        {
            tracing::debug!("dropping partial STT job: worker is busy");
        }
    }

    pub async fn shutdown(mut self) {
        tracing::debug!("Whisper shutdown requested");
        self.cancel.cancel();
        let sender = self.jobs.take();
        drop(sender);
        tracing::debug!("Whisper job sender dropped");
        if let Some(handle) = self.handle.take() {
            let _ = tokio::task::spawn_blocking(move || {
                tracing::debug!("joining Whisper thread");
                let _ = handle.join();
                tracing::debug!("Whisper thread joined");
            })
            .await;
        }
        tracing::debug!("Whisper shutdown complete");
    }
}

fn run_worker(
    model_path: PathBuf,
    config: WhisperConfig,
    mut jobs: mpsc::Receiver<SttJob>,
    events: mpsc::Sender<SttEvent>,
    cancel: CancellationToken,
    preempt: Arc<AtomicBool>,
) {
    whisper_rs::install_logging_hooks();
    let _ = events.blocking_send(SttEvent::Loading {
        stage: "loading Whisper model".to_owned(),
    });
    let load_started = Instant::now();

    let gpu_compiled = cfg!(any(
        feature = "cuda",
        feature = "metal",
        feature = "coreml",
        feature = "vulkan"
    ));
    let mut context_params = WhisperContextParameters::default();
    context_params
        .use_gpu(config.gpu && gpu_compiled)
        .flash_attn(config.flash_attn && gpu_compiled);
    let context = match WhisperContext::new_with_params(&model_path, context_params) {
        Ok(context) => context,
        Err(err) => {
            let _ = events.blocking_send(SttEvent::Fatal(format!(
                "failed to load {}: {err}. Delete the file and restart to re-download, or check \
                 that the build has the required GPU runtime",
                model_path.display()
            )));
            return;
        }
    };
    let load_ms = load_started.elapsed().as_millis() as u64;

    let mut state = match context.create_state() {
        Ok(state) => state,
        Err(err) => {
            let _ = events.blocking_send(SttEvent::Fatal(format!(
                "failed to create Whisper state: {err}"
            )));
            return;
        }
    };

    let threads = config.threads.max(1) as i32;
    let warmup_ms = if config.warmup {
        let _ = events.blocking_send(SttEvent::Loading {
            stage: "warming up Whisper".to_owned(),
        });
        let silence = vec![0.0_f32; 16_000];
        let started = Instant::now();
        match run_inference(
            &mut state,
            &silence,
            &config,
            "en",
            JobKind::Final,
            &cancel,
            &Arc::new(AtomicBool::new(false)),
        ) {
            Ok(_) => started.elapsed().as_millis() as u64,
            Err(err) => {
                tracing::warn!(%err, "Whisper warmup failed; first real request will pay the cost");
                0
            }
        }
    } else {
        0
    };

    let model_label = model_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| model_path.display().to_string());
    if events
        .blocking_send(SttEvent::Ready {
            model: model_label,
            load_ms,
            warmup_ms,
        })
        .is_err()
    {
        return;
    }
    tracing::debug!(
        adaptive_audio_ctx = config.adaptive_audio_ctx,
        "Whisper worker ready, waiting for jobs"
    );

    let mut resolved_language = if config.language.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(config.language.clone())
    };
    let mut language_votes: Vec<String> = Vec::new();

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let Some(job) = jobs.blocking_recv() else {
            tracing::debug!("Whisper job channel closed");
            break;
        };
        if cancel.is_cancelled() {
            break;
        }
        tracing::debug!(id = job.id, kind = ?job.kind, samples = job.audio.len(), "Whisper job received");

        if job.audio.len() < 1_600 {
            tracing::debug!(id = job.id, "skipping STT job shorter than 100 ms");
            continue;
        }

        if job.kind == JobKind::Final {
            preempt.store(false, Ordering::SeqCst);
        }

        // Use a detected language for this job even before it is locked. Lock
        // only after two finals agree, so a jingle or a noisy first segment
        // cannot pin the wrong language for the whole lecture.
        let mut job_language = resolved_language.clone();
        if resolved_language.is_none() && job.kind == JobKind::Final {
            match detect_language(&mut state, &job.audio, threads) {
                Ok(Some((language, confidence))) => {
                    tracing::info!(
                        language = %language,
                        confidence,
                        "Whisper language detected"
                    );
                    if job_language.is_none() {
                        job_language = Some(language.clone());
                    }
                    language_votes.push(language.clone());
                    let agreeing = language_votes
                        .iter()
                        .filter(|vote| **vote == language)
                        .count();
                    if agreeing >= 2 {
                        tracing::info!(language = %language, "Whisper language locked");
                        resolved_language = Some(language);
                    }
                }
                Ok(None) => {
                    tracing::debug!(
                        samples = job.audio.len(),
                        "Whisper language detection inconclusive; retrying on a longer final"
                    );
                }
                Err(err) => tracing::warn!(%err, "Whisper language detection failed"),
            }
        }
        let language = job_language.unwrap_or_else(|| "uk".to_owned());

        let started = Instant::now();
        let inference = run_inference(
            &mut state, &job.audio, &config, &language, job.kind, &cancel, &preempt,
        );
        let finished_at = Instant::now();
        let inference_ms = started.elapsed().as_millis() as u64;
        tracing::debug!(
            id = job.id,
            inference_ms,
            ok = inference.is_ok(),
            "Whisper inference finished"
        );

        let text = match inference {
            Ok(text) => text,
            Err(err) => {
                if cancel.is_cancelled() {
                    break;
                }
                if job.kind == JobKind::Partial && preempt.load(Ordering::SeqCst) {
                    continue;
                }
                let _ = events.blocking_send(SttEvent::Error {
                    job_id: Some(job.id),
                    message: format!("Whisper inference failed: {err}"),
                });
                continue;
            }
        };

        if job.kind == JobKind::Partial && preempt.load(Ordering::SeqCst) {
            tracing::debug!(id = job.id, "partial result discarded after final arrived");
            continue;
        }

        let text = apply_glossary(
            &trim_character_loop(&clean_transcript(&text)),
            &config.glossary,
        );
        if !text.is_empty() {
            let result = SttResult {
                id: job.id,
                kind: job.kind,
                text,
                submitted_at: job.submitted_at,
                finished_at,
                inference_ms,
                speech_end: job.speech_end,
            };
            tracing::debug!(
                id = job.id,
                chars = result.text.chars().count(),
                "sending STT result"
            );
            if events.blocking_send(SttEvent::Result(result)).is_err() {
                break;
            }
        }
    }

    tracing::debug!("Whisper worker stopped");
}

fn run_inference(
    state: &mut WhisperState,
    audio: &[f32],
    config: &WhisperConfig,
    language: &str,
    kind: JobKind,
    cancel: &CancellationToken,
    preempt: &Arc<AtomicBool>,
) -> std::result::Result<String, whisper_rs::WhisperError> {
    let strategy = if config.greedy {
        SamplingStrategy::Greedy { best_of: 1 }
    } else {
        SamplingStrategy::BeamSearch {
            beam_size: 1,
            patience: -1.0,
        }
    };
    let threads = config.threads.max(1) as i32;
    let mut params = FullParams::new(strategy);
    params.set_n_threads(threads);
    params.set_language(Some(language));
    if !config.initial_prompt.trim().is_empty() {
        params.set_initial_prompt(config.initial_prompt.trim());
    }
    params.set_translate(false);
    params.set_detect_language(false);
    params.set_no_context(true);
    // Partials stay one segment for latency. Finals may contain several
    // sentences; keep every decoded segment so no words are dropped.
    params.set_single_segment(kind == JobKind::Partial);
    params.set_no_timestamps(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_temperature(0.0);
    params.set_temperature_inc(0.0);
    params.set_max_tokens(if kind == JobKind::Partial {
        config.max_tokens.min(80)
    } else {
        config.max_tokens
    });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_debug_mode(false);

    if config.adaptive_audio_ctx {
        params.set_audio_ctx(audio_ctx_for(audio.len()));
    }

    // whisper-rs 0.16's `set_abort_callback_safe` installs a pointer with the
    // wrong type (Box<dyn FnMut> instead of the concrete closure), which makes
    // whisper.cpp abort every encode. Use the raw callback with a stack-local
    // state that outlives the synchronous `full` call.
    struct AbortState {
        cancel: CancellationToken,
        preempt: Arc<AtomicBool>,
        is_partial: bool,
    }

    unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
        if user_data.is_null() {
            return true;
        }
        let state = unsafe { &*(user_data as *const AbortState) };
        state.cancel.is_cancelled() || (state.is_partial && state.preempt.load(Ordering::Relaxed))
    }

    let mut abort_state = AbortState {
        cancel: cancel.clone(),
        preempt: preempt.clone(),
        is_partial: kind == JobKind::Partial,
    };
    unsafe {
        params.set_abort_callback(Some(abort_trampoline));
        params.set_abort_callback_user_data(
            (&mut abort_state as *mut AbortState).cast::<std::ffi::c_void>(),
        );
    }

    state.full(params, audio)?;
    drop(abort_state);

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_string());
    }
    Ok(text)
}

fn apply_glossary(text: &str, glossary: &std::collections::HashMap<String, String>) -> String {
    let mut result = text.to_owned();
    // Longest phrases first: "сі шарп" must win over a shorter "сі".
    let mut entries: Vec<_> = glossary.iter().collect();
    entries.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    for (from, to) in entries {
        if from.is_empty() {
            continue;
        }
        result = replace_ci(&result, from, to);
    }
    result
}

fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return haystack.to_owned();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut start = 0;
    while let Some(pos) = lower[start..].find(&needle) {
        let absolute = start + pos;
        let end = absolute + needle.len();
        let left_ok = haystack[..absolute]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let right_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if left_ok && right_ok {
            out.push_str(&haystack[start..absolute]);
            out.push_str(replacement);
        } else {
            out.push_str(&haystack[start..end]);
        }
        start = end;
    }
    out.push_str(&haystack[start..]);
    out
}

/// Whisper sometimes emits one character thousands of times at the end of a
/// synthetic or clipped clip. Keep a readable prefix and drop the loop.
fn trim_character_loop(text: &str) -> String {
    let mut previous = None;
    let mut run = 0;
    let mut cut = text.len();
    for (index, ch) in text.char_indices() {
        if Some(ch) == previous {
            run += 1;
            if run >= 8 {
                cut = index;
                break;
            }
        } else {
            previous = Some(ch);
            run = 1;
        }
    }
    text[..cut].trim_end().to_owned()
}

/// Detect the spoken language once per session. Requires at least two seconds
/// of audio; encoding for detection costs one extra encoder pass, which is why
/// it is not run for every partial.
fn detect_language(
    state: &mut WhisperState,
    audio: &[f32],
    threads: i32,
) -> std::result::Result<Option<(String, f32)>, whisper_rs::WhisperError> {
    if audio.len() < 16_000 * 2 {
        return Ok(None);
    }
    let threads = threads.max(1) as usize;
    state.pcm_to_mel(audio, threads)?;
    let (id, probs) = state.lang_detect(0, threads)?;
    let confidence = probs.get(id as usize).copied().unwrap_or(0.0);
    if confidence < 0.6 {
        return Ok(None);
    }
    match whisper_rs::get_lang_str(id) {
        Some(code) if code != "?" => Ok(Some((code.to_owned(), confidence))),
        _ => Ok(None),
    }
}

/// Whisper's audio context is 1500 frames for 30 s, i.e. 50 frames per second.
/// Add one second of margin and keep the value even (whisper.cpp is happiest
/// with aligned values).
fn audio_ctx_for(samples: usize) -> i32 {
    let frames = samples / 320; // 50 frames/s at 16 kHz
    let with_margin = frames + 50;
    let aligned = with_margin.saturating_add(7) & !7;
    // Short questions need far fewer than the default 1500 encoder frames.
    // 256 frames (~5 s) is the stable floor; real 3–8 s questions land at
    // 300–500. Keep the value aligned to 8: some whisper.cpp builds fail to
    // encode with odd frame counts on longer segments.
    aligned.clamp(256, 1_500) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glossary_fixes_programming_transliterations() {
        let mut glossary = std::collections::HashMap::new();
        glossary.insert("сінгалтан".to_owned(), "Singleton".to_owned());
        glossary.insert("у сі".to_owned(), "у C#".to_owned());
        glossary.insert("сі шарп".to_owned(), "C#".to_owned());
        assert_eq!(
            apply_glossary("патерн сінгалтан у сі шарп", &glossary),
            "патерн Singleton у C#"
        );
    }

    #[test]
    fn audio_ctx_tracks_duration() {
        assert_eq!(audio_ctx_for(16_000), 256);
        assert_eq!(audio_ctx_for(16_000 * 4), 256);
        assert_eq!(audio_ctx_for(16_000 * 12), 656);
        assert_eq!(audio_ctx_for(16_000 * 30), 1500);
    }
}
