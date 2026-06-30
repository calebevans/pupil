#[cfg(feature = "learn")]
use crate::llm::{ChatConfig, LlmProvider, Message};
#[cfg(feature = "learn")]
use crate::mcp::McpManager;
#[cfg(feature = "learn")]
use super::learner::{extract_memory_id_from_result, LearningResult};
#[cfg(feature = "learn")]
use super::reader::ReadingSection;

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct VerificationResult {
    pub total: usize,
    pub retrievable: usize,
    pub gap_memory_ids: Vec<String>,
    pub facts: Vec<VerifiedFact>,
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct VerifiedFact {
    pub fact: String,
    pub retrieved: bool,
    pub gap_memory_id: Option<String>,
}

#[cfg(feature = "learn")]
const SIMILARITY_THRESHOLD: f64 = 0.5;

#[cfg(feature = "learn")]
pub async fn verify_memories(
    llm: &dyn LlmProvider,
    mcp: &McpManager,
    sections: &[ReadingSection],
    _learning_result: &LearningResult,
    source_key: &str,
    namespace: &str,
) -> anyhow::Result<VerificationResult> {
    let content_summary = build_content_summary(sections);
    let fact_count = fact_count_for_sections(sections.len());

    let facts = match extract_facts(llm, &content_summary, source_key, fact_count).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                source = %source_key,
                error = %e,
                "Fact extraction failed; skipping verification"
            );
            return Ok(VerificationResult {
                total: 0,
                retrievable: 0,
                gap_memory_ids: vec![],
                facts: vec![],
            });
        }
    };

    let mut verified_facts = Vec::with_capacity(facts.len());
    let mut gap_memory_ids = Vec::new();
    let mut retrievable = 0;

    for (i, fact) in facts.iter().enumerate() {
        let retrieved = recall_fact(mcp, fact, namespace).await;

        if retrieved {
            retrievable += 1;
            tracing::debug!(
                fact_index = i + 1,
                total = facts.len(),
                fact = %fact,
                "Fact [RETRIEVED]"
            );
            verified_facts.push(VerifiedFact {
                fact: fact.clone(),
                retrieved: true,
                gap_memory_id: None,
            });
        } else {
            let gap_id = store_gap_memory(mcp, fact, source_key, namespace).await;
            tracing::debug!(
                fact_index = i + 1,
                total = facts.len(),
                fact = %fact,
                gap_memory_id = ?gap_id,
                "Fact [GAP FILLED]"
            );
            if let Some(ref id) = gap_id {
                gap_memory_ids.push(id.clone());
            }
            verified_facts.push(VerifiedFact {
                fact: fact.clone(),
                retrieved: false,
                gap_memory_id: gap_id,
            });
        }
    }

    Ok(VerificationResult {
        total: facts.len(),
        retrievable,
        gap_memory_ids,
        facts: verified_facts,
    })
}

#[cfg(feature = "learn")]
fn build_content_summary(sections: &[ReadingSection]) -> String {
    if sections.len() <= 5 {
        sections
            .iter()
            .map(|s| {
                if s.heading_path.is_empty() {
                    s.text.clone()
                } else {
                    format!("## {}\n\n{}", s.heading_path, s.text)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    } else {
        sections
            .iter()
            .map(|s| {
                let preview: String = s.text.chars().take(200).collect();
                if s.heading_path.is_empty() {
                    preview
                } else {
                    format!("## {}\n\n{}", s.heading_path, preview)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }
}

#[cfg(feature = "learn")]
fn fact_count_for_sections(section_count: usize) -> usize {
    match section_count {
        0..=2 => 3,
        3..=5 => 4,
        _ => 5,
    }
}

#[cfg(feature = "learn")]
async fn extract_facts(
    llm: &dyn LlmProvider,
    content_summary: &str,
    source_key: &str,
    fact_count: usize,
) -> anyhow::Result<Vec<String>> {
    let system = format!(
        "You are a verification assistant. Given material that was just studied, extract \
         the {} most important facts that a learner MUST remember. Each fact should be a \
         specific, concrete statement that could be verified by searching a memory \
         system. Prefer facts that are:\n\
         \n\
         - Actionable (procedures, configurations, thresholds)\n\
         - Specific (names, numbers, exact behaviors)\n\
         - Non-obvious (not something derivable from general knowledge)\n\
         \n\
         Respond with a JSON object:\n\
         {{\n\
           \"facts\": [\"fact 1\", \"fact 2\", ...]\n\
         }}\n\
         \n\
         Each fact should be a single sentence, written as a natural language search \
         query (the way someone would ask about it).",
        fact_count,
    );

    let user_msg = format!(
        "Here is the material from \"{}\". Extract {} key facts.\n\n---\n\n{}",
        source_key, fact_count, content_summary,
    );

    let messages = vec![Message::system(&system), Message::user(&user_msg)];

    let config = ChatConfig {
        temperature: Some(0.0),
        max_tokens: Some(1024),
        ..Default::default()
    };

    let response = llm
        .chat(&messages, &[], &config)
        .await
        .map_err(|e| anyhow::anyhow!("LLM error during fact extraction: {}", e))?;

    parse_facts_response(&response.content)
}

#[cfg(feature = "learn")]
fn parse_facts_response(content: &str) -> anyhow::Result<Vec<String>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("Empty response from LLM"));
    }

    let cleaned = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let json_str = if let Some(start) = cleaned.find('{') {
        if let Some(end) = cleaned.rfind('}') {
            &cleaned[start..=end]
        } else {
            cleaned
        }
    } else {
        cleaned
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse facts JSON: {}", e))?;

    let facts = parsed
        .get("facts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Facts response missing 'facts' array"))?;

    let result: Vec<String> = facts
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if result.is_empty() {
        anyhow::bail!("No facts extracted from response");
    }

    Ok(result)
}

#[cfg(feature = "learn")]
async fn recall_fact(mcp: &McpManager, fact: &str, namespace: &str) -> bool {
    let args = serde_json::json!({
        "query": fact,
        "namespace": namespace,
        "limit": 3,
        "minStrength": 0.0,
    });
    let arguments = args.as_object().cloned();

    match mcp.call_tool("recall_memories", arguments).await {
        Ok(result) => {
            let result_text = crate::mcp::schema::tool_result_to_string(&result);
            has_relevant_result(&result_text)
        }
        Err(e) => {
            tracing::warn!(
                fact = %fact,
                error = %e,
                "recall_memories failed during verification"
            );
            false
        }
    }
}

#[cfg(feature = "learn")]
fn has_relevant_result(result_text: &str) -> bool {
    let parsed: serde_json::Value = match serde_json::from_str(result_text) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let results = match parsed.get("results").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            if let Some(arr) = parsed.as_array() {
                arr
            } else {
                return false;
            }
        }
    };

    if results.is_empty() {
        return false;
    }

    if let Some(top) = results.first() {
        if let Some(score) = top.get("score").and_then(|v| v.as_f64()) {
            return score >= SIMILARITY_THRESHOLD;
        }
        if let Some(score) = top.get("similarity").and_then(|v| v.as_f64()) {
            return score >= SIMILARITY_THRESHOLD;
        }
    }

    true
}

#[cfg(feature = "learn")]
async fn store_gap_memory(
    mcp: &McpManager,
    fact: &str,
    source_key: &str,
    namespace: &str,
) -> Option<String> {
    let args = serde_json::json!({
        "summary": fact,
        "namespace": namespace,
        "tags": [format!("source/{}", source_key), "type/verification-gap"],
    });
    let arguments = args.as_object().cloned();

    match mcp.call_tool("store_memory", arguments).await {
        Ok(result) => {
            let result_text = crate::mcp::schema::tool_result_to_string(&result);
            extract_memory_id_from_result(&result_text)
        }
        Err(e) => {
            tracing::warn!(
                fact = %fact,
                source = %source_key,
                error = %e,
                "store_memory failed during gap-filling"
            );
            None
        }
    }
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_build_content_summary_few_sections() {
        let sections = vec![
            ReadingSection {
                document_title: "Doc".into(),
                heading_path: "Intro".into(),
                text: "Some intro text.".into(),
                section_number: 1,
                total_sections: 2,
                is_summary_checkpoint: false,
            },
            ReadingSection {
                document_title: "Doc".into(),
                heading_path: "".into(),
                text: "Body content.".into(),
                section_number: 2,
                total_sections: 2,
                is_summary_checkpoint: false,
            },
        ];

        let summary = build_content_summary(&sections);
        assert!(summary.contains("## Intro"));
        assert!(summary.contains("Some intro text."));
        assert!(summary.contains("Body content."));
        assert!(summary.contains("---"));
    }

    #[test]
    fn test_build_content_summary_many_sections_truncates() {
        let long_text = "x".repeat(500);
        let sections: Vec<ReadingSection> = (1..=7)
            .map(|i| ReadingSection {
                document_title: "Doc".into(),
                heading_path: format!("Section {}", i),
                text: long_text.clone(),
                section_number: i,
                total_sections: 7,
                is_summary_checkpoint: false,
            })
            .collect();

        let summary = build_content_summary(&sections);
        for segment in summary.split("---") {
            let trimmed = segment.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Each segment: "## Section N\n\n" + 200 chars = well under 500
            assert!(trimmed.len() <= 220, "segment too long: {}", trimmed.len());
        }
    }

    #[test]
    fn test_build_content_summary_many_sections_handles_multibyte() {
        let multibyte_text = "\u{1F600}".repeat(300); // 300 emoji, 4 bytes each
        let sections: Vec<ReadingSection> = (1..=6)
            .map(|i| ReadingSection {
                document_title: "Doc".into(),
                heading_path: "".into(),
                text: multibyte_text.clone(),
                section_number: i,
                total_sections: 6,
                is_summary_checkpoint: false,
            })
            .collect();

        // Should not panic (byte-slicing would panic on multibyte)
        let summary = build_content_summary(&sections);
        assert!(!summary.is_empty());
    }

    #[test]
    fn test_build_content_summary_empty_sections() {
        let sections: Vec<ReadingSection> = vec![];
        let summary = build_content_summary(&sections);
        assert!(summary.is_empty());
    }

    #[test]
    fn test_fact_count_for_sections() {
        assert_eq!(fact_count_for_sections(0), 3);
        assert_eq!(fact_count_for_sections(1), 3);
        assert_eq!(fact_count_for_sections(2), 3);
        assert_eq!(fact_count_for_sections(3), 4);
        assert_eq!(fact_count_for_sections(5), 4);
        assert_eq!(fact_count_for_sections(6), 5);
        assert_eq!(fact_count_for_sections(20), 5);
    }

    #[test]
    fn test_parse_facts_response_valid() {
        let json = r#"{"facts": ["fact one", "fact two", "fact three"]}"#;
        let facts = parse_facts_response(json).unwrap();
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0], "fact one");
    }

    #[test]
    fn test_parse_facts_response_empty_array() {
        let json = r#"{"facts": []}"#;
        let result = parse_facts_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_facts_response_missing_key() {
        let json = r#"{"items": ["a", "b"]}"#;
        let result = parse_facts_response(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_facts_response_invalid_json() {
        let result = parse_facts_response("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn test_has_relevant_result_with_high_score() {
        let json = r#"{"results": [{"id": "abc", "summary": "test", "score": 0.8}]}"#;
        assert!(has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_with_low_score() {
        let json = r#"{"results": [{"id": "abc", "summary": "test", "score": 0.2}]}"#;
        assert!(!has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_empty() {
        let json = r#"{"results": []}"#;
        assert!(!has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_no_score_field() {
        let json = r#"{"results": [{"id": "abc", "summary": "test"}]}"#;
        assert!(has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_similarity_field() {
        let json = r#"{"results": [{"id": "abc", "similarity": 0.7}]}"#;
        assert!(has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_similarity_below_threshold() {
        let json = r#"{"results": [{"id": "abc", "similarity": 0.3}]}"#;
        assert!(!has_relevant_result(json));
    }

    #[test]
    fn test_has_relevant_result_invalid_json() {
        assert!(!has_relevant_result("not json"));
    }

    #[test]
    fn test_has_relevant_result_array_format() {
        let json = r#"[{"id": "abc", "score": 0.9}]"#;
        assert!(has_relevant_result(json));
    }
}
