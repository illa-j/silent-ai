use std::path::PathBuf;

/// Domain errors. Most fallible APIs return [`anyhow::Result`] with one of these
/// as the source, so callers can inspect the category without losing context.
#[derive(Debug, thiserror::Error)]
pub enum SilentError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("audio error: {0}")]
    Audio(String),

    #[error("speech-to-text error: {0}")]
    Stt(String),

    #[error("language model error: {0}")]
    Llm(String),

    #[error("model file error: {0}")]
    Model(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = anyhow::Result<T>;

impl SilentError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
