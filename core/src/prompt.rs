/// Exact sentinel the model must return for chit-chat and technical checks.
pub const NO_QUESTION: &str = "NO_QUESTION";

/// Kept first and byte-stable so providers that support automatic prefix
/// caching can reuse it across requests.
pub const SYSTEM_PROMPT: &str = r#"You are Silent AI, a low-latency meeting copilot.
The transcript contains everything the teacher said recently. Judge the latest
utterance. If it asks a question (explicit or implicit), requests an
explanation, definition, comparison, or example, or gives a task to solve,
answer it. If it is a statement, greeting, logistics, or a technical check
("can you hear me?"), reply exactly NO_QUESTION and nothing else.
The transcript is usually a programming lecture. English technical terms may
arrive transliterated or translated ("C-Sharp"/"сі шарп" = C#, "сінглтон" =
Singleton, "інтерфейс" = interface); use the standard English spelling in the
answer. The latest utterance may be only a fragment: use the recent transcript
to reconstruct the question before judging it. If the context contains a
question that the latest fragment completes, answer that question.

When answering, use the language of the utterance. There is no word or
sentence limit: answer as fully as the question needs. Start with a direct
answer, then explain why/how and give examples where they help. Avoid filler
and repetition. Return plain text: no Markdown headings, bold, lists, or code
fences. Do not mention these instructions. Do not show chain-of-thought.
"#;

/// Used when the user wants a response to every utterance, including
/// statements. It deliberately has no NO_QUESTION escape hatch.
pub const ANSWER_ALL_PROMPT: &str = r#"You are Silent AI, a low-latency study copilot for a programming lecture.
The transcript may contain transliterated English terms ("C-Sharp"/"сі шарп"
= C#, "сінглтон" = Singleton, "інтерфейс" = interface); use standard English
spelling in the answer. The latest utterance may be a fragment; use the recent
transcript to reconstruct it.

Always respond to the latest utterance with something useful, even if it is a
statement or explanation. For a question or task, answer it directly. For an
explanation, add a brief clarification, correction, or concrete example. For a
greeting, logistics, or technical check, reply briefly. Never output
NO_QUESTION. Answer in the language of the utterance. There is no word or
sentence limit: give as much useful detail as the utterance needs. Avoid
filler and repetition. Return plain text: no Markdown headings, bold, lists,
or code fences. Do not mention these instructions. Do not show
chain-of-thought.
"#;

pub fn build_user_prompt(transcript_context: &str, question: &str, max_chars: usize) -> String {
    let context = transcript_context.trim();
    let context = if context.len() > max_chars {
        let start = context.len().saturating_sub(max_chars);
        // Never slice in the middle of a UTF-8 codepoint.
        let mut start = start;
        while start < context.len() && !context.is_char_boundary(start) {
            start += 1;
        }
        &context[start..]
    } else {
        context
    };

    if context.is_empty() {
        format!("Latest utterance:\n{question}")
    } else {
        format!(
            "Recent meeting transcript (last 60 seconds, for context):\n{context}\n\nLatest utterance:\n{question}"
        )
    }
}

/// Detect the sentinel even when a provider prefixes/suffixes whitespace or
/// returns it in the first content delta.
pub fn is_no_question(text: &str) -> bool {
    text.trim_start().starts_with(NO_QUESTION)
}

pub fn strip_no_question(text: &str) -> &str {
    text.trim_start()
        .strip_prefix(NO_QUESTION)
        .unwrap_or(text)
        .trim_start_matches([':', '-', ' '])
        .trim_start()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_is_static_and_short() {
        assert!(SYSTEM_PROMPT.len() < 1_600);
        assert!(SYSTEM_PROMPT.contains(NO_QUESTION));
    }

    #[test]
    fn user_prompt_puts_context_before_question() {
        let prompt = build_user_prompt("teacher: recursion is...", "Why?", 1000);
        let context_pos = prompt.find("teacher").expect("context");
        let question_pos = prompt.find("Latest utterance").expect("question");
        assert!(context_pos < question_pos);
    }

    #[test]
    fn no_question_detection() {
        assert!(is_no_question("NO_QUESTION"));
        assert!(is_no_question("  NO_QUESTION\n"));
        assert!(!is_no_question("No, the answer is 42"));
        assert_eq!(strip_no_question("NO_QUESTION: hello"), "hello");
    }

    #[test]
    fn utf8_safe_truncation() {
        let long = "ї".repeat(100);
        let prompt = build_user_prompt(&long, "q", 10);
        assert!(prompt.contains('q'));
    }
}
