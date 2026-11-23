use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use silent_ai_core::audio::wav::WavSource;
use silent_ai_core::audio::{AudioSource, CpalSource};
use silent_ai_core::config::{Config, SttBackend};
use silent_ai_core::metrics::{BudgetStatus, epoch_ms};
use silent_ai_core::pipeline::{self, Command, PipelineEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "silent-ai-bench",
    about = "Feed a WAV question through the full pipeline and print per-stage latency"
)]
struct Args {
    /// WAV file with a question (16 kHz mono recommended; other rates are resampled).
    /// Not needed with --live.
    #[arg(long)]
    wav: Option<PathBuf>,

    /// Capture live system audio instead of a WAV. Play a question while it listens.
    #[arg(long)]
    live: bool,

    /// Config file. Defaults to ./config.local.toml, ./config.toml, or built-in defaults.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Playback speed. 1.0 is real time; >1 only for smoke tests.
    #[arg(long, default_value_t = 1.0)]
    speed: f64,

    /// Start an in-process mock OpenAI SSE server (no API key needed).
    #[arg(long)]
    mock_llm: bool,

    /// With --mock-llm, delay the first content token by this many milliseconds.
    #[arg(long, default_value_t = 0)]
    mock_ttft_ms: u64,

    /// Override stt.whisper.model.
    #[arg(long)]
    model: Option<String>,

    /// Override stt.whisper.language.
    #[arg(long)]
    language: Option<String>,

    /// Abort if no complete answer arrives in this many seconds.
    #[arg(long, default_value_t = 90)]
    timeout_secs: u64,

    /// Print machine-readable JSON events instead of a human report.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,whisper_rs=warn")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    if !args.live {
        let wav = args
            .wav
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--wav is required unless --live is set"))?;
        if !wav.exists() {
            anyhow::bail!("WAV file {} does not exist", wav.display());
        }
    }
    if args.speed <= 0.0 {
        anyhow::bail!("--speed must be positive");
    }

    let mut config = Config::load(args.config.as_deref())?;
    if let Some(model) = args.model {
        config.stt.whisper.model = model;
    }
    if let Some(language) = args.language {
        config.stt.whisper.language = language;
    }
    if config.stt.backend == SttBackend::Whisper && config.stt.whisper.model.is_empty() {
        config.stt.whisper.model = "tiny-q5_1".to_owned();
    }

    let mock = if args.mock_llm {
        let mock = silent_ai_core::mock_llm::MockLlm::spawn_with_delay(
            "Because of Rayleigh scattering. Air molecules scatter shorter wavelengths more strongly, so blue light reaches your eyes from every direction.",
            Duration::from_millis(args.mock_ttft_ms),
        )
        .await?;
        config.llm.base_url = mock.base_url.clone();
        config.llm.model = "mock".to_owned();
        config.llm.api_key_env.clear();
        config.llm.warmup = true;
        Some(mock)
    } else {
        None
    };

    let source: Box<dyn AudioSource> = if args.live {
        eprintln!("Listening to system audio. Play the question now…");
        Box::new(CpalSource::new(config.audio.clone()))
    } else {
        let wav = args
            .wav
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--wav is required unless --live is set"))?;
        Box::new(WavSource::new(wav).with_speed(args.speed))
    };
    let (events_tx, mut events_rx) = mpsc::channel(128);
    let (commands_tx, commands_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let root = cancel.clone();
    let mut pipeline = tokio::spawn(pipeline::run(config, source, events_tx, commands_rx, root));

    let deadline = tokio::time::sleep(Duration::from_secs(args.timeout_secs.max(5)));
    tokio::pin!(deadline);

    let mut budgets = None;
    let mut answer = String::new();
    let mut latency = None;
    let mut done = false;
    let mut no_question = false;
    let mut failed = false;
    let mut pipeline_exit = None;

    loop {
        tokio::select! {
            _ = &mut deadline => {
                eprintln!("[bench] timeout after {} s", args.timeout_secs);
                failed = true;
                break;
            }
            event = events_rx.recv() => {
                match event {
                    Some(event) => {
                        if args.json {
                            if let Ok(line) = serde_json::to_string(&event) {
                                println!("{line}");
                            }
                        } else {
                            print_event(&event, &mut answer);
                        }
                        match event {
                            PipelineEvent::Init { budgets: b, .. } => budgets = Some(b),
                            PipelineEvent::AnswerStart { latency: mut l, .. } => {
                                let ipc_ms = epoch_ms().saturating_sub(l.first_token_epoch_ms);
                                l.ui_ms = ipc_ms;
                                l.total_ms = l.backend_total_ms + ipc_ms;
                                latency = Some(l);
                            }
                            PipelineEvent::AnswerDone { full_text, incomplete, error, .. } => {
                                if !args.json {
                                    println!();
                                }
                                answer = full_text;
                                done = true;
                                if incomplete {
                                    eprintln!("[bench] answer truncated: {}", error.unwrap_or_default());
                                }
                                break;
                            }
                            PipelineEvent::NoQuestion { .. } => {
                                no_question = true;
                                break;
                            }
                            PipelineEvent::Error { message, fatal, .. } => {
                                failed = true;
                                eprintln!("[bench] pipeline error{fatal_marker}: {message}", fatal_marker = if fatal { " (fatal)" } else { "" });
                                if fatal {
                                    break;
                                }
                            }
                            PipelineEvent::Status { state, detail } if !args.json => {
                                println!("[state] {state:?}: {detail}");
                            }
                            _ => {}
                        }
                    }
                    None => break,
                }
            }
            result = &mut pipeline => {
                pipeline_exit = Some(result);
                break;
            }
        }
    }

    let _ = commands_tx.send(Command::Shutdown).await;
    let _ = tokio::time::timeout(Duration::from_secs(15), pipeline).await;
    if let Some(mock) = mock {
        mock.shutdown().await;
    }
    cancel.cancel();

    if args.json {
        let report = serde_json::json!({
            "answer": answer,
            "latency": latency,
            "done": done,
            "no_question": no_question,
            "failed": failed,
        });
        println!("{report}");
    } else if let Some(latency) = latency {
        println!("\n=== latency ===");
        println!(
            "VAD endpoint      {:>6} ms (budget {})",
            latency.vad_ms,
            budgets.as_ref().map_or(400, |b| b.vad_ms)
        );
        println!(
            "final STT         {:>6} ms (budget {})",
            latency.stt_ms,
            budgets.as_ref().map_or(300, |b| b.stt_ms)
        );
        println!(
            "LLM first token   {:>6} ms (budget {})",
            latency.llm_ttft_ms,
            budgets.as_ref().map_or(600, |b| b.llm_ttft_ms)
        );
        println!(
            "UI event/render   {:>6} ms (budget {})",
            latency.ui_ms,
            budgets.as_ref().map_or(50, |b| b.ui_ms)
        );
        println!(
            "TOTAL             {:>6} ms (budget {})",
            latency.total_ms,
            budgets.as_ref().map_or(1500, |b| b.total_ms)
        );
        println!("speculative: {}", latency.speculative);
        if let Some(budgets) = budgets {
            let status = BudgetStatus::check(&latency, &budgets);
            println!(
                "budget: vad={} stt={} llm={} ui={} total={}",
                ok(status.vad_ok),
                ok(status.stt_ok),
                ok(status.llm_ok),
                ok(status.ui_ok),
                ok(status.total_ok)
            );
        }
        println!("answer: {answer}");
    } else if no_question {
        println!("No question detected in the WAV file.");
    } else {
        println!("No answer was produced.");
    }

    if let Some(exit) = pipeline_exit {
        match exit {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                eprintln!("[bench] pipeline failed: {err}");
                failed = true;
            }
            Err(err) => {
                eprintln!("[bench] pipeline task failed: {err}");
                failed = true;
            }
        }
    }

    if failed || !done {
        anyhow::bail!("benchmark did not produce a complete answer");
    }
    Ok(())
}

#[allow(clippy::collapsible_match)]
fn print_event(event: &PipelineEvent, answer: &mut String) {
    match event {
        PipelineEvent::Init {
            backend,
            model,
            budgets,
            ..
        } => {
            println!("backend={backend} model={model}");
            println!(
                "budgets: vad {} ms / stt {} ms / llm {} ms / ui {} ms / total {} ms",
                budgets.vad_ms,
                budgets.stt_ms,
                budgets.llm_ttft_ms,
                budgets.ui_ms,
                budgets.total_ms
            );
        }
        PipelineEvent::Audio { info } => {
            println!(
                "audio: {} — {} ({} Hz, {} ch)",
                info.host, info.device, info.sample_rate, info.channels
            );
        }
        PipelineEvent::TranscriptPartial { text, .. } => {
            print!("\r[partial] {text:<80}");
            let _ = std::io::stdout().flush();
        }
        PipelineEvent::TranscriptFinal { text, stt_ms, .. } => {
            println!("\r[final {stt_ms} ms] {text}");
        }
        PipelineEvent::Question { text, .. } => {
            println!("\nQUESTION: {text}");
            answer.clear();
        }
        PipelineEvent::AnswerStart {
            latency, question, ..
        } => {
            println!(
                "ANSWER START (question: {question}) backend_total={} ms vad={} stt={} llm_ttft={} speculative={}",
                latency.backend_total_ms,
                latency.vad_ms,
                latency.stt_ms,
                latency.llm_ttft_ms,
                latency.speculative
            );
            print!("ANSWER: ");
            let _ = std::io::stdout().flush();
        }
        PipelineEvent::AnswerDelta { text, .. } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
            answer.push_str(text);
        }
        PipelineEvent::AnswerDone { .. } => {}
        PipelineEvent::NoQuestion { transcript } => {
            println!("NO_QUESTION: {transcript}");
        }
        PipelineEvent::Error { stage, message, .. } => {
            eprintln!("\n[{stage}] {message}");
        }
        PipelineEvent::ModelProgress { downloaded, total } => {
            if let Some(total) = total
                && *total > 0
            {
                eprint!(
                    "\r[model] {:.1}% ({} / {} MB)",
                    *downloaded as f64 * 100.0 / *total as f64,
                    downloaded / 1_000_000,
                    total / 1_000_000
                );
            }
        }
        PipelineEvent::Paused { paused } => {
            println!("[paused={paused}]");
        }
        PipelineEvent::Status { .. } => {}
    }
}

fn ok(value: bool) -> &'static str {
    if value { "PASS" } else { "FAIL" }
}
