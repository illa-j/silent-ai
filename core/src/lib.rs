//! Silent AI core: audio capture, VAD, speech-to-text, question detection,
//! and a streaming OpenAI-compatible LLM client.

pub mod audio;
pub mod config;
pub mod detector;
pub mod error;
pub mod llm;
pub mod metrics;
#[cfg(feature = "test-support")]
pub mod mock_llm;
pub mod pipeline;
pub mod project;
pub mod prompt;
pub mod stt;
pub mod vad;

pub use config::Config;
pub use error::{Result, SilentError};
