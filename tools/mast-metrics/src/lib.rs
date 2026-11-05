//! Classifiers and aggregate calculations for offline Mast measurements.

pub mod metrics;
pub mod report;
pub mod snapshot;

/// Detect a potential prose question; this is a heuristic, not a decision audit.
pub fn is_question(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    text.ends_with('?')
        || ["should i", "do you want", "which", "let me know"]
            .iter()
            .any(|phrase| contains_phrase(&text, phrase))
}

/// Detect common status checks. Call only for human messages to coordinators.
pub fn is_status_question(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "status",
        "what's left",
        "what is left",
        "where are we",
        "any updates",
        "any update",
    ]
    .iter()
    .any(|phrase| contains_phrase(&text, phrase))
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + phrase.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Nearest-rank percentile; returns None for an empty sample or invalid percentile.
pub fn percentile(values: &[i64], percent: u32) -> Option<i64> {
    if values.is_empty() || !(1..=100).contains(&percent) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * percent as usize).div_ceil(100);
    Some(sorted[rank - 1])
}

/// Elapsed milliseconds, rejecting invalid or reversed timestamps.
pub fn elapsed_millis(created_at: i64, resolved_at: i64) -> Option<i64> {
    if created_at <= 0 || resolved_at < created_at {
        return None;
    }
    resolved_at.checked_sub(created_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_questions_without_matching_word_fragments() {
        assert!(is_question("Can this ship?  "));
        assert!(is_question("Should I continue."));
        assert!(is_question("Let me know when ready."));
        assert!(!is_question("The sandwhich is ready."));
        assert!(!is_question("Completed successfully."));
    }

    #[test]
    fn detects_status_checks_without_matching_substrings() {
        assert!(is_status_question("Any updates on this?"));
        assert!(is_status_question("What's left?"));
        assert!(!is_status_question("Update the dependency."));
        assert!(!is_status_question("Check the statuscode."));
    }

    #[test]
    fn calculates_elapsed_time_and_nearest_rank() {
        assert_eq!(elapsed_millis(100, 250), Some(150));
        assert_eq!(elapsed_millis(250, 100), None);
        assert_eq!(elapsed_millis(0, 100), None);
        assert_eq!(percentile(&[9, 1, 4, 2, 8], 50), Some(4));
        assert_eq!(percentile(&[9, 1, 4, 2, 8], 90), Some(9));
        assert_eq!(percentile(&[], 50), None);
    }
}
