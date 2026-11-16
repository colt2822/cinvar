//! Custom vocabulary for transcription engines.
//!
//! Whisper-family decoders take domain words in two channels: an initial prompt (text the
//! decoder conditions on, which biases its spelling of proper nouns and jargon) and, for
//! faster-whisper, a hotword list. Both are built here, and both are None for an empty
//! vocabulary so a run without --vocab is byte-for-byte the run it was before this module
//! existed.
//!
//! The vocabulary is data, never a command line: engines receive it as one argv element.

/// Most terms one vocabulary may carry.
pub const MAX_TERMS: usize = 64;
/// Longest prompt text handed to an engine, in characters.
pub const MAX_CHARS: usize = 480;

/// Trim, drop empties and duplicate spellings (case-insensitively, keeping the first
/// spelling seen), then cap the list. Order is preserved so the prompt is reproducible.
pub fn normalise(vocab: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in vocab {
        let term: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if term.is_empty() {
            continue;
        }
        let seen = out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&term));
        if !seen {
            out.push(term);
        }
        if out.len() >= MAX_TERMS {
            break;
        }
    }
    out
}

/// The initial-prompt text for this vocabulary, or None when there is none to give.
pub fn prompt_for(vocab: &[String]) -> Option<String> {
    let terms = normalise(vocab);
    if terms.is_empty() {
        return None;
    }
    let mut prompt = String::from("Glossary of names and terms used in this recording: ");
    for (position, term) in terms.iter().enumerate() {
        let separator = if position == 0 { "" } else { ", " };
        let projected = prompt.chars().count() + separator.len() + term.chars().count() + 1;
        if projected > MAX_CHARS {
            break;
        }
        prompt.push_str(separator);
        prompt.push_str(term);
    }
    prompt.push('.');
    Some(prompt)
}

/// The hotword list (comma separated) for this vocabulary, or None when it is empty.
pub fn hotwords_for(vocab: &[String]) -> Option<String> {
    let terms = normalise(vocab);
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(", "))
}

/// Split a caller's single string ("ZBrush, Blender;Octane") into terms.
pub fn parse_list(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_empty_vocabulary_stays_empty() {
        assert_eq!(prompt_for(&[]), None);
        assert_eq!(hotwords_for(&[]), None);
        assert_eq!(prompt_for(&v(&["", "   ", "\t"])), None);
        assert_eq!(hotwords_for(&v(&[""])), None);
    }

    #[test]
    fn the_prompt_names_every_term_once() {
        let prompt = prompt_for(&v(&["ZBrush", "zbrush", " Octane Render ", "Pinch"])).unwrap();
        assert_eq!(prompt.matches("ZBrush").count(), 1, "{prompt}");
        assert!(!prompt.contains("zbrush"), "the duplicate spelling is dropped: {prompt}");
        assert!(prompt.contains("Octane Render"), "inner spacing collapses: {prompt}");
        assert!(prompt.starts_with("Glossary"), "{prompt}");
        assert!(prompt.ends_with('.'), "{prompt}");
        assert_eq!(hotwords_for(&v(&["a", "b"])).unwrap(), "a, b");
    }

    #[test]
    fn prompt_length_and_term_count_are_capped() {
        let many: Vec<String> = (0..200).map(|i| format!("term{i}")).collect();
        let prompt = prompt_for(&many).unwrap();
        assert!(prompt.chars().count() <= MAX_CHARS + 2, "{}", prompt.chars().count());
        assert_eq!(normalise(&many).len(), MAX_TERMS);
    }

    #[test]
    fn lists_split_on_both_separators() {
        assert_eq!(parse_list("a, b;c ,, d"), v(&["a", "b", "c", "d"]));
        assert!(parse_list("").is_empty());
    }
}
