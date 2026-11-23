use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::{AudioFrame, AudioSource, CaptureInfo, FRAME_SAMPLES};
use crate::config::{BudgetConfig, Config, HotkeyConfig, SttBackend, UiConfig};
use crate::detector::{QuestionDetector, normalize_words};
use crate::error::Result;
use crate::llm::{LlmClient, LlmEvent, LlmRequest};
use crate::metrics::{Latency, epoch_ms, epoch_ms_at};
use crate::stt::agreement::LocalAgreement;
use crate::stt::{JobKind, SttEvent, SttJob};
use crate::vad::{VadEvent, spawn_vad};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiState {
    Loading,
    Listening,
    Thinking,
    Answering,
    Paused,
    Error,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PipelineEvent {
    Init {
        backend: String,
        model: String,
        budgets: BudgetConfig,
        hotkeys: HotkeyConfig,
        ui: UiConfig,
    },
    Status {
        state: UiState,
        detail: String,
    },
    ModelProgress {
        downloaded: u64,
        total: Option<u64>,
    },
    Audio {
        info: CaptureInfo,
    },
    TranscriptPartial {
        text: String,
        segment_id: u64,
    },
    TranscriptFinal {
        text: String,
        segment_id: u64,
        stt_ms: u64,
    },
    Question {
        text: String,
        speech_end_epoch_ms: u64,
    },
    AnswerStart {
        request_id: u64,
        question: String,
        latency: Latency,
    },
    AnswerDelta {
        request_id: u64,
        text: String,
    },
    AnswerDone {
        request_id: u64,
        full_text: String,
        incomplete: bool,
        error: Option<String>,
    },
    NoQuestion {
        transcript: String,
    },
    Paused {
        paused: bool,
    },
    Error {
        stage: String,
        message: String,
        fatal: bool,
    },
}

#[derive(Debug)]
pub enum Command {
    Pause,
    Resume,
    Regenerate,
    AskManual(String),
    SetProjectContext(String),
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
struct FinalTiming {
    speech_end: Instant,
    vad_decision: Instant,
    stt_started: Instant,
    stt_done: Instant,
    speech_end_epoch_ms: u64,
}

impl FinalTiming {
    fn immediate() -> Self {
        let now = Instant::now();
        Self {
            speech_end: now,
            vad_decision: now,
            stt_started: now,
            stt_done: now,
            speech_end_epoch_ms: epoch_ms(),
        }
    }

    fn stt_ms(&self) -> u64 {
        self.stt_done
            .checked_duration_since(self.stt_started)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

struct PendingFinal {
    speech_end: Instant,
    vad_decision: Instant,
    stt_started: Instant,
}

struct LastQuestion {
    text: String,
    context: String,
}

struct AnswerRequest {
    id: u64,
    question: String,
    accepted: bool,
    visible: bool,
    buffer: String,
    full_text: String,
    done: bool,
    incomplete: bool,
    stream_error: Option<String>,
    no_question: bool,
    had_error: bool,
    speculative_started: bool,
    first_token_at: Option<Instant>,
    llm_requested: Instant,
    speech_end: Instant,
    vad_decision: Instant,
    stt_started: Instant,
    stt_done: Instant,
    speech_end_epoch_ms: u64,
    cancel: CancellationToken,
}

impl AnswerRequest {
    fn latency(&self, first_visible: Instant) -> Latency {
        let mut latency = Latency::measure(
            self.speech_end,
            self.vad_decision,
            self.stt_started,
            self.stt_done,
            self.llm_requested,
            first_visible,
            self.speculative_started,
        );
        // LLM TTFT is measured to the raw first token, even if the pipeline
        // buffered it until the final transcript was confirmed.
        if let Some(first) = self.first_token_at {
            latency.llm_ttft_ms = first
                .checked_duration_since(self.llm_requested)
                .unwrap_or_default()
                .as_millis() as u64;
        }
        latency.speech_end_epoch_ms = self.speech_end_epoch_ms;
        latency.first_token_epoch_ms = epoch_ms();
        latency
    }
}

#[derive(Default)]
struct History {
    entries: VecDeque<(Instant, String)>,
    retention: Duration,
    max_chars: usize,
}

impl History {
    fn new(retention: Duration, max_chars: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            retention,
            max_chars: max_chars.max(200),
        }
    }

    fn push(&mut self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let text = if text.len() > 2_000 {
            let mut start = text.len() - 2_000;
            while start < text.len() && !text.is_char_boundary(start) {
                start += 1;
            }
            text[start..].to_owned()
        } else {
            text
        };
        let now = Instant::now();
        self.entries.push_back((now, text));
        self.trim(now);
    }

    fn context(&self) -> String {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.retention).unwrap_or(now);
        let mut out = String::new();
        for (_, text) in self.entries.iter().filter(|(at, _)| *at >= cutoff) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
        }
        if out.len() > self.max_chars {
            let mut start = out.len() - self.max_chars;
            while start < out.len() && !out.is_char_boundary(start) {
                start += 1;
            }
            out = out[start..].to_owned();
        }
        out.trim().to_owned()
    }

    fn trim(&mut self, now: Instant) {
        let cutoff = now.checked_sub(self.retention).unwrap_or(now);
        while self.entries.front().is_some_and(|(at, _)| *at < cutoff) {
            self.entries.pop_front();
        }
        let mut total = self
            .entries
            .iter()
            .map(|(_, text)| text.len())
            .sum::<usize>();
        while total > self.max_chars {
            if let Some((_, text)) = self.entries.pop_front() {
                total = total.saturating_sub(text.len());
            } else {
                break;
            }
        }
    }
}

struct Runtime {
    config: Config,
    events: mpsc::Sender<PipelineEvent>,
    llm: Option<LlmClient>,
    llm_tx: mpsc::UnboundedSender<LlmEvent>,
    next_request_id: u64,
    active: Option<AnswerRequest>,
    speculation: Option<AnswerRequest>,
    detector: QuestionDetector,
    agreement: LocalAgreement,
    history: History,
    last_question: Option<LastQuestion>,
    paused: bool,
    paused_flag: Arc<AtomicBool>,
    segment_id: u64,
    partial_text: String,
    project_context: String,
    root_cancel: CancellationToken,
}

impl Runtime {
    fn new(
        config: Config,
        events: mpsc::Sender<PipelineEvent>,
        llm_tx: mpsc::UnboundedSender<LlmEvent>,
        root_cancel: CancellationToken,
        paused_flag: Arc<AtomicBool>,
        project_context: String,
    ) -> Self {
        let detector = QuestionDetector::new(&config.detector);
        let history = History::new(
            Duration::from_secs(config.llm.history_secs.max(5)),
            config.llm.max_history_chars,
        );
        Self {
            config,
            events,
            llm: None,
            llm_tx,
            next_request_id: 1,
            active: None,
            speculation: None,
            detector,
            agreement: LocalAgreement::new(),
            history,
            last_question: None,
            paused: false,
            paused_flag,
            segment_id: 0,
            partial_text: String::new(),
            project_context,
            root_cancel,
        }
    }

    async fn emit(&self, event: PipelineEvent) {
        if self.events.send(event).await.is_err() {
            tracing::debug!("pipeline event receiver closed");
        }
    }

    async fn status(&mut self, state: UiState, detail: impl Into<String>) {
        self.emit(PipelineEvent::Status {
            state,
            detail: detail.into(),
        })
        .await;
    }

    async fn error(&self, stage: &str, message: impl Into<String>, fatal: bool) {
        self.emit(PipelineEvent::Error {
            stage: stage.to_owned(),
            message: message.into(),
            fatal,
        })
        .await;
    }

    async fn ensure_llm(&mut self) -> Option<LlmClient> {
        if let Some(client) = &self.llm {
            return Some(client.clone());
        }
        match LlmClient::new(self.config.llm.clone()) {
            Ok(client) => {
                self.llm = Some(client.clone());
                Some(client)
            }
            Err(err) => {
                self.error("llm", err.to_string(), false).await;
                None
            }
        }
    }

    async fn start_request(
        &mut self,
        question: String,
        context: String,
        timing: FinalTiming,
        speculative: bool,
    ) -> Option<u64> {
        if speculative && !self.config.detector.speculative {
            return None;
        }
        let Some(client) = self.ensure_llm().await else {
            if !speculative {
                self.idle_status().await;
            }
            return None;
        };
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        let cancel = self.root_cancel.child_token();
        let llm_requested = Instant::now();
        let request = LlmRequest {
            id,
            question: question.clone(),
            transcript: context,
            project: self.project_context.clone(),
            speech_end_epoch_ms: timing.speech_end_epoch_ms,
        };
        client.spawn(request, self.llm_tx.clone(), cancel.clone());

        let req = AnswerRequest {
            id,
            question,
            accepted: !speculative,
            visible: false,
            buffer: String::new(),
            full_text: String::new(),
            done: false,
            incomplete: false,
            stream_error: None,
            no_question: false,
            had_error: false,
            speculative_started: speculative,
            first_token_at: None,
            llm_requested,
            speech_end: timing.speech_end,
            vad_decision: timing.vad_decision,
            stt_started: timing.stt_started,
            stt_done: timing.stt_done,
            speech_end_epoch_ms: timing.speech_end_epoch_ms,
            cancel,
        };

        if speculative {
            if let Some(old) = self.speculation.take() {
                old.cancel.cancel();
            }
            self.speculation = Some(req);
            tracing::debug!(request_id = id, "started speculative LLM request");
        } else {
            if let Some(old) = self.active.take() {
                old.cancel.cancel();
            }
            if let Some(old) = self.speculation.take() {
                old.cancel.cancel();
            }
            self.active = Some(req);
            self.status(UiState::Thinking, "thinking").await;
            tracing::debug!(request_id = id, "started final LLM request");
        }
        Some(id)
    }

    async fn on_speech_started(&mut self) {
        self.segment_id = self.segment_id.wrapping_add(1);
        self.agreement.reset();
        self.partial_text.clear();
        if let Some(spec) = self.speculation.take() {
            spec.cancel.cancel();
        }
        if !self.paused {
            self.idle_status().await;
        }
    }

    async fn on_local_partial(&mut self, hypothesis: &str) {
        if self.paused {
            return;
        }
        if let Some(new_words) = self.agreement.update(hypothesis)
            && !new_words.is_empty()
        {
            if !self.partial_text.is_empty() {
                self.partial_text.push(' ');
            }
            self.partial_text.push_str(&new_words);
            self.emit(PipelineEvent::TranscriptPartial {
                text: self.partial_text.clone(),
                segment_id: self.segment_id,
            })
            .await;
        }
        self.maybe_speculate(hypothesis).await;
    }

    async fn on_cloud_partial(&mut self, text: &str) {
        if self.paused {
            return;
        }
        self.partial_text = text.trim().to_owned();
        if self.config.ui.show_partial && !self.partial_text.is_empty() {
            self.emit(PipelineEvent::TranscriptPartial {
                text: self.partial_text.clone(),
                segment_id: self.segment_id,
            })
            .await;
        }
        self.maybe_speculate(text).await;
    }

    async fn maybe_speculate(&mut self, candidate: &str) {
        if self.paused || !self.config.detector.speculative {
            return;
        }
        let candidate = candidate.trim();
        if normalize_words(candidate).len() < self.config.detector.speculation_min_words {
            return;
        }
        if !self.detector.is_question(candidate) {
            return;
        }
        if let Some(existing) = &self.speculation
            && self.detector.compatible(&existing.question, candidate)
        {
            return;
        }
        let context = self.history.context();
        let timing = FinalTiming {
            speech_end: Instant::now(),
            vad_decision: Instant::now(),
            stt_started: Instant::now(),
            stt_done: Instant::now(),
            speech_end_epoch_ms: 0,
        };
        self.start_request(candidate.to_owned(), context, timing, true)
            .await;
    }

    async fn on_final(&mut self, text: String, timing: FinalTiming) {
        if self.paused {
            return;
        }
        let text = text.trim().to_owned();
        tracing::debug!(transcript = %text, "final transcript text");
        let context = self.history.context();
        self.history.push(text.clone());
        tracing::info!(
            vad_ms = timing
                .vad_decision
                .saturating_duration_since(timing.speech_end)
                .as_millis() as u64,
            stt_ms = timing.stt_ms(),
            chars = text.chars().count(),
            "final transcript ready"
        );
        self.emit(PipelineEvent::TranscriptFinal {
            text: text.clone(),
            segment_id: self.segment_id,
            stt_ms: timing.stt_ms(),
        })
        .await;

        if text.is_empty() {
            self.idle_status().await;
            return;
        }

        let is_question = self.detector.is_question(&text);

        if !is_question {
            if let Some(spec) = self.speculation.take() {
                spec.cancel.cancel();
            }

            if !self.config.detector.send_all {
                // Local heuristic mode: statements never reach the model.
                // Do not disturb an answer that is still streaming from the
                // previous question.
                if self.active.is_none() {
                    self.emit(PipelineEvent::NoQuestion {
                        transcript: text.clone(),
                    })
                    .await;
                    self.idle_status().await;
                }
                return;
            }

            // send_all mode: the model sees every final and decides whether it
            // needs an answer. While an answer is streaming, keep it; the new
            // utterance is already in the history for the next request.
            if self.active.is_some() {
                return;
            }
            self.last_question = Some(LastQuestion {
                text: text.clone(),
                context: context.clone(),
            });
            self.start_request(text, context, timing, false).await;
            // A statement request is speculative in spirit: do not leave the
            // status on "thinking" while the model decides it is NO_QUESTION.
            self.idle_status().await;
            return;
        }

        self.last_question = Some(LastQuestion {
            text: text.clone(),
            context: context.clone(),
        });
        self.emit(PipelineEvent::Question {
            text: text.clone(),
            speech_end_epoch_ms: timing.speech_end_epoch_ms,
        })
        .await;

        if let Some(mut spec) = self.speculation.take() {
            let compatible = !spec.no_question
                && !spec.had_error
                && self.detector.compatible(&spec.question, &text);
            let empty_after_done = spec.done && spec.buffer.is_empty() && spec.full_text.is_empty();
            if compatible && !empty_after_done {
                spec.accepted = true;
                spec.question = text.clone();
                spec.speech_end = timing.speech_end;
                spec.vad_decision = timing.vad_decision;
                spec.stt_started = timing.stt_started;
                spec.stt_done = timing.stt_done;
                spec.speech_end_epoch_ms = timing.speech_end_epoch_ms;
                if let Some(old) = self.active.take() {
                    old.cancel.cancel();
                }
                self.active = Some(spec);
                self.status(UiState::Thinking, "thinking").await;
                tracing::debug!("accepted speculative answer");
                self.flush_active().await;
                return;
            }
            spec.cancel.cancel();
        }

        self.start_request(text, context, timing, false).await;
    }

    async fn flush_active(&mut self) {
        let Some(req) = self.active.as_mut() else {
            return;
        };
        if req.visible {
            return;
        }
        if req.buffer.is_empty() && req.done && !req.full_text.is_empty() {
            req.buffer = req.full_text.clone();
        }
        if req.buffer.is_empty() {
            return;
        }
        let latency = req.latency(Instant::now());
        tracing::info!(
            vad_ms = latency.vad_ms,
            stt_ms = latency.stt_ms,
            llm_ttft_ms = latency.llm_ttft_ms,
            backend_total_ms = latency.backend_total_ms,
            speculative = latency.speculative,
            "first answer token"
        );
        let request_id = req.id;
        let question = req.question.clone();
        let buffered = std::mem::take(&mut req.buffer);
        req.visible = true;
        let done = req.done;
        let incomplete = req.incomplete;
        let stream_error = req.stream_error.clone();
        let full_text = if req.full_text.is_empty() {
            buffered.clone()
        } else {
            req.full_text.clone()
        };

        self.emit(PipelineEvent::AnswerStart {
            request_id,
            question,
            latency,
        })
        .await;
        self.emit(PipelineEvent::AnswerDelta {
            request_id,
            text: buffered,
        })
        .await;
        self.status(UiState::Answering, "answering").await;
        if done {
            self.emit(PipelineEvent::AnswerDone {
                request_id,
                full_text,
                incomplete,
                error: stream_error,
            })
            .await;
            self.active = None;
            self.idle_status().await;
        }
    }

    async fn on_llm_event(&mut self, event: LlmEvent) {
        match event {
            LlmEvent::FirstToken { request_id, at } => {
                if let Some(req) = self.active.as_mut().filter(|r| r.id == request_id) {
                    req.first_token_at.get_or_insert(at);
                } else if let Some(req) = self.speculation.as_mut().filter(|r| r.id == request_id) {
                    req.first_token_at.get_or_insert(at);
                }
            }
            LlmEvent::Token {
                request_id,
                text,
                at,
            } => {
                let is_active = self.active.as_ref().is_some_and(|r| r.id == request_id);
                if is_active {
                    let start = {
                        let Some(req) = self.active.as_mut() else {
                            return;
                        };
                        req.first_token_at.get_or_insert(at);
                        if req.visible {
                            None
                        } else {
                            req.visible = true;
                            Some((req.question.clone(), req.latency(Instant::now())))
                        }
                    };
                    if let Some((question, latency)) = start {
                        tracing::info!(
                            vad_ms = latency.vad_ms,
                            stt_ms = latency.stt_ms,
                            llm_ttft_ms = latency.llm_ttft_ms,
                            backend_total_ms = latency.backend_total_ms,
                            speculative = latency.speculative,
                            "first answer token"
                        );
                        self.emit(PipelineEvent::AnswerStart {
                            request_id,
                            question,
                            latency,
                        })
                        .await;
                        self.status(UiState::Answering, "answering").await;
                    }
                    self.emit(PipelineEvent::AnswerDelta { request_id, text })
                        .await;
                } else if let Some(req) = self.speculation.as_mut().filter(|r| r.id == request_id) {
                    req.first_token_at.get_or_insert(at);
                    req.buffer.push_str(&text);
                }
            }
            LlmEvent::NoQuestion { request_id } => {
                if self.active.as_ref().is_some_and(|r| r.id == request_id) {
                    let question = self
                        .active
                        .as_ref()
                        .map(|r| r.question.clone())
                        .unwrap_or_default();
                    self.emit(PipelineEvent::NoQuestion {
                        transcript: question,
                    })
                    .await;
                    self.active = None;
                    self.idle_status().await;
                } else if let Some(req) = self.speculation.as_mut().filter(|r| r.id == request_id) {
                    req.no_question = true;
                }
            }
            LlmEvent::Done {
                request_id,
                full_text,
                first_token_at,
                incomplete,
                error,
            } => {
                if self.active.as_ref().is_some_and(|r| r.id == request_id) {
                    let request = self.active.take();
                    let Some(req) = request else {
                        return;
                    };
                    let final_text = if full_text.is_empty() {
                        req.buffer.clone()
                    } else {
                        full_text
                    };
                    if final_text.trim().is_empty() && !req.visible {
                        self.error("llm", "model returned an empty answer", false)
                            .await;
                        self.idle_status().await;
                        return;
                    }
                    if !req.visible {
                        let latency = req.latency(Instant::now());
                        self.emit(PipelineEvent::AnswerStart {
                            request_id,
                            question: req.question.clone(),
                            latency,
                        })
                        .await;
                        self.emit(PipelineEvent::AnswerDelta {
                            request_id,
                            text: final_text.clone(),
                        })
                        .await;
                        self.status(UiState::Answering, "answering").await;
                    }
                    self.emit(PipelineEvent::AnswerDone {
                        request_id,
                        full_text: final_text,
                        incomplete,
                        error,
                    })
                    .await;
                    self.idle_status().await;
                } else if let Some(req) = self.speculation.as_mut().filter(|r| r.id == request_id) {
                    req.done = true;
                    req.incomplete = incomplete;
                    req.stream_error = error;
                    req.first_token_at = req.first_token_at.or(first_token_at);
                    if !full_text.is_empty() {
                        req.full_text = full_text;
                        if req.buffer.is_empty() {
                            req.buffer = req.full_text.clone();
                        }
                    }
                }
            }
            LlmEvent::Error {
                request_id,
                message,
            } => {
                if self.active.as_ref().is_some_and(|r| r.id == request_id) {
                    self.active = None;
                    self.error("llm", message, false).await;
                    self.idle_status().await;
                } else if let Some(req) = self.speculation.as_mut().filter(|r| r.id == request_id) {
                    req.had_error = true;
                    req.no_question = false;
                }
            }
        }
    }

    async fn idle_status(&mut self) {
        if self.paused {
            self.status(UiState::Paused, "paused").await;
        } else {
            self.status(UiState::Listening, "listening").await;
        }
    }

    async fn pause(&mut self) {
        self.paused = true;
        self.paused_flag.store(true, Ordering::SeqCst);
        if let Some(req) = self.active.take() {
            req.cancel.cancel();
        }
        if let Some(req) = self.speculation.take() {
            req.cancel.cancel();
        }
        self.agreement.reset();
        self.partial_text.clear();
        self.status(UiState::Paused, "paused").await;
        self.emit(PipelineEvent::Paused { paused: true }).await;
    }

    async fn resume(&mut self) {
        self.paused = false;
        self.paused_flag.store(false, Ordering::SeqCst);
        self.agreement.reset();
        self.partial_text.clear();
        self.status(UiState::Listening, "listening").await;
        self.emit(PipelineEvent::Paused { paused: false }).await;
    }

    async fn regenerate(&mut self) {
        let Some(last) = self.last_question.as_ref() else {
            self.error("pipeline", "no previous question to regenerate", false)
                .await;
            return;
        };
        let question = last.text.clone();
        let context = last.context.clone();
        let timing = FinalTiming::immediate();
        self.emit(PipelineEvent::Question {
            text: question.clone(),
            speech_end_epoch_ms: timing.speech_end_epoch_ms,
        })
        .await;
        self.start_request(question, context, timing, false).await;
    }

    async fn ask_manual(&mut self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            self.error("manual", "manual question is empty", false)
                .await;
            return;
        }
        let context = self.history.context();
        self.last_question = Some(LastQuestion {
            text: text.clone(),
            context: context.clone(),
        });
        let timing = FinalTiming::immediate();
        self.emit(PipelineEvent::Question {
            text: text.clone(),
            speech_end_epoch_ms: timing.speech_end_epoch_ms,
        })
        .await;
        self.start_request(text, context, timing, false).await;
    }
}

async fn next_optional<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    mut source: Box<dyn AudioSource>,
    events: mpsc::Sender<PipelineEvent>,
    mut commands: mpsc::Receiver<Command>,
    root_cancel: CancellationToken,
    project_context: String,
) -> Result<()> {
    let mut config = config;
    config.normalize();
    config.validate()?;

    // Cloud STT is optional. Without a key, keep the app usable with the
    // local Whisper backend instead of failing at startup.
    if config.stt.backend == SttBackend::Deepgram {
        #[cfg(feature = "deepgram")]
        if config.deepgram_api_key().is_err() {
            tracing::warn!("Deepgram key is not configured; falling back to local Whisper");
            config.stt.backend = SttBackend::Whisper;
        }
        #[cfg(not(feature = "deepgram"))]
        {
            tracing::warn!("built without Deepgram support; falling back to local Whisper");
            config.stt.backend = SttBackend::Whisper;
        }
    }
    if !project_context.is_empty() {
        tracing::info!(
            chars = project_context.chars().count(),
            "project context enabled"
        );
    }
    let paused_flag = Arc::new(AtomicBool::new(false));
    let (llm_tx, mut llm_rx) = mpsc::unbounded_channel();
    let mut runtime = Runtime::new(
        config.clone(),
        events.clone(),
        llm_tx,
        root_cancel.clone(),
        paused_flag.clone(),
        project_context,
    );

    runtime
        .status(UiState::Loading, "initializing speech-to-text")
        .await;

    // Warm DNS/TLS/HTTP2 before the first question. Do not fail startup if the
    // API key is not set yet: `ensure_llm` retries lazily when a question
    // arrives, and the UI still gets a useful STT-only session.
    match LlmClient::new(config.llm.clone()) {
        Ok(client) => {
            if config.llm.warmup {
                let warm_client = client.clone();
                let warm_cancel = root_cancel.child_token();
                tokio::spawn(async move {
                    warm_client.warmup(&warm_cancel).await;
                });
            }
            runtime.llm = Some(client);
        }
        Err(err) => {
            tracing::warn!(%err, "LLM is not ready at startup; will retry on first question");
            runtime.error("llm", err.to_string(), false).await;
        }
    }

    #[allow(unused_assignments)]
    let mut stt_worker = None;
    #[allow(unused_assignments)]
    let mut stt_rx: Option<mpsc::Receiver<SttEvent>> = None;
    let mut cloud_rx: Option<mpsc::Receiver<crate::stt::CloudSttEvent>> = None;
    #[allow(unused_mut)]
    let mut cloud_audio_tx: Option<mpsc::Sender<AudioFrame>> = None;
    #[allow(unused_mut)]
    let mut cloud_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut model_label = String::new();

    match config.stt.backend {
        SttBackend::Whisper => {
            let model_path = config.model_path();
            let progress_events = events.clone();
            let progress = move |progress: crate::stt::ModelProgress| {
                let _ = progress_events.try_send(PipelineEvent::ModelProgress {
                    downloaded: progress.downloaded,
                    total: progress.total,
                });
            };
            let model_path = crate::stt::ensure_model(
                &model_path,
                &config.model_url(),
                config.stt.whisper.download,
                &root_cancel,
                progress,
            )
            .await?;
            let (tx, mut rx) = mpsc::channel(32);
            let worker = crate::stt::whisper::WhisperWorker::spawn(
                model_path,
                config.stt.whisper.clone(),
                tx,
                root_cancel.child_token(),
            )?;
            let mut ready = false;
            while !ready {
                tokio::select! {
                    _ = root_cancel.cancelled() => {
                        worker.shutdown().await;
                        return Ok(());
                    }
                    event = rx.recv() => {
                        match event {
                            Some(SttEvent::Loading { stage }) => {
                                runtime.status(UiState::Loading, stage).await;
                            }
                            Some(SttEvent::Ready { model, load_ms, warmup_ms }) => {
                                model_label = model;
                                runtime.status(
                                    UiState::Loading,
                                    format!("Whisper ready (load {load_ms} ms, warmup {warmup_ms} ms)"),
                                ).await;
                                ready = true;
                            }
                            Some(SttEvent::Fatal(message)) => {
                                runtime.error("stt", message.clone(), true).await;
                                worker.shutdown().await;
                                return Err(anyhow::anyhow!(message));
                            }
                            Some(SttEvent::Error { message, .. }) => {
                                runtime.error("stt", message, false).await;
                            }
                            Some(SttEvent::Result(_)) => {}
                            None => {
                                let message = "Whisper worker exited during startup".to_owned();
                                runtime.error("stt", message.clone(), true).await;
                                return Err(anyhow::anyhow!(message));
                            }
                        }
                    }
                }
            }
            stt_worker = Some(worker);
            stt_rx = Some(rx);
        }
        SttBackend::Deepgram => {
            #[cfg(feature = "deepgram")]
            {
                runtime
                    .status(UiState::Loading, "connecting to Deepgram")
                    .await;
                let api_key = config.deepgram_api_key()?;
                let (audio_tx, audio_rx) = mpsc::channel::<AudioFrame>(64);
                let (cloud_tx, mut cloud_events) = mpsc::channel(128);
                let dg_cancel = root_cancel.child_token();
                let dg_config = config.stt.deepgram.clone();
                let handle = tokio::spawn(async move {
                    if let Err(err) =
                        crate::stt::deepgram::run(audio_rx, dg_config, api_key, cloud_tx, dg_cancel)
                            .await
                    {
                        tracing::error!(%err, "Deepgram task stopped");
                    }
                });
                let connected = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        match cloud_events.recv().await {
                            Some(crate::stt::CloudSttEvent::Connected) => return Ok(()),
                            Some(crate::stt::CloudSttEvent::Error(message)) => {
                                return Err(anyhow::anyhow!(message));
                            }
                            Some(_) => continue,
                            None => return Err(anyhow::anyhow!("Deepgram task exited")),
                        }
                    }
                })
                .await;
                match connected {
                    Ok(Ok(())) => {
                        model_label = format!("deepgram:{}", config.stt.deepgram.model);
                        cloud_audio_tx = Some(audio_tx);
                        cloud_rx = Some(cloud_events);
                        cloud_handle = Some(handle);
                    }
                    Ok(Err(err)) => {
                        root_cancel.cancel();
                        let _ = handle.await;
                        runtime.error("stt", err.to_string(), true).await;
                        return Err(err);
                    }
                    Err(_) => {
                        root_cancel.cancel();
                        let _ = handle.await;
                        let message = "Deepgram did not connect within 10 s".to_owned();
                        runtime.error("stt", message.clone(), true).await;
                        return Err(anyhow::anyhow!(message));
                    }
                }
            }
            #[cfg(not(feature = "deepgram"))]
            {
                let message = "this build has no Deepgram support; rebuild with --features deepgram or choose stt.backend = \"whisper\"".to_owned();
                runtime.error("stt", message.clone(), true).await;
                return Err(anyhow::anyhow!(message));
            }
        }
    }

    let session = match source.start() {
        Ok(session) => session,
        Err(err) => {
            if let Some(worker) = stt_worker.take() {
                worker.shutdown().await;
            }
            if let Some(handle) = cloud_handle.take() {
                root_cancel.cancel();
                let _ = handle.await;
            }
            return Err(err);
        }
    };
    let (audio_stream, audio_guard) = session.into_parts();
    let info = audio_stream.info.clone();
    runtime.emit(PipelineEvent::Audio { info }).await;

    let (vad_tx, vad_rx) = mpsc::channel(64);
    let (vad_events_tx, vad_events_rx) = mpsc::channel(64);
    let mut vad_events_rx = Some(vad_events_rx);
    let vad_handle = match spawn_vad(
        vad_rx,
        config.vad.clone(),
        vad_events_tx,
        root_cancel.child_token(),
        paused_flag.clone(),
    ) {
        Ok(handle) => handle,
        Err(err) => {
            audio_guard.shutdown().await;
            if let Some(worker) = stt_worker.take() {
                worker.shutdown().await;
            }
            if let Some(handle) = cloud_handle.take() {
                root_cancel.cancel();
                let _ = handle.await;
            }
            return Err(err);
        }
    };

    let paused_for_forward = paused_flag.clone();
    let mut cloud_tx_for_forward = cloud_audio_tx;
    let forward_handle = tokio::spawn(async move {
        let mut frames = audio_stream.frames;
        let mut last_real = Instant::now();
        let mut last_synth = Instant::now();
        let mut synth_seq = 0_u64;

        loop {
            tokio::select! {
                incoming = frames.recv() => {
                    let Some(frame) = incoming else {
                        break;
                    };
                    last_real = Instant::now();
                    tracing::trace!(sequence = frame.sequence, "forwarded audio frame");
                    if !paused_for_forward.load(Ordering::Relaxed)
                        && let Some(tx) = cloud_tx_for_forward.take()
                    {
                        match tx.try_send(frame.clone()) {
                            Ok(()) => cloud_tx_for_forward = Some(tx),
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                tracing::debug!("Deepgram audio queue full; dropping a frame");
                                cloud_tx_for_forward = Some(tx);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                cloud_tx_for_forward = None;
                            }
                        }
                    }
                    if vad_tx.send(frame).await.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    // The same idle-sink stall that affects the local VAD also
                    // stops Deepgram from receiving audio. Feed real-time
                    // silence to the cloud stream so its endpointing can
                    // finalize the last utterance.
                    if cloud_tx_for_forward.is_none()
                        || paused_for_forward.load(Ordering::Relaxed)
                        || last_real.elapsed() < Duration::from_millis(120)
                        || last_synth.elapsed() < Duration::from_millis(20)
                    {
                        continue;
                    }
                    last_synth = Instant::now();
                    synth_seq = synth_seq.wrapping_add(1);
                    let zero = AudioFrame {
                        samples: vec![0.0; FRAME_SAMPLES],
                        captured_at: Instant::now(),
                        sequence: synth_seq,
                    };
                    if let Some(tx) = cloud_tx_for_forward.take() {
                        match tx.try_send(zero) {
                            Ok(()) => cloud_tx_for_forward = Some(tx),
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                cloud_tx_for_forward = Some(tx);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                cloud_tx_for_forward = None;
                            }
                        }
                    }
                }
            }
        }
    });

    runtime
        .emit(PipelineEvent::Init {
            backend: match config.stt.backend {
                SttBackend::Whisper => "whisper".to_owned(),
                SttBackend::Deepgram => "deepgram".to_owned(),
            },
            model: model_label,
            budgets: config.budgets.clone(),
            hotkeys: config.hotkeys.clone(),
            ui: config.ui.clone(),
        })
        .await;
    runtime.status(UiState::Listening, "listening").await;

    let mut pending_finals: HashMap<u64, PendingFinal> = HashMap::new();
    let mut recent_vad: Option<(Instant, Instant)> = None;
    let mut fatal_error: Option<String> = None;

    loop {
        tokio::select! {
            biased;
            _ = root_cancel.cancelled() => break,
            command = commands.recv() => {
                match command {
                    Some(Command::Pause) => {
                        pending_finals.clear();
                        runtime.pause().await;
                    }
                    Some(Command::Resume) => runtime.resume().await,
                    Some(Command::Regenerate) => runtime.regenerate().await,
                    Some(Command::AskManual(text)) => runtime.ask_manual(text).await,
                    Some(Command::SetProjectContext(context)) => {
                        tracing::info!(
                            chars = context.chars().count(),
                            "project context updated"
                        );
                        runtime.project_context = context;
                    }
                    Some(Command::Shutdown) | None => break,
                }
            }
            event = next_optional(&mut vad_events_rx) => {
                match event {
                    Some(VadEvent::SpeechStarted { .. }) => runtime.on_speech_started().await,
                    Some(VadEvent::Partial { audio, .. }) => {
                        if !runtime.paused
                            && matches!(config.stt.backend, SttBackend::Whisper)
                            && let Some(worker) = &stt_worker
                        {
                            worker.submit_partial(SttJob {
                                id: runtime.segment_id,
                                audio,
                                kind: JobKind::Partial,
                                submitted_at: Instant::now(),
                                speech_end: None,
                            });
                        }
                    }
                    Some(VadEvent::Endpoint { audio, speech_end, at }) => {
                        tracing::debug!(
                            samples = audio.len(),
                            "VAD endpoint; submitting final STT"
                        );
                        recent_vad = Some((speech_end, at));
                        if !runtime.paused && matches!(config.stt.backend, SttBackend::Whisper) {
                            let stt_started = Instant::now();
                            pending_finals.insert(
                                runtime.segment_id,
                                PendingFinal {
                                    speech_end,
                                    vad_decision: at,
                                    stt_started,
                                },
                            );
                            match &stt_worker {
                                Some(worker) => {
                                    if let Err(err) = worker.submit_final(SttJob {
                                        id: runtime.segment_id,
                                        audio,
                                        kind: JobKind::Final,
                                        submitted_at: stt_started,
                                        speech_end: Some(speech_end),
                                    }).await {
                                        runtime.error("stt", err.to_string(), false).await;
                                    }
                                }
                                None => runtime.error("stt", "Whisper worker is not running", true).await,
                            }
                        }
                    }
                    None => {
                        vad_events_rx = None;
                    }
                }
            }
            event = next_optional(&mut stt_rx) => {
                match event {
                    Some(SttEvent::Loading { stage }) => runtime.status(UiState::Loading, stage).await,
                    Some(SttEvent::Ready { .. }) => {}
                    Some(SttEvent::Result(result)) => {
                        match result.kind {
                            JobKind::Partial => {
                                if result.id == runtime.segment_id {
                                    runtime.on_local_partial(&result.text).await;
                                }
                            }
                            JobKind::Final => {
                                if result.id == runtime.segment_id {
                                    let pending = pending_finals.remove(&result.id);
                                    let timing = if let Some(pending) = pending {
                                        FinalTiming {
                                            speech_end: pending.speech_end,
                                            vad_decision: pending.vad_decision,
                                            stt_started: pending.stt_started,
                                            stt_done: result.finished_at,
                                            speech_end_epoch_ms: epoch_ms_at(pending.speech_end),
                                        }
                                    } else {
                                        let speech_end = result.speech_end.unwrap_or(result.finished_at);
                                        FinalTiming {
                                            speech_end,
                                            vad_decision: result.submitted_at,
                                            stt_started: result.submitted_at,
                                            stt_done: result.finished_at,
                                            speech_end_epoch_ms: epoch_ms_at(speech_end),
                                        }
                                    };
                                    runtime.on_final(result.text, timing).await;
                                }
                            }
                        }
                    }
                    Some(SttEvent::Error { message, .. }) => runtime.error("stt", message, false).await,
                    Some(SttEvent::Fatal(message)) => {
                        runtime.error("stt", message.clone(), true).await;
                        fatal_error = Some(message);
                        break;
                    }
                    None => {
                        stt_rx = None;
                    }
                }
            }
            event = next_optional(&mut cloud_rx) => {
                match event {
                    Some(crate::stt::CloudSttEvent::Connected) => {
                        runtime.status(UiState::Listening, "connected to Deepgram").await;
                    }
                    Some(crate::stt::CloudSttEvent::Partial { text, .. }) => {
                        runtime.on_cloud_partial(&text).await;
                    }
                    Some(crate::stt::CloudSttEvent::Final { text, received_at }) => {
                        if runtime.paused { continue; }
                        let now = received_at;
                        let (speech_end, vad_decision, stt_started) = match recent_vad {
                            Some((speech_end, at))
                                if now
                                    .checked_duration_since(at)
                                    .is_some_and(|d| d < Duration::from_secs(2)) =>
                            {
                                (speech_end, at, at)
                            }
                            _ => {
                                let approx = now.checked_sub(Duration::from_millis(300)).unwrap_or(now);
                                (approx, now, now)
                            }
                        };
                        let timing = FinalTiming {
                            speech_end,
                            vad_decision,
                            stt_started,
                            stt_done: now,
                            speech_end_epoch_ms: epoch_ms_at(speech_end),
                        };
                        runtime.on_final(text, timing).await;
                    }
                    Some(crate::stt::CloudSttEvent::Error(message)) => {
                        runtime.error("stt", message, false).await;
                    }
                    None => {
                        cloud_rx = None;
                    }
                }
            }
            llm_event = llm_rx.recv() => {
                if let Some(event) = llm_event {
                    runtime.on_llm_event(event).await;
                }
            }
        }
    }

    if let Some(req) = runtime.active.take() {
        req.cancel.cancel();
    }
    if let Some(req) = runtime.speculation.take() {
        req.cancel.cancel();
    }

    tracing::debug!("cleanup: stopping audio");
    audio_guard.shutdown().await;
    tracing::debug!("cleanup: awaiting audio forwarder");
    let _ = forward_handle.await;
    drop(vad_events_rx);
    tracing::debug!("cleanup: joining VAD");
    let _ = tokio::task::spawn_blocking(move || {
        let _ = vad_handle.join();
    })
    .await;
    tracing::debug!("cleanup: stopping STT");
    if let Some(worker) = stt_worker {
        worker.shutdown().await;
    }
    tracing::debug!("cleanup: stopping Deepgram");
    if let Some(handle) = cloud_handle {
        root_cancel.cancel();
        let _ = handle.await;
    }
    tracing::debug!("cleanup complete");

    runtime.status(UiState::Stopped, "stopped").await;

    if let Some(message) = fatal_error {
        return Err(anyhow::anyhow!(message));
    }
    Ok(())
}
