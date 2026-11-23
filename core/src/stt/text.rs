/// Removes Whisper's non-speech artefacts without touching legitimate
/// punctuation in the transcript.
pub fn clean_transcript(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut bracket = 0usize;
    let mut special = false;

    for ch in raw.chars() {
        if special {
            if ch == '>' {
                special = false;
            }
            continue;
        }
        match ch {
            '<' => special = true,
            '[' => bracket = bracket.saturating_add(1),
            ']' => bracket = bracket.saturating_sub(1),
            '♪' | '♫' => text.push(' '),
            _ if bracket > 0 => {}
            _ => text.push(ch),
        }
    }

    let mut cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if looks_like_hallucination(&cleaned) {
        return String::new();
    }
    cleaned = collapse_periodic(&cleaned);
    cleaned = collapse_repeated_token(&cleaned);
    cleaned = cleaned
        .trim_matches(|c: char| c == '-' || c == '–' || c == '—')
        .trim()
        .to_owned();

    // Markers sometimes arrive wrapped in parentheses: "(silence)".
    let lower = cleaned
        .trim_matches(|c: char| c == '(' || c == ')' || c == '[' || c == ']' || c.is_whitespace())
        .to_lowercase();
    let markers = [
        "blank_audio",
        "blank audio",
        "silence",
        "music",
        "applause",
        "no speech",
        "субтитри",
        "субтитры",
        "продолжение следует",
        "дякую за перегляд",
        "спасибо за просмотр",
    ];
    if markers.iter().any(|marker| lower == *marker) {
        return String::new();
    }
    // Whisper's Ukrainian/Russian YouTube hallucination credits. These are
    // never part of a lecture question and only add junk to the context.
    const HALLUCINATED_CREDITS: [&str; 6] = [
        "редактор субтитрів",
        "редактор субтитров",
        "підпишіться на канал",
        "подпишитесь на канал",
        "ставте лайк",
        "ставь лайк",
    ];
    if HALLUCINATED_CREDITS
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return String::new();
    }
    if cleaned
        .chars()
        .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
    {
        return String::new();
    }

    cleaned
}

/// Whisper occasionally loops on music/noise, emitting the same token dozens
/// of times ("селі, селі, селі..."). Such an output has almost no information
/// and would poison the question detector and history.
fn looks_like_hallucination(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() < 5 {
        return false;
    }
    let mut counts = std::collections::HashMap::<&str, usize>::new();
    for word in &words {
        *counts.entry(word.as_str()).or_insert(0) += 1;
    }
    let unique = counts.len();
    let most_common = counts.values().copied().max().unwrap_or(0);
    let ratio = most_common as f32 / words.len() as f32;
    let unique_ratio = unique as f32 / words.len() as f32;
    ratio > 0.6 || unique_ratio < 0.3
}

fn normalize_word(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Collapse a single word repeated back-to-back more than four times. Keeping
/// four copies preserves legitimate emphasis while cutting runaway loops.
fn collapse_repeated_token(text: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let mut previous = String::new();
    let mut run = 0usize;
    for word in text.split_whitespace() {
        let normalized = normalize_word(word);
        if !normalized.is_empty() && normalized == previous {
            run += 1;
        } else {
            run = 1;
            previous = normalized;
        }
        if run <= 4 {
            output.push(word);
        }
    }
    output.join(" ")
}

/// Whisper sometimes loops on clean short clips without timestamps. Collapse
/// an exactly periodic word sequence to one period (e.g. a question repeated
/// three times becomes one question).
fn collapse_periodic(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let len = words.len();
    if len < 4 {
        return text.to_owned();
    }
    for period in 2..=len / 2 {
        if (period..len).all(|index| words[index] == words[index % period]) {
            return words[..period].join(" ");
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_special_tokens() {
        assert_eq!(clean_transcript(" <|startoftranscript|> hello"), "hello");
        assert_eq!(
            clean_transcript("[Music] why is the sky blue?"),
            "why is the sky blue?"
        );
    }

    #[test]
    fn keeps_punctuation() {
        assert_eq!(
            clean_transcript("Що таке Rust? Це мова програмування."),
            "Що таке Rust? Це мова програмування."
        );
    }

    #[test]
    fn removes_lone_markers() {
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(clean_transcript("♪"), "");
    }

    #[test]
    fn drops_repeated_token_hallucination() {
        let looped = "Тобі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, селі, с";
        assert_eq!(clean_transcript(looped), "");
    }

    #[test]
    fn drops_fake_subtitle_credit() {
        assert_eq!(clean_transcript("Редактор субтитрів А. Дяченко"), "");
        assert_eq!(clean_transcript("Підпишіться на канал, ставте лайк"), "");
    }

    #[test]
    fn collapses_repeated_question_loop() {
        assert_eq!(
            clean_transcript("Why is the sky blue? Why is the sky blue? Why is the sky blue?"),
            "Why is the sky blue?"
        );
    }
}
