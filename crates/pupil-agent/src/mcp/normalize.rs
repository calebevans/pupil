//! Tool name normalization for recovering from LLM-hallucinated tool names.
//!
//! LLMs (especially Gemini Flash) sometimes produce mangled tool names:
//! CamelCase instead of snake_case, doubled words, typos. This module
//! implements a normalization pipeline that attempts to resolve a
//! hallucinated name to a registered tool name.

/// The result of a successful normalization.
#[derive(Debug, Clone)]
pub struct NormalizedTool {
    /// The registered tool name that the input resolved to.
    pub name: String,
    /// Which normalization strategy produced the match.
    pub strategy: &'static str,
}

/// Attempt to resolve `input` to one of the `registered` tool names.
///
/// Returns `Some(NormalizedTool)` if exactly one registered name matches
/// through any normalization strategy. Returns `None` if the input cannot
/// be unambiguously resolved.
pub fn resolve_tool_name(input: &str, registered: &[&str]) -> Option<NormalizedTool> {
    // Strategy 1: Case-insensitive exact match
    if let Some(resolved) = case_insensitive_match(input, registered) {
        return Some(NormalizedTool {
            name: resolved.to_string(),
            strategy: "case_insensitive",
        });
    }

    // Strategy 2: CamelCase/PascalCase -> snake_case conversion
    let snake = to_snake_case(input);
    if snake != input {
        if let Some(pos) = registered.iter().position(|r| *r == snake) {
            return Some(NormalizedTool {
                name: registered[pos].to_string(),
                strategy: "snake_case_conversion",
            });
        }
    }

    // Strategy 3: Strip doubled words (after snake_case conversion)
    let deduped = strip_doubled_words(&snake);
    if deduped != snake {
        if let Some(pos) = registered.iter().position(|r| *r == deduped) {
            return Some(NormalizedTool {
                name: registered[pos].to_string(),
                strategy: "strip_doubled_words",
            });
        }
    }

    // Strategy 4: Prefix match (must be unambiguous)
    if let Some(resolved) = prefix_match(&deduped, registered) {
        return Some(NormalizedTool {
            name: resolved.to_string(),
            strategy: "prefix_match",
        });
    }

    // Strategy 5: Levenshtein distance (max 3 edits, < 40% of target length)
    if let Some(resolved) = levenshtein_match(&deduped, registered) {
        return Some(NormalizedTool {
            name: resolved.to_string(),
            strategy: "levenshtein",
        });
    }

    None
}

/// Find the closest registered tool name to `input` for error message
/// suggestions, regardless of whether it meets the normalization thresholds.
pub fn closest_tool_name(input: &str, registered: &[&str]) -> Option<String> {
    if registered.is_empty() {
        return None;
    }

    let snake = to_snake_case(input);
    let deduped = strip_doubled_words(&snake);

    let mut best: Option<(&str, usize)> = None;
    for &name in registered {
        let dist = levenshtein_distance(&deduped, name);
        if best.is_none() || dist < best.unwrap().1 {
            best = Some((name, dist));
        }
    }

    // Only suggest if the distance is less than the full length of the name
    // (otherwise the suggestion is meaningless).
    best.filter(|(name, dist)| *dist < name.len())
        .map(|(name, _)| name.to_string())
}

/// Case-insensitive exact match. Returns the registered name if exactly
/// one matches.
fn case_insensitive_match<'a>(input: &str, registered: &[&'a str]) -> Option<&'a str> {
    let lower = input.to_lowercase();
    let mut matches: Vec<&str> = registered
        .iter()
        .filter(|r| r.to_lowercase() == lower)
        .copied()
        .collect();

    if matches.len() == 1 {
        Some(matches.remove(0))
    } else {
        None
    }
}

/// Convert a CamelCase or PascalCase string to snake_case.
///
/// Handles:
/// - `StoreMemory` -> `store_memory`
/// - `StoreMemoriesMemories` -> `store_memories_memories`
/// - `recallMemories` -> `recall_memories`
/// - `HTMLParser` -> `html_parser` (consecutive uppercase runs)
/// - `getHTTPResponse` -> `get_http_response`
/// - Already snake_case input is returned unchanged.
/// - Dots (from server-prefixed names like `server.tool`) are preserved.
pub fn to_snake_case(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 8);
    let chars: Vec<char> = input.chars().collect();

    for (i, &ch) in chars.iter().enumerate() {
        if ch == '.' || ch == '_' || ch == '-' {
            result.push(if ch == '-' { '_' } else { ch });
            continue;
        }

        if ch.is_uppercase() {
            // Insert underscore before an uppercase letter when:
            // 1. It's not the first character
            // 2. The previous character was lowercase or a digit, OR
            // 3. The previous character was uppercase and the next character
            //    is lowercase (end of an acronym like "HTTP" in "getHTTPResponse")
            if i > 0 {
                let prev = chars[i - 1];
                let next = chars.get(i + 1);

                let prev_is_separator = prev == '.' || prev == '_' || prev == '-';
                let prev_was_lower_or_digit = prev.is_lowercase() || prev.is_ascii_digit();
                let is_acronym_end =
                    prev.is_uppercase() && next.map_or(false, |n| n.is_lowercase());

                if !prev_is_separator && (prev_was_lower_or_digit || is_acronym_end) {
                    result.push('_');
                }
            }
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }

    result
}

/// Remove consecutive duplicate words from a snake_case string.
///
/// `store_memories_memories` -> `store_memories`
/// `store_memories_memories_memories` -> `store_memories`
/// `recall_memories` -> `recall_memories` (no change, words are different)
pub fn strip_doubled_words(input: &str) -> String {
    // Handle server-prefixed names: split on dot, process the tool part
    if let Some((prefix, tool_part)) = input.split_once('.') {
        let cleaned = strip_doubled_words_inner(tool_part);
        return format!("{prefix}.{cleaned}");
    }

    strip_doubled_words_inner(input)
}

fn strip_doubled_words_inner(input: &str) -> String {
    let words: Vec<&str> = input.split('_').collect();
    if words.len() <= 1 {
        return input.to_string();
    }

    let mut deduped: Vec<&str> = Vec::with_capacity(words.len());
    for word in &words {
        if deduped.last() != Some(word) {
            deduped.push(word);
        }
    }

    deduped.join("_")
}

/// Find a registered tool name that shares a prefix with `input`.
/// Returns `Some` only if exactly one tool matches (unambiguous).
fn prefix_match<'a>(input: &str, registered: &[&'a str]) -> Option<&'a str> {
    let mut matches: Vec<&str> = Vec::new();

    for &name in registered {
        // Check if input is a prefix of a registered name
        if name.starts_with(input) && input.len() >= 4 {
            matches.push(name);
        }
        // Check if a registered name is a prefix of input
        if input.starts_with(name) && name.len() >= 4 {
            matches.push(name);
        }
    }

    matches.dedup();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

/// Find the best Levenshtein match within thresholds.
/// Returns `Some` only if exactly one tool is within the threshold.
fn levenshtein_match<'a>(input: &str, registered: &[&'a str]) -> Option<&'a str> {
    const MAX_DISTANCE: usize = 3;

    let mut best_name: Option<&str> = None;
    let mut best_dist: usize = usize::MAX;
    let mut is_ambiguous = false;

    for &name in registered {
        // Distance must be < 40% of the registered name's length to prevent
        // short names from matching anything.
        let max_allowed = (name.len() * 2) / 5; // 40%
        let threshold = MAX_DISTANCE.min(max_allowed);
        if threshold == 0 {
            continue;
        }

        let dist = levenshtein_distance(input, name);
        if dist > threshold {
            continue;
        }

        if dist < best_dist {
            best_dist = dist;
            best_name = Some(name);
            is_ambiguous = false;
        } else if dist == best_dist {
            is_ambiguous = true;
        }
    }

    if is_ambiguous {
        None
    } else {
        best_name
    }
}

/// Compute the Levenshtein edit distance between two strings.
///
/// Uses the standard two-row dynamic programming algorithm with O(min(m,n))
/// space. No external dependency needed for this.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    let m = a_chars.len();
    let n = b_chars.len();

    // Optimize by making sure we iterate over the shorter string in the
    // inner loop.
    if m < n {
        return levenshtein_distance(b, a);
    }

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr: Vec<usize> = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // to_snake_case
    // ---------------------------------------------------------------

    #[test]
    fn test_snake_case_pascal() {
        assert_eq!(to_snake_case("StoreMemory"), "store_memory");
    }

    #[test]
    fn test_snake_case_camel() {
        assert_eq!(to_snake_case("recallMemories"), "recall_memories");
    }

    #[test]
    fn test_snake_case_already_snake() {
        assert_eq!(to_snake_case("store_memory"), "store_memory");
    }

    #[test]
    fn test_snake_case_consecutive_uppercase() {
        assert_eq!(to_snake_case("HTMLParser"), "html_parser");
    }

    #[test]
    fn test_snake_case_acronym_in_middle() {
        assert_eq!(to_snake_case("getHTTPResponse"), "get_http_response");
    }

    #[test]
    fn test_snake_case_dotted_prefix() {
        assert_eq!(
            to_snake_case("server.StoreMemory"),
            "server.store_memory"
        );
    }

    #[test]
    fn test_snake_case_hallucinated_double() {
        assert_eq!(
            to_snake_case("StoreMemoriesMemories"),
            "store_memories_memories"
        );
    }

    #[test]
    fn test_snake_case_single_word() {
        assert_eq!(to_snake_case("Store"), "store");
    }

    #[test]
    fn test_snake_case_all_lowercase() {
        assert_eq!(to_snake_case("storememory"), "storememory");
    }

    #[test]
    fn test_snake_case_with_numbers() {
        assert_eq!(to_snake_case("getV2Status"), "get_v2_status");
    }

    #[test]
    fn test_snake_case_hyphenated() {
        assert_eq!(to_snake_case("store-memory"), "store_memory");
    }

    // ---------------------------------------------------------------
    // strip_doubled_words
    // ---------------------------------------------------------------

    #[test]
    fn test_strip_doubled_consecutive() {
        assert_eq!(
            strip_doubled_words("store_memories_memories"),
            "store_memories"
        );
    }

    #[test]
    fn test_strip_doubled_triple() {
        assert_eq!(
            strip_doubled_words("store_memories_memories_memories"),
            "store_memories"
        );
    }

    #[test]
    fn test_strip_doubled_no_change() {
        assert_eq!(strip_doubled_words("recall_memories"), "recall_memories");
    }

    #[test]
    fn test_strip_doubled_single_word() {
        assert_eq!(strip_doubled_words("store"), "store");
    }

    #[test]
    fn test_strip_doubled_with_server_prefix() {
        assert_eq!(
            strip_doubled_words("recalld.store_memories_memories"),
            "recalld.store_memories"
        );
    }

    #[test]
    fn test_strip_doubled_multiple_groups() {
        assert_eq!(
            strip_doubled_words("find_find_similar_similar"),
            "find_similar"
        );
    }

    // ---------------------------------------------------------------
    // levenshtein_distance
    // ---------------------------------------------------------------

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein_distance("store_memory", "store_memory"), 0);
    }

    #[test]
    fn test_levenshtein_one_edit() {
        assert_eq!(levenshtein_distance("store_memry", "store_memory"), 1);
    }

    #[test]
    fn test_levenshtein_two_edits() {
        // "store_memmry" vs "store_memory" is actually 1 edit (m->o).
        assert_eq!(levenshtein_distance("store_memmry", "store_memory"), 1);
        // "store_mammry" vs "store_memory" is 2 edits (e->a, o->m).
        assert_eq!(levenshtein_distance("store_mammry", "store_memory"), 2);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn test_levenshtein_completely_different() {
        assert_eq!(levenshtein_distance("abc", "xyz"), 3);
    }

    // ---------------------------------------------------------------
    // resolve_tool_name (integration)
    // ---------------------------------------------------------------

    fn tool_names() -> Vec<&'static str> {
        vec![
            "store_memory",
            "recall_memories",
            "find_similar_memories",
            "reinforce_memory",
            "forget_memory",
            "get_memory",
            "create_namespace",
        ]
    }

    #[test]
    fn test_resolve_exact_match_not_triggered() {
        // Exact matches should be handled by tool_index before reaching
        // resolve_tool_name. But if called, case_insensitive catches it.
        let names = tool_names();
        let result = resolve_tool_name("store_memory", &names);
        // case_insensitive_match matches it because lowercased == lowercased,
        // but since the exact name is in the list, the caller should not
        // invoke this function. Regardless, it resolves correctly.
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "store_memory");
    }

    #[test]
    fn test_resolve_case_insensitive() {
        let names = tool_names();
        let result = resolve_tool_name("Store_Memory", &names).unwrap();
        assert_eq!(result.name, "store_memory");
        assert_eq!(result.strategy, "case_insensitive");
    }

    #[test]
    fn test_resolve_pascal_case() {
        let names = tool_names();
        let result = resolve_tool_name("StoreMemory", &names).unwrap();
        assert_eq!(result.name, "store_memory");
        assert_eq!(result.strategy, "snake_case_conversion");
    }

    #[test]
    fn test_resolve_camel_case() {
        let names = tool_names();
        let result = resolve_tool_name("recallMemories", &names).unwrap();
        assert_eq!(result.name, "recall_memories");
        assert_eq!(result.strategy, "snake_case_conversion");
    }

    #[test]
    fn test_resolve_doubled_words() {
        let names = tool_names();
        let result = resolve_tool_name("StoreMemoriesMemories", &names);
        // StoreMemoriesMemories -> store_memories_memories -> strip doubled
        // -> store_memories. But "store_memories" is not in the list
        // (it's "store_memory"). So this falls through to levenshtein.
        // Let's use the actual hallucination pattern from the bug report:
        // "store_memories" vs registered "store_memory" has distance 1.
        // That's within threshold.
        assert!(result.is_some());
    }

    #[test]
    fn test_resolve_doubled_words_exact() {
        // Use a tool list that includes the base form.
        let names = vec!["store_memories", "recall_memories"];
        let result =
            resolve_tool_name("StoreMemoriesMemories", &names).unwrap();
        assert_eq!(result.name, "store_memories");
        assert_eq!(result.strategy, "strip_doubled_words");
    }

    #[test]
    fn test_resolve_recall_memories_pascal() {
        let names = tool_names();
        let result = resolve_tool_name("RecallMemories", &names).unwrap();
        assert_eq!(result.name, "recall_memories");
        assert_eq!(result.strategy, "snake_case_conversion");
    }

    #[test]
    fn test_resolve_find_similar_memories_camel() {
        let names = tool_names();
        let result =
            resolve_tool_name("findSimilarMemories", &names).unwrap();
        assert_eq!(result.name, "find_similar_memories");
        assert_eq!(result.strategy, "snake_case_conversion");
    }

    #[test]
    fn test_resolve_typo_levenshtein() {
        let names = tool_names();
        let result = resolve_tool_name("store_memmory", &names).unwrap();
        assert_eq!(result.name, "store_memory");
        assert_eq!(result.strategy, "levenshtein");
    }

    #[test]
    fn test_resolve_no_match() {
        let names = tool_names();
        let result = resolve_tool_name("completely_unrelated_tool", &names);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_empty_registered() {
        let result = resolve_tool_name("store_memory", &[]);
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------
    // closest_tool_name (suggestion for error messages)
    // ---------------------------------------------------------------

    #[test]
    fn test_closest_tool_name_typo() {
        let names = tool_names();
        let suggestion = closest_tool_name("store_memmory", &names);
        assert_eq!(suggestion.as_deref(), Some("store_memory"));
    }

    #[test]
    fn test_closest_tool_name_no_tools() {
        let suggestion = closest_tool_name("anything", &[]);
        assert!(suggestion.is_none());
    }

    #[test]
    fn test_closest_tool_name_completely_unrelated() {
        // When the input is so different that the distance exceeds the
        // name length, no suggestion is returned.
        let names = vec!["a", "b"];
        let suggestion = closest_tool_name("xxxxxxxxxxxxxxxxxx", &names);
        assert!(suggestion.is_none());
    }

    // ---------------------------------------------------------------
    // prefix_match
    // ---------------------------------------------------------------

    #[test]
    fn test_prefix_match_input_is_prefix() {
        let names = vec!["store_memory", "recall_memories"];
        // "store_memo" (len 10 >= 4) is a prefix of "store_memory"
        let result = prefix_match("store_memo", &names);
        assert_eq!(result, Some("store_memory"));
    }

    #[test]
    fn test_prefix_match_ambiguous() {
        let names = vec!["store_memory", "store_memories"];
        // "store_mem" is a prefix of both -- ambiguous
        let result = prefix_match("store_mem", &names);
        assert!(result.is_none());
    }

    #[test]
    fn test_prefix_match_too_short() {
        let names = vec!["store_memory"];
        let result = prefix_match("sto", &names);
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------
    // case_insensitive_match
    // ---------------------------------------------------------------

    #[test]
    fn test_case_insensitive_upper() {
        let names = vec!["store_memory", "recall_memories"];
        let result = case_insensitive_match("STORE_MEMORY", &names);
        assert_eq!(result, Some("store_memory"));
    }

    #[test]
    fn test_case_insensitive_mixed() {
        let names = vec!["store_memory"];
        let result = case_insensitive_match("Store_Memory", &names);
        assert_eq!(result, Some("store_memory"));
    }

    #[test]
    fn test_case_insensitive_no_match() {
        let names = vec!["store_memory"];
        let result = case_insensitive_match("store_memories", &names);
        assert!(result.is_none());
    }
}
