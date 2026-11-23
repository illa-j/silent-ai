use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SilentError};

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Config {
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub stt: SttConfig,
    pub llm: LlmConfig,
    pub detector: DetectorConfig,
    pub budgets: BudgetConfig,
    pub hotkeys: HotkeyConfig,
    pub ui: UiConfig,
    pub stealth: StealthConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    /// Capture what the meeting plays through the speakers (system audio).
    #[default]
    System,
    /// Capture a microphone. Useful when the teacher is in the same room.
    Microphone,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AudioConfig {
    pub mode: AudioMode,
    /// `auto`, a device id from `--list-devices`, a display name, or a unique
    /// case-insensitive substring.
    pub device: String,
    /// `None`/0 keeps the device's native rate.
    pub sample_rate: Option<u32>,
    /// Ring buffer capacity in the audio callback. 2000 ms is plenty and
    /// tolerates a scheduling hiccup without dropping captured audio.
    pub ring_buffer_ms: u32,
    /// Preferred capture frame in milliseconds. The VAD runs on 20 ms chunks.
    pub frame_ms: u32,
    /// Prefer the native PipeWire host when built with `native-pipewire`.
    /// The default Linux build uses PipeWire's PulseAudio compatibility and
    /// captures the monitor source of the current default sink.
    pub linux_prefer_pipewire: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            mode: AudioMode::System,
            device: "auto".to_owned(),
            sample_rate: Some(48_000),
            ring_buffer_ms: 2_000,
            frame_ms: 20,
            linux_prefer_pipewire: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebrtcVadMode {
    #[default]
    Quality,
    LowBitrate,
    Aggressive,
    VeryAggressive,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VadConfig {
    pub webrtc_mode: WebrtcVadMode,
    /// Silence required to close the phrase. 300 ms + one 20 ms frame keeps
    /// the measured endpoint under the 400 ms budget.
    pub silence_ms: u32,
    /// Shorter blips are discarded as noise.
    pub min_speech_ms: u32,
    /// Audio kept before the first voiced frame.
    pub pre_roll_ms: u32,
    /// Hard cap. The segment is finalized if a monologue runs longer.
    pub max_segment_ms: u32,
    /// Minimum audio before the first partial hypothesis is requested.
    pub partial_min_speech_ms: u32,
    /// How often partial hypotheses are requested.
    pub partial_interval_ms: u32,
    /// Maximum audio sent to a partial Whisper run. Finals always use the
    /// complete segment.
    pub partial_window_ms: u32,
    /// Skip VAD entirely below this level (-60 dBFS is near-digital silence).
    pub min_rms_dbfs: f32,
    /// Clean system audio sometimes defeats WebRTC's speech classifier (TTS,
    /// codec artifacts). Above this level, sustained energy is treated as
    /// speech as a fallback. Set to 0.0 to disable the fallback.
    pub energy_fallback_dbfs: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            webrtc_mode: WebrtcVadMode::Quality,
            silence_ms: 600,
            min_speech_ms: 100,
            pre_roll_ms: 400,
            max_segment_ms: 10_000,
            partial_min_speech_ms: 700,
            partial_interval_ms: 800,
            partial_window_ms: 4_000,
            min_rms_dbfs: -58.0,
            energy_fallback_dbfs: -38.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SttBackend {
    #[default]
    Whisper,
    Deepgram,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WhisperConfig {
    /// Model name (`tiny`, `base`, `small`, `distil-large-v3`, ...) or a
    /// path to a ggml `.bin` file.
    pub model: String,
    pub model_dir: PathBuf,
    /// Empty means the official whisper.cpp Hugging Face URL.
    pub model_url: String,
    /// Language code passed to Whisper. `"auto"` detects the language once per
    /// session from the first final of at least two seconds; the detected
    /// language is then reused for the rest of the session.
    pub language: String,
    /// Bias Whisper toward course terms and English spellings. Useful for
    /// programming lectures where terms like "C#", "Singleton", "interface"
    /// must stay in English instead of being transliterated.
    pub initial_prompt: String,
    /// Deterministic post-processing for common Ukrainian transliterations of
    /// English programming terms.
    pub glossary: std::collections::HashMap<String, String>,
    /// 0 = physical core count.
    pub threads: usize,
    /// Only effective if whisper-rs was built with `cuda`, `metal`, `coreml`,
    /// or `vulkan`.
    pub gpu: bool,
    pub flash_attn: bool,
    pub download: bool,
    pub warmup: bool,
    /// Greedy with best_of=1 is the fastest supported sampler.
    pub greedy: bool,
    /// Automatically shrink whisper.cpp's audio context to the audio length.
    /// This cuts encoder time substantially for 2-8 second questions.
    pub adaptive_audio_ctx: bool,
    pub max_tokens: i32,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: "small-q5_1".to_owned(),
            model_dir: default_model_dir(),
            model_url: String::new(),
            language: "uk".to_owned(),
            initial_prompt: "Це лекція з програмування. Терміни англійською: class, object, instance, method, property, field, constructor, interface, inheritance, polymorphism, encapsulation, abstraction, composition, Singleton, factory, array, list, dictionary, stack, heap, thread, async, await, exception, reference, pointer, garbage collector, C#, .NET, Rust, Python.".to_owned(),
            glossary: default_programming_glossary(),
            threads: 0,
            gpu: false,
            flash_attn: false,
            download: true,
            warmup: true,
            greedy: true,
            adaptive_audio_ctx: true,
            max_tokens: 256,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DeepgramConfig {
    pub api_key_env: String,
    /// Fallback file with the key (trimmed); `~` is expanded.
    pub api_key_file: Option<PathBuf>,
    pub model: String,
    pub language: String,
    /// Nova-3 keyterm prompting: course-specific terms and names.
    pub keyterms: Vec<String>,
    /// Deepgram closes an utterance after this much silence.
    pub endpointing_ms: u32,
    pub interim_results: bool,
    pub smart_format: bool,
    pub reconnect_delay_ms: u64,
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            api_key_env: "DEEPGRAM_API_KEY".to_owned(),
            api_key_file: None,
            model: "nova-3".to_owned(),
            language: "uk".to_owned(),
            keyterms: vec![
                "рефлексія".to_owned(),
                "інкапсуляція".to_owned(),
                "поліморфізм".to_owned(),
                "успадкування".to_owned(),
                "абстракція".to_owned(),
                "композиція".to_owned(),
                "інтерфейс".to_owned(),
                "властивість".to_owned(),
                "делегат".to_owned(),
                "лямбда".to_owned(),
                "виняток".to_owned(),
                "багатопотоковість".to_owned(),
                "C#".to_owned(),
                "Rust".to_owned(),
                "Python".to_owned(),
                "Singleton".to_owned(),
            ],
            endpointing_ms: 500,
            interim_results: true,
            smart_format: true,
            reconnect_delay_ms: 500,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SttConfig {
    pub backend: SttBackend,
    pub whisper: WhisperConfig,
    pub deepgram: DeepgramConfig,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            backend: SttBackend::Whisper,
            whisper: WhisperConfig::default(),
            deepgram: DeepgramConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmConfig {
    /// OpenAI-compatible base, e.g. `https://opencode.ai/zen/v1`.
    pub base_url: String,
    pub model: String,
    /// Name of the environment variable that holds the key. Empty string
    /// means "no Authorization header" (useful for localhost endpoints).
    pub api_key_env: String,
    /// Optional file with the key, used when the environment variable is not
    /// set. Handy for desktop launches, where shell exports are absent.
    /// A leading `~` is expanded to the home directory.
    pub api_key_file: Option<PathBuf>,
    pub max_tokens: u32,
    /// Always answer the latest utterance, even when it is a statement or an
    /// explanation. When false, the model may return NO_QUESTION.
    pub answer_all: bool,
    pub temperature: f32,
    pub timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub retry_once: bool,
    pub retry_delay_ms: u64,
    /// Transcript context appended after the static system prompt.
    pub history_secs: u64,
    pub max_history_chars: usize,
    pub warmup: bool,
    /// Extra JSON merged into the request body. Set provider-specific switches
    /// here, e.g. `{ "reasoning_effort": "none" }`.
    pub extra_body: serde_json::Value,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://opencode.ai/zen/go/v1".to_owned(),
            model: "glm-5.3-flash".to_owned(),
            api_key_env: "OPENCODE_API_KEY".to_owned(),
            api_key_file: default_api_key_file(),
            max_tokens: 4096,
            answer_all: true,
            temperature: 0.2,
            timeout_ms: 120_000,
            connect_timeout_ms: 2_500,
            retry_once: true,
            retry_delay_ms: 250,
            history_secs: 60,
            max_history_chars: 4_000,
            warmup: true,
            extra_body: serde_json::json!({ "reasoning_effort": "none" }),
        }
    }
}

impl LlmConfig {
    /// Resolve the API key: environment variable first, then the optional key
    /// file. The key itself is never stored in `config.toml`.
    pub fn api_key(&self) -> Result<Option<String>> {
        let name = self.api_key_env.trim();
        if looks_like_literal_key(name) {
            return Err(anyhow::Error::from(SilentError::Llm(
                "llm.api_key_env holds the API key itself, but it must hold the \
                 NAME of an environment variable. Set api_key_env = \"OPENCODE_API_KEY\" \
                 and either run `export OPENCODE_API_KEY='...'` or put the key into \
                 llm.api_key_file."
                    .to_owned(),
            )));
        }

        let mut env_problem = None;
        if !name.is_empty() {
            match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => {
                    return Ok(Some(value.trim().to_owned()));
                }
                Ok(_) => env_problem = Some(format!("environment variable {name} is empty")),
                Err(_) => env_problem = Some(format!("environment variable {name} is not set")),
            }
        }

        if let Some(path) = &self.api_key_file {
            return read_api_key_file(path).map(Some);
        }

        match env_problem {
            Some(problem) => Err(anyhow::Error::from(SilentError::Llm(format!(
                "{problem}; export your API key into it or set llm.api_key_file"
            )))),
            None => Ok(None),
        }
    }
}

/// `api_key_env` must hold a variable name. Catch the common mistake of
/// pasting the key itself (`oc_sk_...`, `sk-...`, `Bearer ...`).
fn looks_like_literal_key(value: &str) -> bool {
    const PREFIXES: [&str; 7] = ["oc_sk_", "sk-", "sk_", "gsk_", "xai-", "AIza", "hf_"];
    if value.contains(char::is_whitespace) {
        return true;
    }
    PREFIXES.iter().any(|prefix| value.starts_with(prefix))
}

fn default_api_key_file() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("silent-ai").join("api_key"))
}

fn read_api_key_file(path: &Path) -> Result<String> {
    let path = expand_tilde(path);
    let text = std::fs::read_to_string(&path).map_err(|err| {
        anyhow::Error::from(SilentError::Llm(format!(
            "cannot read llm.api_key_file {}: {err}",
            path.display()
        )))
    })?;
    let key = text.trim();
    if key.is_empty() {
        return Err(anyhow::Error::from(SilentError::Llm(format!(
            "llm.api_key_file {} is empty",
            path.display()
        ))));
    }
    Ok(key.to_owned())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DetectorConfig {
    /// Extra question words/phrases on top of the built-in Ukrainian and
    /// English lists.
    pub extra_question_words: Vec<String>,
    pub ignore_phrases: Vec<String>,
    pub speculative: bool,
    /// Minimum words in a partial before a speculative LLM request starts.
    /// One-word partials ("чому") are too ambiguous and only waste a request.
    pub speculation_min_words: usize,
    /// Send every final utterance to the LLM. The model decides whether it is
    /// answer-worthy and returns NO_QUESTION otherwise. When false, the local
    /// heuristic gates requests and statements never reach the model.
    pub send_all: bool,
    pub min_words: usize,
    /// Word-set Jaccard similarity needed to keep a speculative answer after
    /// the final transcript arrives.
    pub similarity_threshold: f32,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            extra_question_words: Vec::new(),
            ignore_phrases: Vec::new(),
            speculative: true,
            speculation_min_words: 3,
            send_all: true,
            min_words: 1,
            similarity_threshold: 0.7,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub vad_ms: u64,
    pub stt_ms: u64,
    pub llm_ttft_ms: u64,
    pub ui_ms: u64,
    pub total_ms: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            vad_ms: 700,
            stt_ms: 300,
            llm_ttft_ms: 600,
            ui_ms: 50,
            total_ms: 1_500,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StealthConfig {
    /// Keep the assistant window out of screen capture/sharing.
    /// Windows uses SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE), macOS sets
    /// NSWindow.sharingType = .none. Linux has no compositor API for this, so
    /// the window is hidden while a PipeWire screen-share stream is detected.
    pub hide_from_capture: bool,
    /// Linux only: hide the window while screen sharing is active. Off by
    /// default: the window should stay visible, and on GNOME/KDE Wayland a
    /// visible window cannot be excluded from a full-screen capture.
    pub auto_hide_on_screen_share: bool,
    /// Linux only: emit `silent://screen-share` so the UI can warn that a
    /// full-screen share will include the assistant window.
    pub notify_on_screen_share: bool,
    /// Linux only: how often to poll `pw-dump` for a screen-share stream.
    pub screen_share_poll_ms: u64,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            hide_from_capture: true,
            auto_hide_on_screen_share: false,
            notify_on_screen_share: false,
            screen_share_poll_ms: 1_500,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub toggle_pause: String,
    pub copy_answer: String,
    pub regenerate: String,
    pub manual_question: String,
    pub toggle_stealth: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle_pause: "CmdOrCtrl+Shift+Space".to_owned(),
            copy_answer: "CmdOrCtrl+Shift+C".to_owned(),
            regenerate: "CmdOrCtrl+Shift+R".to_owned(),
            manual_question: "CmdOrCtrl+Shift+Q".to_owned(),
            toggle_stealth: "CmdOrCtrl+Shift+H".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    pub always_on_top: bool,
    pub width: u32,
    pub height: u32,
    pub opacity: f32,
    pub show_partial: bool,
    /// Linux/Wayland only: run the GTK window through XWayland
    /// (`GDK_BACKEND=x11`). GNOME/KDE Wayland ignore GTK's keep-above for
    /// native Wayland windows, so this is what makes always-on-top work.
    /// Ignored if XWayland (`DISPLAY`) is unavailable.
    pub force_x11_on_wayland: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            always_on_top: true,
            width: 460,
            height: 440,
            opacity: 0.94,
            show_partial: true,
            force_x11_on_wayland: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub console: bool,
    pub file: bool,
    pub dir: PathBuf,
    /// Append every final transcript line to
    /// `<dir>/../transcripts/transcript.log.YYYY-MM-DD`.
    pub transcript: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            console: true,
            file: true,
            dir: default_log_dir(),
            transcript: true,
        }
    }
}

/// Prefer `config.local.toml` over `config.toml` in a directory. The local file
/// is gitignored and may contain GPU, provider, or machine-specific settings.
fn preferred_config(dir: &Path) -> PathBuf {
    let local = dir.join("config.local.toml");
    if local.exists() {
        local
    } else {
        dir.join("config.toml")
    }
}

impl Config {
    /// Load `explicit` if given, otherwise `./config.local.toml`, then
    /// `./config.toml`, then the per-user config directory, otherwise
    /// built-in defaults.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(path) => Some(path.to_path_buf()),
            None => Self::default_config_path().filter(|path| path.exists()),
        };

        let mut config = match path {
            Some(path) if path.exists() => {
                let text =
                    std::fs::read_to_string(&path).map_err(|e| SilentError::io(path.clone(), e))?;
                toml::from_str(&text).map_err(|e| {
                    anyhow::anyhow!(
                        "failed to parse {}: {e}. Compare it with the documented config.toml \
                         or remove the local override",
                        path.display()
                    )
                })?
            }
            Some(path) => {
                return Err(anyhow::anyhow!(
                    "config file {} does not exist",
                    path.display()
                ));
            }
            None => Self::default(),
        };

        config.normalize();
        config.validate()?;
        Ok(config)
    }

    pub fn default_config_path() -> Option<PathBuf> {
        let local = PathBuf::from("config.local.toml");
        if local.exists() {
            return Some(local);
        }
        let local = PathBuf::from("config.toml");
        if local.exists() {
            return Some(local);
        }
        dirs::config_dir().map(|dir| preferred_config(&dir.join("silent-ai")))
    }

    pub fn normalize(&mut self) {
        self.llm.base_url = self.llm.base_url.trim().trim_end_matches('/').to_owned();
        self.audio.device = self.audio.device.trim().to_owned();
        self.stt.whisper.model = self.stt.whisper.model.trim().to_owned();
        self.stt.whisper.language = self.stt.whisper.language.trim().to_owned();
        self.stt.whisper.model_url = self.stt.whisper.model_url.trim().to_owned();
        self.stt.whisper.model_dir = expand_tilde(&self.stt.whisper.model_dir);
        self.logging.dir = expand_tilde(&self.logging.dir);
        self.ui.opacity = self.ui.opacity.clamp(0.2, 1.0);

        if self.audio.sample_rate == Some(0) {
            self.audio.sample_rate = None;
        }
        if self.audio.device.is_empty() {
            self.audio.device = "auto".to_owned();
        }
    }

    pub fn validate(&self) -> Result<()> {
        let err = |msg: &str| Err(anyhow::Error::from(SilentError::Config(msg.to_owned())));

        if self.audio.ring_buffer_ms < 500 || self.audio.ring_buffer_ms > 10_000 {
            return err("audio.ring_buffer_ms must be between 500 and 10000");
        }
        if self.audio.frame_ms != 20 {
            return err(
                "audio.frame_ms must be 20: WebRTC VAD and the streaming pipeline use 20 ms frames",
            );
        }
        if let Some(rate) = self.audio.sample_rate
            && !(8_000..=192_000).contains(&rate)
        {
            return err("audio.sample_rate must be 8000..192000 or omitted");
        }
        if self.vad.silence_ms < 50 || self.vad.silence_ms > 1_500 {
            return err("vad.silence_ms must be between 50 and 1500");
        }
        if self.vad.min_speech_ms < 60 || self.vad.min_speech_ms > 2_000 {
            return err("vad.min_speech_ms must be between 60 and 2000");
        }
        if self.vad.partial_interval_ms < 100 {
            return err("vad.partial_interval_ms must be at least 100");
        }
        if self.vad.max_segment_ms < 1_000 {
            return err("vad.max_segment_ms must be at least 1000");
        }
        if self.vad.min_rms_dbfs > 0.0 || self.vad.min_rms_dbfs < -100.0 {
            return err("vad.min_rms_dbfs must be in -100..0");
        }
        if self.vad.energy_fallback_dbfs > 0.0 || self.vad.energy_fallback_dbfs < -100.0 {
            return err("vad.energy_fallback_dbfs must be in -100..0 (0 disables it)");
        }
        if self.stt.whisper.language.is_empty() {
            return err("stt.whisper.language must not be empty; auto-detection is disabled");
        }
        if self.stt.whisper.model.is_empty() && self.stt.backend == SttBackend::Whisper {
            return err("stt.whisper.model must not be empty");
        }
        if self.stt.whisper.threads > 256 {
            return err("stt.whisper.threads is unreasonably large");
        }
        if self.llm.base_url.is_empty() {
            return err("llm.base_url must not be empty");
        }
        if !(self.llm.base_url.starts_with("http://") || self.llm.base_url.starts_with("https://"))
        {
            return err("llm.base_url must start with http:// or https://");
        }
        if self.llm.model.is_empty() {
            return err("llm.model must not be empty");
        }
        if self.llm.max_tokens == 0 || self.llm.max_tokens > 4_096 {
            return err("llm.max_tokens must be between 1 and 4096");
        }
        if !(0.0..=2.0).contains(&self.llm.temperature) {
            return err("llm.temperature must be between 0.0 and 2.0");
        }
        if self.llm.timeout_ms < 1_000 {
            return err("llm.timeout_ms must be at least 1000");
        }
        if self.detector.min_words == 0 {
            return err("detector.min_words must be at least 1");
        }
        if !(0.1..=1.0).contains(&self.detector.similarity_threshold) {
            return err("detector.similarity_threshold must be in 0.1..=1.0");
        }
        if !(250..=10_000).contains(&self.stealth.screen_share_poll_ms) {
            return err("stealth.screen_share_poll_ms must be between 250 and 10000");
        }
        for (name, hotkey) in [
            ("toggle_pause", &self.hotkeys.toggle_pause),
            ("copy_answer", &self.hotkeys.copy_answer),
            ("regenerate", &self.hotkeys.regenerate),
            ("manual_question", &self.hotkeys.manual_question),
            ("toggle_stealth", &self.hotkeys.toggle_stealth),
        ] {
            if hotkey.trim().is_empty() {
                return Err(anyhow::anyhow!("hotkeys.{name} must not be empty"));
            }
        }
        Ok(())
    }

    pub fn model_path(&self) -> PathBuf {
        let model = &self.stt.whisper.model;
        if model.contains('/') || model.contains('\\') || model.ends_with(".bin") {
            return expand_tilde(Path::new(model));
        }
        self.stt.whisper.model_dir.join(format!("ggml-{model}.bin"))
    }

    pub fn model_url(&self) -> String {
        if !self.stt.whisper.model_url.is_empty() {
            return self.stt.whisper.model_url.clone();
        }
        if self.stt.whisper.model.contains('/')
            || self.stt.whisper.model.contains('\\')
            || self.stt.whisper.model.ends_with(".bin")
        {
            // A local path cannot be derived into an official model URL.
            return String::new();
        }
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
            self.stt.whisper.model
        )
    }

    pub fn stt_threads(&self) -> usize {
        if self.stt.whisper.threads == 0 {
            num_cpus::get_physical().max(1)
        } else {
            self.stt.whisper.threads
        }
    }

    pub fn api_key(&self) -> Result<Option<String>> {
        self.llm.api_key()
    }

    pub fn deepgram_api_key(&self) -> Result<String> {
        let name = self.stt.deepgram.api_key_env.trim();
        let mut env_problem = None;
        if !name.is_empty() {
            match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => {
                    return Ok(value.trim().to_owned());
                }
                Ok(_) => env_problem = Some(format!("environment variable {name} is empty")),
                Err(_) => env_problem = Some(format!("environment variable {name} is not set")),
            }
        }
        if let Some(path) = &self.stt.deepgram.api_key_file {
            let path = expand_tilde(path);
            let text = std::fs::read_to_string(&path).map_err(|err| {
                anyhow::Error::from(SilentError::Stt(format!(
                    "cannot read stt.deepgram.api_key_file {}: {err}",
                    path.display()
                )))
            })?;
            let key = text.trim();
            if key.is_empty() {
                return Err(anyhow::Error::from(SilentError::Stt(format!(
                    "stt.deepgram.api_key_file {} is empty",
                    path.display()
                ))));
            }
            return Ok(key.to_owned());
        }
        Err(anyhow::Error::from(SilentError::Stt(format!(
            "{}; set DEEPGRAM_API_KEY or stt.deepgram.api_key_file",
            env_problem.unwrap_or_else(|| "Deepgram API key is not configured".to_owned())
        ))))
    }
}

fn default_programming_glossary() -> std::collections::HashMap<String, String> {
    [
        ("сінгалтан", "Singleton"),
        ("сінгалтон", "Singleton"),
        ("сінглтон", "Singleton"),
        ("синглтон", "Singleton"),
        ("сі шарп", "C#"),
        ("сі-шарп", "C#"),
        ("c sharp", "C#"),
        ("c-sharp", "C#"),
        ("у сі", "у C#"),
        ("паттерн", "pattern"),
        ("патерн", "pattern"),
        ("інтерфейс", "interface"),
        ("интерфейс", "interface"),
        ("інкапсуляція", "encapsulation"),
        ("инкапсуляция", "encapsulation"),
        ("поліморфізм", "polymorphism"),
        ("успадкування", "inheritance"),
        ("абстрактний клас", "abstract class"),
        ("абстрактного класу", "abstract class"),
        ("абстрактним класом", "abstract class"),
        ("рефлексія", "reflection"),
        ("рефлексії", "reflection"),
        ("рефлексію", "reflection"),
        ("властивість", "property"),
        ("властивості", "property"),
        ("властивістю", "property"),
        ("конструктор", "constructor"),
        ("конструктора", "constructor"),
        ("конструктором", "constructor"),
    ]
    .into_iter()
    .map(|(from, to)| (from.to_owned(), to.to_owned()))
    .collect()
}

fn default_model_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("silent-ai")
        .join("models")
}

fn default_log_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local"))
        .join("silent-ai")
        .join("logs")
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        let config = Config::default();
        config.validate().expect("default config must validate");
    }

    #[test]
    fn linux_screen_share_keeps_window_visible_by_default() {
        let config = Config::default();
        assert!(!config.stealth.auto_hide_on_screen_share);
        // KDE Plasma uses the KWin excludeFromCapture script, so no warning is
        // needed by default. GNOME users can enable the notification.
        assert!(!config.stealth.notify_on_screen_share);
    }

    #[test]
    fn sends_every_utterance_and_uses_ukrainian_by_default() {
        let config = Config::default();
        assert!(config.detector.send_all);
        assert!(config.llm.answer_all);
        assert_eq!(config.stt.whisper.language, "uk");
        assert!(config.vad.max_segment_ms <= 10_000);
    }

    #[test]
    fn model_path_and_url() {
        let mut config = Config::default();
        config.stt.whisper.model = "tiny".to_owned();
        assert!(config.model_path().ends_with("ggml-tiny.bin"));
        assert!(config.model_url().contains("ggml-tiny.bin"));

        config.stt.whisper.model = "/tmp/custom.bin".to_owned();
        assert_eq!(config.model_path(), PathBuf::from("/tmp/custom.bin"));
        assert!(config.model_url().is_empty());
    }

    #[test]
    fn rejects_bad_temperature() {
        let mut config = Config::default();
        config.llm.temperature = 3.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_literal_key_in_api_key_env() {
        let mut config = Config::default();
        config.llm.api_key_env = "oc_sk_deadbeef".to_owned();
        let err = config
            .api_key()
            .expect_err("a literal key in api_key_env must be rejected")
            .to_string();
        assert!(
            err.contains("api_key_env holds the API key itself"),
            "{err}"
        );
    }

    #[test]
    fn reads_api_key_from_file() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "silent-ai-key-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::write(&path, "  test-key-123\n").expect("write key file");

        let mut config = Config::default();
        config.llm.api_key_env = "SILENT_AI_TEST_KEY_UNSET_6B1D".to_owned();
        config.llm.api_key_file = Some(path.clone());
        let key = config.api_key().expect("file key").expect("a key");
        assert_eq!(key, "test-key-123");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_key_file_names_path_and_env_option() {
        let mut config = Config::default();
        config.llm.api_key_env = "SILENT_AI_TEST_KEY_UNSET_6B1D".to_owned();
        config.llm.api_key_file = Some(PathBuf::from("/nonexistent/silent-ai/key"));
        let err = config.api_key().expect_err("missing file").to_string();
        assert!(err.contains("/nonexistent/silent-ai/key"), "{err}");
    }

    #[test]
    fn local_config_takes_priority() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("silent-ai-config-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp config dir");
        std::fs::write(dir.join("config.toml"), "").expect("write base config");
        assert_eq!(preferred_config(&dir), dir.join("config.toml"));
        std::fs::write(dir.join("config.local.toml"), "").expect("write local config");
        assert_eq!(preferred_config(&dir), dir.join("config.local.toml"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repository_config_matches_built_in_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config.toml");
        if !path.exists() {
            return;
        }
        let config = Config::load(Some(&path)).expect("repository config.toml must parse");
        let defaults = Config::default();
        assert_eq!(config.stt.whisper.model, defaults.stt.whisper.model);
        assert_eq!(config.stt.whisper.language, defaults.stt.whisper.language);
        assert_eq!(config.stt.whisper.gpu, defaults.stt.whisper.gpu);
        assert_eq!(config.llm.answer_all, defaults.llm.answer_all);
        assert_eq!(config.llm.max_tokens, defaults.llm.max_tokens);
        assert_eq!(config.vad.silence_ms, defaults.vad.silence_ms);
        assert_eq!(config.detector.send_all, defaults.detector.send_all);
        assert_eq!(config.budgets.vad_ms, defaults.budgets.vad_ms);
    }

    #[test]
    fn repository_config_parses_and_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config.toml");
        if !path.exists() {
            return;
        }
        let config = Config::load(Some(&path)).expect("repository config.toml must parse");
        config
            .validate()
            .expect("repository config.toml must validate");
        assert!(config.stt.whisper.adaptive_audio_ctx);
        assert!(config.llm.max_tokens <= 4096);
    }
}
