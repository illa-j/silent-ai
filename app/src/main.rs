#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use serde::Serialize;
use silent_ai_core::audio::{CpalSource, format_device_list, list_devices};
use silent_ai_core::config::{Config, ProjectConfig, UiConfig};
use silent_ai_core::pipeline::{self, Command, PipelineEvent, UiState};
use silent_ai_core::project::{self, ProjectContext};
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod stealth;

#[derive(Debug, Parser)]
#[command(
    name = "silent-ai",
    version,
    about = "Low-latency meeting copilot for Google Meet"
)]
struct Args {
    /// Path to the config file. If omitted: ./config.local.toml, then
    /// ./config.toml, then the per-user config directory.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Print audio hosts, devices, and loopback hints.
    #[arg(long)]
    list_devices: bool,

    /// Download the configured Whisper model and exit.
    #[arg(long)]
    download_model: bool,

    /// Run the audio → VAD → STT → LLM pipeline in the terminal.
    #[arg(long)]
    headless: bool,

    /// Override logging.level.
    #[arg(long)]
    log_level: Option<String>,

    /// Override stt.whisper.language (`auto`, `uk`, `ru`, `en`, ...).
    #[arg(long)]
    language: Option<String>,

    /// Override llm.model (for example `glm-5.3-flash`).
    #[arg(long)]
    model: Option<String>,
}

fn main() {
    let args = Args::parse();
    if let Err(err) = run(args) {
        eprintln!("silent-ai: {err:#}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let mut config = Config::load(args.config.as_deref())?;
    if let Some(level) = args.log_level {
        config.logging.level = level;
    }
    if let Some(language) = args.language {
        config.stt.whisper.language = language;
    }
    if let Some(model) = args.model {
        config.llm.model = model;
    }

    // Must run before init_logging/GTK start any threads: std::env::set_var is
    // only sound while the process is single-threaded.
    let gui = !args.list_devices && !args.download_model && !args.headless;
    let backend_note = if gui {
        configure_linux_backend(&config.ui)
    } else {
        None
    };

    let _log_guard = init_logging(&config);
    match backend_note {
        Some(LinuxBackendNote::X11) => tracing::info!(
            "Wayland detected: using XWayland so the window stays always-on-top \
             (ui.force_x11_on_wayland = true)"
        ),
        Some(LinuxBackendNote::NoX11) => tracing::warn!(
            "Wayland session without XWayland; the compositor cannot enforce \
             always-on-top for this window"
        ),
        Some(LinuxBackendNote::Native) => tracing::warn!(
            "ui.force_x11_on_wayland = false: native Wayland may ignore \
             always-on-top (ui.always_on_top = true)"
        ),
        None => {}
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        "Silent AI starting"
    );

    if args.list_devices {
        let devices = list_devices()?;
        print!("{}", format_device_list(&devices));
        if devices.is_empty() {
            anyhow::bail!("no audio devices found");
        }
        println!(
            "\nSet [audio] device in config.toml to an ID above (or leave it as \"auto\").\n\
             Linux: auto selects the monitor of the current default sink; pick another monitor \
             if Google Meet plays to a different output. Native PipeWire: build with \
             --features native-pipewire and use `default_sink`.\n\
             Windows: choose the playback device (WASAPI loopback).\n\
             macOS: 14.6+ uses Core Audio taps; 13–14.5 needs BlackHole input."
        );
        return Ok(());
    }

    if args.download_model {
        return download_model(&config);
    }

    if args.headless {
        return run_headless(config);
    }

    run_gui(config)
}

fn init_logging(config: &Config) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let level = if config.logging.level.contains('=') {
        config.logging.level.clone()
    } else {
        format!("{},whisper_rs=warn", config.logging.level)
    };
    let filter = EnvFilter::try_new(level).unwrap_or_else(|err| {
        eprintln!("invalid logging.level: {err}; using info");
        EnvFilter::new("info,whisper_rs=warn")
    });

    let console_layer = config
        .logging
        .console
        .then(|| tracing_subscriber::fmt::layer().with_target(false));

    let mut guard = None;
    let file_layer = if config.logging.file {
        match std::fs::create_dir_all(&config.logging.dir) {
            Ok(()) => {
                let appender =
                    tracing_appender::rolling::daily(&config.logging.dir, "silent-ai.log");
                let (writer, worker_guard) = tracing_appender::non_blocking(appender);
                guard = Some(worker_guard);
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false)
                        .with_target(true),
                )
            }
            Err(err) => {
                eprintln!(
                    "cannot create log directory {}: {err}; logging to console only",
                    config.logging.dir.display()
                );
                None
            }
        }
    } else {
        None
    };

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer);
    if let Err(err) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("failed to initialize logging: {err}");
    }
    guard
}

fn download_model(config: &Config) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let cancel = CancellationToken::new();
        let path = config.model_path();
        let path = silent_ai_core::stt::ensure_model(
            &path,
            &config.model_url(),
            config.stt.whisper.download,
            &cancel,
            |progress| {
                if let Some(total) = progress.total
                    && total > 0
                {
                    eprint!(
                        "\rmodel: {:.1}% ({} / {} MB)",
                        progress.downloaded as f64 * 100.0 / total as f64,
                        progress.downloaded / 1_000_000,
                        total / 1_000_000
                    );
                    let _ = std::io::stderr().flush();
                }
            },
        )
        .await?;
        eprintln!();
        println!("Model ready: {}", path.display());
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

fn run_headless(config: Config) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let (events_tx, mut events_rx) = mpsc::channel(128);
        let (commands_tx, commands_rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let source = CpalSource::new(config.audio.clone());
        let root = cancel.clone();
        let project_context = if load_project_enabled() {
            load_saved_project(&config.project)
                .map(|context| context.text)
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut pipeline_task = tokio::spawn(pipeline::run(
            config,
            Box::new(source),
            events_tx,
            commands_rx,
            root,
            project_context,
        ));

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("\nstopping…");
                    cancel.cancel();
                    let _ = commands_tx.send(Command::Shutdown).await;
                    break;
                }
                event = events_rx.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    print_headless_event(&event);
                }
                result = &mut pipeline_task => {
                    match result {
                        Ok(Ok(())) => return Ok(()),
                        Ok(Err(err)) => return Err(err),
                        Err(err) => return Err(anyhow::anyhow!("pipeline task failed: {err}")),
                    }
                }
            }
        }

        match tokio::time::timeout(Duration::from_secs(10), pipeline_task).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(err)) => Err(anyhow::anyhow!("pipeline task failed: {err}")),
            Err(_) => Err(anyhow::anyhow!("pipeline did not stop within 10 s")),
        }
    })
}

fn print_headless_event(event: &PipelineEvent) {
    match event {
        PipelineEvent::Status { state, detail } => match state {
            UiState::Listening => eprintln!("[listening]"),
            UiState::Thinking => eprintln!("[thinking]"),
            _ => eprintln!("[{state:?}] {detail}"),
        },
        PipelineEvent::TranscriptPartial { text, .. } => {
            eprint!("\r… {text:<78}");
            let _ = std::io::stderr().flush();
        }
        PipelineEvent::TranscriptFinal { text, stt_ms, .. } => {
            eprintln!("\r[stt {stt_ms} ms] {text}");
        }
        PipelineEvent::Question { text, .. } => {
            println!("\nQUESTION: {text}");
            print!("ANSWER: ");
            let _ = std::io::stdout().flush();
        }
        PipelineEvent::AnswerDelta { text, .. } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        PipelineEvent::AnswerStart { latency, .. } => {
            eprintln!(
                "\n[latency] vad={} ms stt={} ms llm_ttft={} ms backend_total={} ms speculative={}",
                latency.vad_ms,
                latency.stt_ms,
                latency.llm_ttft_ms,
                latency.backend_total_ms,
                latency.speculative
            );
        }
        PipelineEvent::AnswerDone {
            incomplete, error, ..
        } => {
            println!();
            if *incomplete {
                eprintln!(
                    "[warning] incomplete answer: {}",
                    error.as_deref().unwrap_or("")
                );
            }
        }
        PipelineEvent::NoQuestion { transcript } => {
            eprintln!("\r[no question] {transcript}");
        }
        PipelineEvent::ModelProgress { downloaded, total } => {
            if let Some(total) = total
                && *total > 0
            {
                eprint!(
                    "\r[model] {:.1}%",
                    *downloaded as f64 * 100.0 / *total as f64
                );
                let _ = std::io::stderr().flush();
            }
        }
        PipelineEvent::Error { stage, message, .. } => {
            eprintln!("\n[{stage}] {message}");
        }
        PipelineEvent::Audio { info } => {
            eprintln!(
                "[audio] {} / {} ({} Hz, {} ch, loopback={})",
                info.host, info.device, info.sample_rate, info.channels, info.loopback
            );
        }
        PipelineEvent::Init { backend, model, .. } => {
            eprintln!("[stt] {backend}: {model}");
        }
        PipelineEvent::Paused { paused } => {
            eprintln!("[paused={paused}]");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProjectInfo {
    path: String,
    name: String,
    files: usize,
    chars: usize,
    truncated: bool,
}

impl From<&ProjectContext> for ProjectInfo {
    fn from(context: &ProjectContext) -> Self {
        Self {
            path: context.root.display().to_string(),
            name: context.name.clone(),
            files: context.files,
            chars: context.chars,
            truncated: context.truncated,
        }
    }
}

struct ProjectRuntime {
    info: Option<ProjectInfo>,
    context: String,
    /// Whether the selected project block is sent to the LLM. Disabling it
    /// keeps the folder selected, so it can be re-enabled without rescanning.
    enabled: bool,
}

impl ProjectRuntime {
    fn new(context: Option<ProjectContext>, enabled: bool) -> Self {
        match context {
            Some(context) => Self {
                info: Some(ProjectInfo::from(&context)),
                context: context.text,
                enabled,
            },
            None => Self {
                info: None,
                context: String::new(),
                enabled,
            },
        }
    }
}

struct AppState {
    config: Config,
    commands: StdMutex<Option<mpsc::Sender<Command>>>,
    project: StdMutex<ProjectRuntime>,
    paused: AtomicBool,
    cancel: CancellationToken,
    started: AtomicBool,
    shutting_down: AtomicBool,
    pipeline_done: Arc<tokio::sync::Notify>,
}

impl AppState {
    fn new(config: Config, project: Option<ProjectContext>, project_enabled: bool) -> Self {
        let project = ProjectRuntime::new(project, project_enabled);
        Self {
            config,
            commands: StdMutex::new(None),
            project: StdMutex::new(project),
            paused: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            started: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            pipeline_done: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn project_snapshot(&self) -> (Option<ProjectInfo>, String, bool) {
        let guard = self
            .project
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard.info.clone(), guard.context.clone(), guard.enabled)
    }

    fn set_project(&self, context: Option<ProjectContext>) -> Option<ProjectInfo> {
        let (info, text) = {
            let mut guard = self
                .project
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match context {
                Some(context) => {
                    let info = ProjectInfo::from(&context);
                    guard.info = Some(info.clone());
                    guard.context = context.text;
                    // Picking a folder is an explicit request to use it.
                    guard.enabled = true;
                    (Some(info), guard.context.clone())
                }
                None => {
                    guard.info = None;
                    guard.context.clear();
                    (None, String::new())
                }
            }
        };
        let _ = self.send(Command::SetProjectContext(text));
        info
    }

    fn set_project_enabled(&self, enabled: bool) -> Result<(), String> {
        let (context, has_project) = {
            let mut guard = self
                .project
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.enabled = enabled;
            (guard.context.clone(), guard.info.is_some())
        };
        if let Err(err) = save_project_enabled(enabled) {
            tracing::warn!(%err, "cannot persist project enabled state");
        }
        if has_project {
            // The pipeline may not be started yet; start_pipeline reads the
            // flag from the state and passes the effective context.
            if let Err(err) = self.send(Command::SetProjectContext(if enabled {
                context
            } else {
                String::new()
            })) {
                tracing::warn!(%err, "cannot update project context in the running pipeline");
            }
        }
        Ok(())
    }

    fn send(&self, command: Command) -> Result<(), String> {
        let guard = self
            .commands
            .lock()
            .map_err(|_| "command state is poisoned".to_owned())?;
        let sender = guard
            .as_ref()
            .ok_or_else(|| "pipeline is not ready yet".to_owned())?;
        sender.try_send(command).map_err(|err| err.to_string())
    }
}

fn silent_ai_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("silent-ai")
}

fn notes_path() -> PathBuf {
    silent_ai_data_dir().join("notes.md")
}

fn project_path_file() -> PathBuf {
    silent_ai_data_dir().join("project_path")
}

fn project_enabled_file() -> PathBuf {
    silent_ai_data_dir().join("project_enabled")
}

fn load_project_enabled() -> bool {
    std::fs::read_to_string(project_enabled_file())
        .ok()
        .map(|raw| !matches!(raw.trim(), "0" | "false" | "off"))
        .unwrap_or(true)
}

fn save_project_enabled(enabled: bool) -> Result<(), String> {
    let file = project_enabled_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create project state dir: {err}"))?;
    }
    std::fs::write(&file, if enabled { b"1" } else { b"0" })
        .map_err(|err| format!("cannot save project state: {err}"))
}

fn load_saved_project(config: &ProjectConfig) -> Option<ProjectContext> {
    let raw = std::fs::read_to_string(project_path_file()).ok()?;
    let path = PathBuf::from(raw.trim());
    if path.as_os_str().is_empty() {
        return None;
    }
    if !path.is_dir() {
        tracing::warn!(path = %path.display(), "saved project folder no longer exists");
        return None;
    }
    match project::scan_project(&path, config) {
        Ok(context) => {
            tracing::info!(
                path = %context.root.display(),
                files = context.files,
                chars = context.chars,
                truncated = context.truncated,
                "loaded saved project context"
            );
            Some(context)
        }
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "cannot scan saved project");
            None
        }
    }
}

fn save_project_path(path: &std::path::Path) -> Result<(), String> {
    let file = project_path_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create project state dir: {err}"))?;
    }
    std::fs::write(&file, path.to_string_lossy().as_bytes())
        .map_err(|err| format!("cannot save project path: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[tauri::command]
fn load_notes() -> Result<String, String> {
    match std::fs::read_to_string(notes_path()) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("cannot read notes: {err}")),
    }
}

#[tauri::command]
fn save_notes(text: String) -> Result<(), String> {
    if text.len() > 4 * 1024 * 1024 {
        return Err("notes are too large (limit 4 MiB)".to_owned());
    }
    let path = notes_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("cannot create notes dir: {err}"))?;
    }
    std::fs::write(&path, text).map_err(|err| format!("cannot write notes: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[tauri::command]
async fn pick_project_folder(app: tauri::AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    dialog.pick_folder(move |path| {
        let _ = tx.send(path);
    });
    let folder = rx.await.ok().flatten()?;
    folder
        .into_path()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn set_project_folder(
    path: String,
    state: State<'_, AppState>,
) -> Result<ProjectInfo, String> {
    let path = PathBuf::from(path);
    if !path.is_dir() {
        return Err(format!("not a folder: {}", path.display()));
    }
    let scan_config = state.config.project.clone();
    let context = tokio::task::spawn_blocking(move || project::scan_project(&path, &scan_config))
        .await
        .map_err(|err| format!("project scan task failed: {err}"))?
        .map_err(|err| format!("cannot scan project: {err:#}"))?;
    save_project_path(&context.root)?;
    if let Err(err) = save_project_enabled(true) {
        tracing::warn!(%err, "cannot persist project enabled state");
    }
    let info = ProjectInfo::from(&context);
    state.set_project(Some(context));
    Ok(info)
}

#[tauri::command]
fn clear_project(state: State<'_, AppState>) -> Result<(), String> {
    let _ = std::fs::remove_file(project_path_file());
    state.set_project(None);
    Ok(())
}

#[tauri::command]
fn set_project_enabled(enabled: bool, state: State<'_, AppState>) -> Result<bool, String> {
    state.set_project_enabled(enabled)?;
    Ok(enabled)
}

#[derive(Serialize)]
struct Bootstrap {
    budgets: silent_ai_core::config::BudgetConfig,
    hotkeys: silent_ai_core::config::HotkeyConfig,
    ui: silent_ai_core::config::UiConfig,
    project: Option<ProjectInfo>,
    project_enabled: bool,
}

#[tauri::command]
fn bootstrap(state: State<'_, AppState>) -> Bootstrap {
    let (project, _, project_enabled) = state.project_snapshot();
    Bootstrap {
        budgets: state.config.budgets.clone(),
        hotkeys: state.config.hotkeys.clone(),
        ui: state.config.ui.clone(),
        project,
        project_enabled,
    }
}

#[tauri::command]
fn toggle_pause(state: State<'_, AppState>) -> Result<bool, String> {
    let was_paused = state.paused.fetch_xor(true, Ordering::SeqCst);
    let now_paused = !was_paused;
    state.send(if now_paused {
        Command::Pause
    } else {
        Command::Resume
    })?;
    Ok(now_paused)
}

#[tauri::command]
fn regenerate(state: State<'_, AppState>) -> Result<(), String> {
    state.send(Command::Regenerate)
}

#[tauri::command]
fn ask_manual(text: String, state: State<'_, AppState>) -> Result<(), String> {
    state.send(Command::AskManual(text))
}

#[tauri::command]
fn report_ui_latency(ui_ms: u64, total_ms: u64) {
    tracing::info!(ui_ms, total_ms, "UI first-token latency");
}

/// Called by the webview after its event listeners are installed, so no
/// startup/progress event can be lost.
#[tauri::command]
fn start_pipeline(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if state.started.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let config = state.config.clone();
    let cancel = state.cancel.clone();
    let done = state.pipeline_done.clone();
    let (events_tx, mut events_rx) = mpsc::channel::<PipelineEvent>(256);
    let (commands_tx, commands_rx) = mpsc::channel::<Command>(16);
    {
        let mut slot = state
            .commands
            .lock()
            .map_err(|_| "command state is poisoned".to_owned())?;
        *slot = Some(commands_tx);
    }

    let transcript_log = if config.logging.transcript {
        let dir = config
            .logging
            .dir
            .parent()
            .map(|parent| parent.join("transcripts"))
            .unwrap_or_else(|| PathBuf::from("transcripts"));
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                tracing::info!(dir = %dir.display(), "writing full transcript");
                Some(Arc::new(StdMutex::new(tracing_appender::rolling::daily(
                    &dir,
                    "transcript.log",
                ))))
            }
            Err(err) => {
                tracing::warn!(%err, dir = %dir.display(), "cannot create transcript dir");
                None
            }
        }
    } else {
        None
    };

    let bridge = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if let Some(log) = &transcript_log
                && let PipelineEvent::TranscriptFinal { text, .. } = &event
                && !text.trim().is_empty()
                && let Ok(mut file) = log.lock()
            {
                let _ = writeln!(file, "{text}");
                let _ = file.flush();
            }
            if let Err(err) = bridge.emit("silent://event", &event) {
                tracing::warn!(%err, "failed to emit pipeline event");
            }
        }
    });

    let error_tx = events_tx.clone();
    let pipeline_app = app.clone();
    let source = CpalSource::new(config.audio.clone());
    let (_, project_context, project_enabled) = state.project_snapshot();
    let project_context = if project_enabled {
        project_context
    } else {
        String::new()
    };
    tauri::async_runtime::spawn(async move {
        if let Err(err) = pipeline::run(
            config,
            Box::new(source),
            events_tx,
            commands_rx,
            cancel,
            project_context,
        )
        .await
        {
            tracing::error!(%err, "pipeline stopped");
            let _ = error_tx
                .send(PipelineEvent::Error {
                    stage: "pipeline".to_owned(),
                    message: err.to_string(),
                    fatal: true,
                })
                .await;
            let _ = pipeline_app.emit(
                "silent://event",
                &PipelineEvent::Status {
                    state: UiState::Error,
                    detail: err.to_string(),
                },
            );
        }
        done.notify_one();
    });

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum HotkeyAction {
    TogglePause,
    Copy,
    Regenerate,
    Manual,
    ToggleStealth,
}

/// GNOME/KDE on Wayland ignore GTK's keep-above for native Wayland windows,
/// so always-on-top silently does nothing there. Running through XWayland makes
/// `_NET_WM_STATE_ABOVE` work. Must run before GTK/Tauri initialization.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxBackendNote {
    X11,
    NoX11,
    Native,
}

/// GNOME/KDE on Wayland ignore GTK's keep-above for native Wayland windows,
/// so always-on-top silently does nothing there. Running through XWayland makes
/// `_NET_WM_STATE_ABOVE` work. Must run before any thread starts.
#[cfg(target_os = "linux")]
fn configure_linux_backend(ui: &UiConfig) -> Option<LinuxBackendNote> {
    std::env::var_os("WAYLAND_DISPLAY")?;
    if !ui.force_x11_on_wayland {
        return ui.always_on_top.then_some(LinuxBackendNote::Native);
    }
    if std::env::var_os("GDK_BACKEND").is_some() {
        return None;
    }
    if std::env::var_os("DISPLAY").is_none() {
        return Some(LinuxBackendNote::NoX11);
    }
    // SAFETY: called from the main thread before logging/GTK start threads.
    unsafe {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    Some(LinuxBackendNote::X11)
}

#[cfg(not(target_os = "linux"))]
fn configure_linux_backend(_ui: &UiConfig) -> Option<LinuxBackendNote> {
    None
}

fn run_gui(config: Config) -> anyhow::Result<()> {
    let project = load_saved_project(&config.project);
    let project_enabled = load_project_enabled();
    let state = AppState::new(config.clone(), project, project_enabled);
    let shortcut_config = config.hotkeys.clone();
    let ui_config = config.ui.clone();
    let stealth_config = config.stealth.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            start_pipeline,
            toggle_pause,
            regenerate,
            ask_manual,
            report_ui_latency,
            load_notes,
            save_notes,
            pick_project_folder,
            set_project_folder,
            clear_project,
            set_project_enabled
        ])
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                if let Err(err) = window.set_always_on_top(ui_config.always_on_top) {
                    tracing::warn!(%err, "failed to set always-on-top");
                }
                let size = tauri::LogicalSize::new(ui_config.width as f64, ui_config.height as f64);
                let _ = window.set_size(size);

                if stealth_config.hide_from_capture {
                    if let Err(err) = stealth::apply_native_capture_exclusion(&window) {
                        tracing::warn!(%err, "native screen-capture exclusion unavailable");
                    }
                    #[cfg(target_os = "linux")]
                    if stealth_config.auto_hide_on_screen_share
                        || stealth_config.notify_on_screen_share
                    {
                        stealth::spawn_screen_share_watcher(
                            app.handle().clone(),
                            stealth_config.screen_share_poll_ms,
                            ui_config.always_on_top,
                            stealth_config.auto_hide_on_screen_share,
                        );
                    }
                }
            }

            register_hotkey(
                app.handle(),
                &shortcut_config.toggle_pause,
                HotkeyAction::TogglePause,
                ui_config.always_on_top,
            );
            register_hotkey(
                app.handle(),
                &shortcut_config.copy_answer,
                HotkeyAction::Copy,
                ui_config.always_on_top,
            );
            register_hotkey(
                app.handle(),
                &shortcut_config.regenerate,
                HotkeyAction::Regenerate,
                ui_config.always_on_top,
            );
            register_hotkey(
                app.handle(),
                &shortcut_config.manual_question,
                HotkeyAction::Manual,
                ui_config.always_on_top,
            );
            register_hotkey(
                app.handle(),
                &shortcut_config.toggle_stealth,
                HotkeyAction::ToggleStealth,
                ui_config.always_on_top,
            );

            Ok(())
        })
        .build(tauri::generate_context!())?
        .run(move |app_handle, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                // A programmatic app.exit(0) after our cleanup must be allowed
                // through; only intercept user-initiated window close/quit.
                if code.is_some() {
                    return;
                }
                let Some(state) = app_handle.try_state::<AppState>() else {
                    return;
                };
                if !state.started.load(Ordering::SeqCst) {
                    return;
                }
                api.prevent_exit();
                if state.shutting_down.swap(true, Ordering::SeqCst) {
                    return;
                }
                state.cancel.cancel();
                let _ = state.send(Command::Shutdown);
                let done = state.pipeline_done.clone();
                let app = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(10), done.notified()).await;
                    app.exit(0);
                });
            } else if let tauri::RunEvent::Exit = event
                && let Some(state) = app_handle.try_state::<AppState>()
            {
                state.cancel.cancel();
            }
        });

    Ok(())
}

fn register_hotkey(
    app: &tauri::AppHandle,
    accelerator: &str,
    action: HotkeyAction,
    always_on_top: bool,
) {
    let handle = app.clone();
    if let Err(err) =
        app.global_shortcut()
            .on_shortcut(accelerator, move |_app, _shortcut, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                let Some(state) = handle.try_state::<AppState>() else {
                    return;
                };
                match action {
                    HotkeyAction::TogglePause => {
                        let was_paused = state.paused.fetch_xor(true, Ordering::SeqCst);
                        let _ = state.send(if was_paused {
                            Command::Resume
                        } else {
                            Command::Pause
                        });
                    }
                    HotkeyAction::Copy => {
                        let _ =
                            handle.emit("silent://hotkey", serde_json::json!({ "action": "copy" }));
                    }
                    HotkeyAction::Regenerate => {
                        let _ = state.send(Command::Regenerate);
                    }
                    HotkeyAction::Manual => {
                        if let Some(window) = handle.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_always_on_top(always_on_top);
                            let _ = window.set_focus();
                        }
                        let _ = handle.emit(
                            "silent://hotkey",
                            serde_json::json!({ "action": "focus_manual" }),
                        );
                    }
                    HotkeyAction::ToggleStealth => {
                        if let Some(window) = handle.get_webview_window("main") {
                            if window.is_visible().unwrap_or(true) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_always_on_top(always_on_top);
                                let _ = window.set_focus();
                            }
                        }
                    }
                }
            })
    {
        tracing::warn!(accelerator, %err, "failed to register global hotkey");
    }
}
