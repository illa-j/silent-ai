use crate::config::DetectorConfig;

const BUILTIN_QUESTION_WORDS: &[&str] = &[
    // Ukrainian
    "чому",
    "як",
    "що",
    "який",
    "яка",
    "яке",
    "які",
    "яких",
    "якому",
    "якій",
    "якою",
    "якими",
    "чого",
    "чим",
    "коли",
    "де",
    "хто",
    "чи",
    "скільки",
    "котрий",
    "котра",
    "котре",
    "котрі",
    "навіщо",
    "звідки",
    "куди",
    "поясніть",
    "поясни",
    "наведіть",
    "наведи",
    "опишіть",
    "скажіть",
    "розкажіть",
    "порівняйте",
    "сформулюйте",
    "доведіть",
    "обґрунтуйте",
    "назвіть",
    // English
    "why",
    "how",
    "what",
    "what's",
    "whats",
    "how's",
    "hows",
    "where's",
    "wheres",
    "who's",
    "whos",
    "why's",
    "whys",
    "which",
    "when",
    "where",
    "who",
    "whom",
    "whose",
    "explain",
    "describe",
    "define",
    "compare",
    "give",
    "tell",
    "show",
    "list",
    "can",
    "could",
    "would",
    "should",
    "is",
    "are",
    "does",
    "do",
    "did",
    "will",
    "have",
    "has",
    // Russian (many Ukrainian courses mix languages)
    "почему",
    "как",
    "что",
    "какой",
    "какая",
    "какое",
    "какие",
    "когда",
    "где",
    "кто",
    "сколько",
    "зачем",
    "откуда",
    "куда",
    "объясните",
    "приведите",
    "расскажите",
    "назовите",
    "опишите",
    "сравните",
];

const LEADING_FILLERS: &[&str] = &[
    "ну",
    "так",
    "отже",
    "значить",
    "тепер",
    "зараз",
    "добре",
    "окей",
    "ок",
    "далі",
    "до речі",
    "итак",
    "значит",
    "сейчас",
    "хорошо",
    "ладно",
    "дальше",
    "короче",
    "well",
    "so",
    "okay",
    "ok",
    "now",
    "then",
    "right",
];

const QUESTION_PHRASES: &[&str] = &[
    "у чому",
    "в чому",
    "яким чином",
    "яким способом",
    "з якою метою",
    "для чого",
    "в чем",
    "каким образом",
    "с какой целью",
    "для чего",
    "что такое",
    "кто такой",
    "как вы думаете",
    "как вы считаете",
    "как гадаєте",
    "як ви думаєте",
    "як вважаєте",
    "what is",
    "what are",
    "how does",
    "how do",
    "why does",
    "why do",
    "what do you think",
    "does anyone",
    "can anyone",
    "who can",
    "could you explain",
    "can you explain",
];

const BUILTIN_IGNORE_PHRASES: &[&str] = &[
    "всі мене чують",
    "все мене чують",
    "мене чути",
    "мене чутно",
    "чути мене",
    "чуєте мене",
    "ви мене чуєте",
    "перевірка звуку",
    "перевірка мікрофона",
    "тест тест",
    "раз два три",
    "can you hear me",
    "do you hear me",
    "hear me",
    "check check",
    "testing testing",
    "test test",
    "one two three",
    "can everyone hear",
    "is my mic",
    "мікрофон працює",
];

/// Heuristic question detector that runs on partial text with zero network
/// calls. It deliberately errs on the side of starting a speculative request:
/// the final LLM answer can still return `NO_QUESTION`, and the pipeline never
/// renders speculative tokens before the final transcript.
pub struct QuestionDetector {
    extra_words: Vec<String>,
    ignore_phrases: Vec<String>,
    min_words: usize,
    similarity_threshold: f32,
}

impl QuestionDetector {
    pub fn new(config: &DetectorConfig) -> Self {
        let ignore_phrases = BUILTIN_IGNORE_PHRASES
            .iter()
            .map(|s| normalize_text(s))
            .chain(config.ignore_phrases.iter().map(|s| normalize_text(s)))
            .collect();

        Self {
            extra_words: config
                .extra_question_words
                .iter()
                .map(|s| normalize_text(s))
                .collect(),
            ignore_phrases,
            min_words: config.min_words.max(1),
            similarity_threshold: config.similarity_threshold,
        }
    }

    pub fn is_question(&self, text: &str) -> bool {
        let words = normalize_words(text);
        if words.len() < self.min_words {
            return false;
        }

        let normalized = words.join(" ");
        if self
            .ignore_phrases
            .iter()
            .any(|phrase| !phrase.is_empty() && normalized.contains(phrase))
        {
            return false;
        }

        if text.contains('?') || text.contains('？') {
            return true;
        }

        // Teachers often start with a filler ("ну", "отже", "итак", "so").
        // Strip those before looking at the head; otherwise a question word
        // lands at position 2-3 and is missed.
        let effective: Vec<String> = words
            .iter()
            .skip_while(|word| LEADING_FILLERS.contains(&word.as_str()))
            .cloned()
            .collect();
        let effective = if effective.is_empty() {
            &words
        } else {
            &effective
        };

        // Question words/imperatives at the start are high signal. A question
        // word buried mid-sentence is often a subordinate clause ("я думаю, що ...").
        let head: String = effective
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let head2: String = effective
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");

        if self.starts_with_question_word(&head) || self.starts_with_question_word(&head2) {
            return true;
        }

        // Teachers often introduce a question with a noun phrase before the
        // interrogative ("Приватне поле, як би ти..."). Look a little deeper,
        // but only for unambiguous interrogatives.
        const STRONG_WORDS: &[&str] = &[
            "чому",
            "навіщо",
            "скільки",
            "коли",
            "де",
            "хто",
            "куди",
            "звідки",
            "why",
            "how",
            "what",
            "when",
            "where",
            "who",
            "which",
        ];
        if effective
            .iter()
            .take(4)
            .any(|word| STRONG_WORDS.contains(&word.as_str()))
        {
            return true;
        }

        // "Не могли б ви ...", "чи не ...", "could you ..."
        let start = effective
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if QUESTION_PHRASES
            .iter()
            .any(|phrase| start.starts_with(phrase))
        {
            return true;
        }
        const POLITE_STARTS: &[&str] = &[
            "чи не",
            "не могли б",
            "не могли б ви",
            "could you",
            "can you",
            "would you",
            "could you please",
            "можете пояснити",
            "можеш пояснити",
        ];
        if POLITE_STARTS.iter().any(|p| start.starts_with(p)) {
            return true;
        }

        // Tag question at the end: "... , чи не так?" usually has a '?', but
        // Whisper sometimes drops punctuation.
        let tail: String = words
            .iter()
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        const TAGS: &[&str] = &["чи не так", "правильно", "вірно", "right", "correct"];
        TAGS.iter().any(|tag| tail.ends_with(tag))
    }

    fn starts_with_question_word(&self, head: &str) -> bool {
        BUILTIN_QUESTION_WORDS.iter().any(|word| {
            head == *word
                || head
                    .strip_prefix(word)
                    .is_some_and(|rest| rest.starts_with(' '))
        }) || self.extra_words.iter().any(|word| {
            head == word
                || head
                    .strip_prefix(word.as_str())
                    .is_some_and(|rest| rest.starts_with(' '))
        })
    }

    /// True when the final transcript is close enough to the speculative
    /// transcript that its already-streamed answer is still valid.
    pub fn compatible(&self, speculative: &str, final_text: &str) -> bool {
        let a = normalize_words(speculative);
        let b = normalize_words(final_text);
        if a.is_empty() || b.is_empty() {
            return false;
        }

        let set_a: std::collections::HashSet<&String> = a.iter().collect();
        let set_b: std::collections::HashSet<&String> = b.iter().collect();
        let intersection = set_a.intersection(&set_b).count();
        let union = set_a.union(&set_b).count().max(1);
        let jaccard = intersection as f32 / union as f32;

        let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
        let prefix_ratio = prefix as f32 / a.len().min(b.len()).max(1) as f32;

        let length_delta = a.len().abs_diff(b.len());
        // A short prefix is not enough: "скільки буде два" can be the start of
        // a very different question. Only trust the prefix shortcut when both
        // sides are substantial and nearly identical.
        let prefix_ok = a.len() >= 4 && b.len() >= 4 && prefix_ratio >= 0.9 && length_delta <= 2;
        jaccard >= self.similarity_threshold || prefix_ok
    }
}

pub fn normalize_text(text: &str) -> String {
    normalize_words(text).join(" ")
}

pub fn normalize_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        let keep = ch.is_alphanumeric() || ch == '\'' || ch == '’' || ch == '-';
        if keep {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.retain(|w| !w.is_empty());
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DetectorConfig;

    fn detector() -> QuestionDetector {
        QuestionDetector::new(&DetectorConfig::default())
    }

    #[test]
    fn detects_ukrainian_questions() {
        let d = detector();
        assert!(d.is_question("Чому небо синє?"));
        assert!(d.is_question("Поясніть, будь ласка, різницю між TCP і UDP"));
        assert!(d.is_question("Як працює рекурсія"));
        assert!(d.is_question("Наведіть приклад використання мьютекса"));
    }

    #[test]
    fn detects_english_questions() {
        let d = detector();
        assert!(d.is_question("Why is the sky blue?"));
        assert!(d.is_question("Why?"));
        assert!(d.is_question("Can you explain borrow checking"));
        assert!(d.is_question("What is the difference between a process and a thread"));
    }

    #[test]
    fn ignores_meeting_checks_and_statements() {
        let d = detector();
        assert!(!d.is_question("Всі мене чують?"));
        assert!(!d.is_question("Can you hear me?"));
        assert!(!d.is_question("Сьогодні ми розглянемо три види сортування"));
        assert!(!d.is_question("Я думаю, що це твердження не є запитанням"));
        assert!(!d.is_question("Hello"));
    }

    #[test]
    fn compatibility_tracks_small_stt_revisions() {
        let d = detector();
        assert!(d.compatible("чому небо синє", "Чому небо синє?"));
        assert!(!d.compatible("як працює рекурсія", "Як працює рекурсія в Rust"));
        assert!(d.compatible("чому небо синє насправді", "Чому небо синє насправді?"));
        assert!(!d.compatible("чому небо синє", "що таке інкапсуляція"));
    }

    #[test]
    fn normalization_is_unicode_friendly() {
        assert_eq!(
            normalize_words("Що, це — \"Rust\"?!"),
            vec!["що", "це", "rust"]
        );
    }
}
