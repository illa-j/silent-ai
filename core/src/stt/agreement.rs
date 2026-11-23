use crate::detector::normalize_words;

/// Confirms words that survived two consecutive Whisper hypotheses.
///
/// Partial inference re-transcribes the growing utterance. We compare the new
/// hypothesis with the previous one, take their common prefix, and emit only
/// words beyond the already-confirmed prefix. This hides Whisper's unstable
/// tail and gives the detector a stable partial text to work with.
#[derive(Debug, Default)]
pub struct LocalAgreement {
    previous: Vec<String>,
    committed: Vec<String>,
}

impl LocalAgreement {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns newly confirmed words, if any.
    pub fn update(&mut self, hypothesis: &str) -> Option<String> {
        let words = normalize_words(hypothesis);
        if words.is_empty() {
            return None;
        }
        if self.previous.is_empty() {
            self.previous = words;
            return None;
        }

        let common = self
            .previous
            .iter()
            .zip(words.iter())
            .take_while(|(a, b)| a == b)
            .count();

        if common > self.committed.len() {
            let new_words = words[self.committed.len()..common].to_vec();
            self.committed.extend(new_words.iter().cloned());
            self.previous = words;
            Some(new_words.join(" "))
        } else {
            self.previous = words;
            None
        }
    }

    pub fn committed_text(&self) -> String {
        self.committed.join(" ")
    }

    /// The final transcript is authoritative; it replaces the partial text.
    pub fn finalize(&mut self, text: &str) -> String {
        let text = text.trim().to_owned();
        self.reset();
        text
    }

    pub fn reset(&mut self) {
        self.previous.clear();
        self.committed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_words_only_after_two_matching_runs() {
        let mut agreement = LocalAgreement::new();
        assert_eq!(agreement.update("чому небо"), None);
        assert_eq!(
            agreement.update("чому небо синє"),
            Some("чому небо".to_owned())
        );
        assert_eq!(agreement.committed_text(), "чому небо");
    }

    #[test]
    fn does_not_repeat_committed_words() {
        let mut agreement = LocalAgreement::new();
        agreement.update("як працює");
        assert_eq!(
            agreement.update("як працює рекурсія"),
            Some("як працює".to_owned())
        );
        assert_eq!(
            agreement.update("як працює рекурсія в rust"),
            Some("рекурсія".to_owned())
        );
    }

    #[test]
    fn final_replaces_partial() {
        let mut agreement = LocalAgreement::new();
        agreement.update("чому небо");
        agreement.update("чому небо синє");
        let final_text = agreement.finalize("Чому небо синє?");
        assert_eq!(final_text, "Чому небо синє?");
        assert!(agreement.committed_text().is_empty());
    }
}
