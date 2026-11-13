//! Subsequence fuzzy matching for the command palette.

/// Scores `candidate` against `query` (case-insensitive subsequence);
/// `None` when it does not match. Higher is better: consecutive characters,
/// word starts and an early first match score more.
pub fn score(query: &str, candidate: &str) -> Option<i64> {
    let query: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = candidate.to_lowercase().chars().collect();
    if lower.len() != chars.len() {
        return None;
    }
    let mut score = 0i64;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, c) in lower.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if *c != query[qi] {
            continue;
        }
        let word_start = i == 0 || !chars[i - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        match last {
            Some(prev) if prev + 1 == i => score += 5,
            Some(prev) => score -= (i - prev) as i64 / 2,
            None => score -= i as i64 / 3,
        }
        last = Some(i);
        qi += 1;
    }
    (qi == query.len()).then_some(score)
}

/// Indices of `items` matching `query`, best first (stable for ties).
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(usize, i64)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| score(query, item).map(|s| (i, s)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_and_ranking() {
        assert!(score("bc", "Bury card").is_some());
        assert!(score("xyz", "Bury card").is_none());
        assert_eq!(score("", "anything"), Some(0));
        let items = ["Suspend card", "Bury card", "Set due date...", "Toggle auto advance"];
        assert_eq!(rank("bury", items)[0], 1);
        assert_eq!(rank("sdd", items)[0], 2);
        assert_eq!(rank("auto", items), [3]);
        assert_eq!(rank("card", items), [1, 0]);
    }
}
