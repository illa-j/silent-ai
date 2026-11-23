use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::BudgetConfig;

/// Per-question latency report. Durations are measured with `Instant`;
/// epoch fields let the webview compute IPC + paint time without sharing a
/// monotonic clock with Rust.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Latency {
    pub vad_ms: u64,
    pub stt_ms: u64,
    pub llm_ttft_ms: u64,
    /// Filled by the frontend after the first token is painted.
    pub ui_ms: u64,
    /// Backend-side end-to-end value; the UI adds its own render time.
    pub backend_total_ms: u64,
    /// End-to-end as shown to the user (backend + render/IPC).
    pub total_ms: u64,
    pub speculative: bool,
    pub speech_end_epoch_ms: u64,
    pub first_token_epoch_ms: u64,
}

impl Latency {
    #[allow(clippy::too_many_arguments)]
    pub fn measure(
        speech_end: Instant,
        vad_decision: Instant,
        stt_started: Instant,
        stt_done: Instant,
        llm_requested: Instant,
        first_visible_token: Instant,
        speculative: bool,
    ) -> Self {
        let vad_ms = ms_between(speech_end, vad_decision);
        let stt_ms = ms_between(stt_started, stt_done);
        let llm_ttft_ms = ms_between(llm_requested, first_visible_token);
        let backend_total_ms = ms_between(speech_end, first_visible_token);

        Self {
            vad_ms,
            stt_ms,
            llm_ttft_ms,
            ui_ms: 0,
            backend_total_ms,
            total_ms: backend_total_ms,
            speculative,
            speech_end_epoch_ms: epoch_ms(),
            first_token_epoch_ms: epoch_ms(),
        }
    }

    pub fn with_epochs(mut self, speech_end_epoch_ms: u64, first_token_epoch_ms: u64) -> Self {
        self.speech_end_epoch_ms = speech_end_epoch_ms;
        self.first_token_epoch_ms = first_token_epoch_ms;
        self
    }

    pub fn set_ui(&mut self, ui_ms: u64) {
        self.ui_ms = ui_ms;
        self.total_ms = self.backend_total_ms.saturating_add(ui_ms);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetStatus {
    pub vad_ok: bool,
    pub stt_ok: bool,
    pub llm_ok: bool,
    pub ui_ok: bool,
    pub total_ok: bool,
}

impl BudgetStatus {
    pub fn check(latency: &Latency, budgets: &BudgetConfig) -> Self {
        Self {
            vad_ok: latency.vad_ms <= budgets.vad_ms,
            stt_ok: latency.stt_ms <= budgets.stt_ms,
            llm_ok: latency.llm_ttft_ms <= budgets.llm_ttft_ms,
            ui_ok: latency.ui_ms <= budgets.ui_ms,
            total_ok: latency.total_ms <= budgets.total_ms,
        }
    }
}

fn ms_between(start: Instant, end: Instant) -> u64 {
    end.checked_duration_since(start)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Convert a monotonic `Instant` in the recent past to a Unix epoch
/// millisecond timestamp. Used so the webview can subtract clocks safely.
pub fn epoch_ms_at(instant: Instant) -> u64 {
    let elapsed = Instant::now()
        .checked_duration_since(instant)
        .unwrap_or_default()
        .as_millis() as u64;
    epoch_ms().saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn stages_add_up_without_speculation() {
        let speech_end = Instant::now();
        thread::sleep(Duration::from_millis(10));
        let vad = Instant::now();
        thread::sleep(Duration::from_millis(10));
        let stt_done = Instant::now();
        thread::sleep(Duration::from_millis(10));
        let first = Instant::now();
        let latency = Latency::measure(speech_end, vad, vad, stt_done, stt_done, first, false);
        assert!(latency.total_ms >= latency.vad_ms + latency.stt_ms + latency.llm_ttft_ms);
        assert!(!latency.speculative);
    }

    #[test]
    fn speculative_total_can_be_less_than_stage_sum() {
        let now = Instant::now();
        let latency = Latency::measure(
            now,
            now + Duration::from_millis(300),
            now + Duration::from_millis(300),
            now + Duration::from_millis(500),
            now - Duration::from_millis(400),
            now + Duration::from_millis(520),
            true,
        );
        assert_eq!(latency.total_ms, 520);
        assert_eq!(latency.llm_ttft_ms, 920);
    }
}
