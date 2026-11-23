pub mod agreement;
#[cfg(feature = "deepgram")]
pub mod deepgram;
pub mod model;
mod text;
pub mod whisper;

use std::time::Instant;

pub use agreement::LocalAgreement;
pub use model::{ModelProgress, ensure_model};
pub use text::clean_transcript;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Partial,
    Final,
}

#[derive(Debug)]
pub struct SttJob {
    pub id: u64,
    pub audio: Vec<f32>,
    pub kind: JobKind,
    pub submitted_at: Instant,
    pub speech_end: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct SttResult {
    pub id: u64,
    pub kind: JobKind,
    pub text: String,
    pub submitted_at: Instant,
    pub finished_at: Instant,
    pub inference_ms: u64,
    pub speech_end: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum SttEvent {
    Loading {
        stage: String,
    },
    Ready {
        model: String,
        load_ms: u64,
        warmup_ms: u64,
    },
    Result(SttResult),
    Error {
        job_id: Option<u64>,
        message: String,
    },
    Fatal(String),
}

/// Deepgram streaming events use their own clock: the cloud service performs
/// endpointing. The pipeline correlates them with the local VAD speech end.
#[derive(Debug, Clone)]
pub enum CloudSttEvent {
    Connected,
    Partial { text: String, received_at: Instant },
    Final { text: String, received_at: Instant },
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_silence_markers() {
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(clean_transcript("(silence)"), "");
        assert_eq!(clean_transcript("♪ ♪"), "");
        assert_eq!(clean_transcript("  Привіт,   світе! "), "Привіт, світе!");
    }
}
