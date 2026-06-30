use clap::Args;
use quick_junit::{NonSuccessKind, Report, TestCase as JunitTestCase, TestSuite as JunitSuite};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

use crate::agent_config::{
    image_ref, resolve_agent, resolve_api_key, AgentConfig, SourceEntry,
};
use crate::container::{self, ContainerRuntime, RunOptions};
use crate::error::CliError;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Args)]
pub struct TestArgs {
    pub name: Option<String>,

    #[arg(long, default_value = "tests.yaml")]
    pub file: Option<PathBuf>,

    #[arg(long)]
    pub show_details: bool,

    #[arg(long)]
    pub json: bool,

    #[arg(long, value_name = "PATH")]
    pub junit: Option<PathBuf>,

    #[arg(long)]
    pub filter: Option<String>,

    #[arg(long)]
    pub generate: bool,

    #[arg(long, default_value = "10")]
    pub count: usize,

    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub retries: Option<u32>,

    #[arg(long)]
    pub timeout: Option<u64>,

    #[arg(long)]
    pub threshold: Option<f64>,
}

// ---------------------------------------------------------------------------
// Test YAML schema types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestFile {
    #[serde(default)]
    pub config: TestConfig,
    pub tests: Vec<TestCaseDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestConfig {
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub retries: u32,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_threshold() -> f64 {
    0.8
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            retries: 0,
            timeout_secs: default_timeout_secs(),
            judge_model: None,
            threshold: default_threshold(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestCaseDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub threshold: Option<f64>,
    pub expects: Vec<Assertion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assertion {
    Contains(String),
    NotContains(String),
    ContainsAny(Vec<String>),
    ContainsAll(Vec<String>),
    Matches(String),
    NotMatches(String),
    StartsWith(String),
    MemoryHit(bool),
    MemorySource(String),
    MemoryQuery(String),
    ToolCalled(String),
    ToolNotCalled(String),
    LlmJudge(LlmJudgeConfig),
    SemanticSimilarity(SemanticSimilarityConfig),
    Faithfulness(FaithfulnessConfig),
    LatencyMs(LatencyConfig),
    TokenCount(TokenCountConfig),
}

impl<'de> Deserialize<'de> for Assertion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AssertionVisitor;

        impl<'de> Visitor<'de> for AssertionVisitor {
            type Value = Assertion;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a single-key map representing an assertion")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Assertion, M::Error>
            where
                M: MapAccess<'de>,
            {
                let key: String =
                    map.next_key()?.ok_or_else(|| de::Error::custom("empty assertion map"))?;

                let assertion = match key.as_str() {
                    "contains" => Assertion::Contains(map.next_value()?),
                    "not_contains" => Assertion::NotContains(map.next_value()?),
                    "contains_any" => Assertion::ContainsAny(map.next_value()?),
                    "contains_all" => Assertion::ContainsAll(map.next_value()?),
                    "matches" => Assertion::Matches(map.next_value()?),
                    "not_matches" => Assertion::NotMatches(map.next_value()?),
                    "starts_with" => Assertion::StartsWith(map.next_value()?),
                    "memory_hit" => Assertion::MemoryHit(map.next_value()?),
                    "memory_source" => Assertion::MemorySource(map.next_value()?),
                    "memory_query" => Assertion::MemoryQuery(map.next_value()?),
                    "tool_called" => Assertion::ToolCalled(map.next_value()?),
                    "tool_not_called" => Assertion::ToolNotCalled(map.next_value()?),
                    "llm_judge" => Assertion::LlmJudge(map.next_value()?),
                    "semantic_similarity" => Assertion::SemanticSimilarity(map.next_value()?),
                    "faithfulness" => Assertion::Faithfulness(map.next_value()?),
                    "latency_ms" => Assertion::LatencyMs(map.next_value()?),
                    "token_count" => Assertion::TokenCount(map.next_value()?),
                    other => {
                        return Err(de::Error::unknown_variant(
                            other,
                            &[
                                "contains",
                                "not_contains",
                                "contains_any",
                                "contains_all",
                                "matches",
                                "not_matches",
                                "starts_with",
                                "memory_hit",
                                "memory_source",
                                "memory_query",
                                "tool_called",
                                "tool_not_called",
                                "llm_judge",
                                "semantic_similarity",
                                "faithfulness",
                                "latency_ms",
                                "token_count",
                            ],
                        ));
                    }
                };

                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom("assertion map must have exactly one key"));
                }

                Ok(assertion)
            }
        }

        deserializer.deserialize_map(AssertionVisitor)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmJudgeConfig {
    pub criteria: String,
    #[serde(default)]
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SemanticSimilarityConfig {
    pub reference: String,
    #[serde(default)]
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FaithfulnessConfig {
    #[serde(default)]
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LatencyConfig {
    pub max: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenCountConfig {
    pub max: u64,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl TestFile {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.tests.is_empty() {
            errors.push("tests list must not be empty".to_string());
        }

        let mut seen_names = HashSet::new();
        for test in &self.tests {
            if !seen_names.insert(&test.name) {
                errors.push(format!("duplicate test name: '{}'", test.name));
            }
        }

        if !(0.0..=1.0).contains(&self.config.threshold) {
            errors.push(format!(
                "config.threshold must be 0.0-1.0, got {}",
                self.config.threshold
            ));
        }

        if self.config.timeout_secs == 0 {
            errors.push("config.timeout_secs must be > 0".to_string());
        }

        let name_re = regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]*$").unwrap();

        for test in &self.tests {
            let prefix = format!("test '{}': ", test.name);

            if !name_re.is_match(&test.name) {
                errors.push(format!(
                    "{prefix}name must start with alphanumeric and contain only \
                     alphanumeric, hyphens, underscores"
                ));
            }

            if test.question.trim().is_empty() {
                errors.push(format!("{prefix}question must not be empty"));
            }

            if test.expects.is_empty() {
                errors.push(format!("{prefix}expects must have at least one assertion"));
            }

            if let Some(t) = test.threshold {
                if !(0.0..=1.0).contains(&t) {
                    errors.push(format!("{prefix}threshold must be 0.0-1.0, got {t}"));
                }
            }

            for (i, assertion) in test.expects.iter().enumerate() {
                let a_prefix = format!("{prefix}expects[{i}]: ");
                match assertion {
                    Assertion::Matches(pattern) | Assertion::NotMatches(pattern) => {
                        if let Err(e) = regex::Regex::new(pattern) {
                            errors.push(format!("{a_prefix}invalid regex: {e}"));
                        }
                    }
                    Assertion::ContainsAny(list) if list.is_empty() => {
                        errors.push(format!("{a_prefix}contains_any list must not be empty"));
                    }
                    Assertion::ContainsAll(list) if list.is_empty() => {
                        errors.push(format!("{a_prefix}contains_all list must not be empty"));
                    }
                    Assertion::LatencyMs(cfg) if cfg.max == 0 => {
                        errors.push(format!("{a_prefix}latency_ms.max must be > 0"));
                    }
                    Assertion::TokenCount(cfg) if cfg.max == 0 => {
                        errors.push(format!("{a_prefix}token_count.max must be > 0"));
                    }
                    Assertion::LlmJudge(cfg) if cfg.criteria.trim().is_empty() => {
                        errors.push(format!("{a_prefix}llm_judge.criteria must not be empty"));
                    }
                    Assertion::SemanticSimilarity(cfg) if cfg.reference.trim().is_empty() => {
                        errors.push(format!(
                            "{a_prefix}semantic_similarity.reference must not be empty"
                        ));
                    }
                    Assertion::LlmJudge(cfg) => {
                        if let Some(t) = cfg.threshold {
                            if !(0.0..=1.0).contains(&t) {
                                errors.push(format!(
                                    "{a_prefix}llm_judge.threshold must be 0.0-1.0, got {t}"
                                ));
                            }
                        }
                    }
                    Assertion::SemanticSimilarity(cfg) => {
                        if let Some(t) = cfg.threshold {
                            if !(0.0..=1.0).contains(&t) {
                                errors.push(format!(
                                    "{a_prefix}semantic_similarity.threshold must be 0.0-1.0, got {t}"
                                ));
                            }
                        }
                    }
                    Assertion::Faithfulness(cfg) => {
                        if let Some(t) = cfg.threshold {
                            if !(0.0..=1.0).contains(&t) {
                                errors.push(format!(
                                    "{a_prefix}faithfulness.threshold must be 0.0-1.0, got {t}"
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// ---------------------------------------------------------------------------
// Result types (deserialized from container output)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    pub question: String,
    pub response: String,
    pub assertions: Vec<AssertionResult>,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: Vec<CapturedToolCall>,
    pub retries_used: usize,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    pub assertion_type: String,
    pub passed: bool,
    pub score: Option<f64>,
    pub threshold: Option<f64>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedToolCall {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result: serde_json::Value,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseCapture {
    pub response_text: String,
    pub tool_calls: Vec<CapturedToolCall>,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub recalled_memories: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Summary and output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct TestSummary {
    agent: String,
    total: usize,
    passed: usize,
    failed: usize,
    pass_rate: f64,
    total_latency_ms: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    estimated_cost_usd: f64,
}

#[derive(Debug, Serialize)]
struct JsonOutput {
    agent: String,
    timestamp: String,
    summary: TestSummary,
    tests: Vec<TestResult>,
}

// ---------------------------------------------------------------------------
// Assertion evaluation functions (all 17 types)
// ---------------------------------------------------------------------------

fn eval_contains(response: &str, expected: &str) -> AssertionResult {
    let response_lower = response.to_lowercase();
    let expected_lower = expected.to_lowercase();
    let passed = response_lower.contains(&expected_lower);
    AssertionResult {
        assertion_type: "contains".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("Found '{}' in response", expected)
        } else {
            format!("'{}' not found in response", expected)
        },
    }
}

fn eval_not_contains(response: &str, excluded: &str) -> AssertionResult {
    let response_lower = response.to_lowercase();
    let excluded_lower = excluded.to_lowercase();
    let passed = !response_lower.contains(&excluded_lower);
    AssertionResult {
        assertion_type: "not_contains".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("'{}' correctly absent from response", excluded)
        } else {
            format!("'{}' found in response but should not be", excluded)
        },
    }
}

fn eval_contains_any(response: &str, candidates: &[String]) -> AssertionResult {
    let response_lower = response.to_lowercase();
    let matched: Vec<&String> = candidates
        .iter()
        .filter(|c| response_lower.contains(&c.to_lowercase()))
        .collect();
    let passed = !matched.is_empty();
    AssertionResult {
        assertion_type: "contains_any".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!(
                "Found {} of {} candidates: {}",
                matched.len(),
                candidates.len(),
                matched
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "None of [{}] found in response",
                candidates
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    }
}

fn eval_contains_all(response: &str, required: &[String]) -> AssertionResult {
    let response_lower = response.to_lowercase();
    let missing: Vec<&String> = required
        .iter()
        .filter(|r| !response_lower.contains(&r.to_lowercase()))
        .collect();
    let passed = missing.is_empty();
    AssertionResult {
        assertion_type: "contains_all".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("All {} required strings found", required.len())
        } else {
            format!(
                "Missing {} of {}: {}",
                missing.len(),
                required.len(),
                missing
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    }
}

fn eval_matches(response: &str, pattern: &str) -> AssertionResult {
    let re = regex::Regex::new(pattern).expect("regex validated at parse time");
    let passed = re.is_match(response);
    AssertionResult {
        assertion_type: "matches".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("Response matches pattern /{}/", pattern)
        } else {
            format!("Response does not match pattern /{}/", pattern)
        },
    }
}

fn eval_not_matches(response: &str, pattern: &str) -> AssertionResult {
    let re = regex::Regex::new(pattern).expect("regex validated at parse time");
    let passed = !re.is_match(response);
    AssertionResult {
        assertion_type: "not_matches".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("Response correctly does not match pattern /{}/", pattern)
        } else {
            format!("Response matches pattern /{}/ but should not", pattern)
        },
    }
}

fn eval_starts_with(response: &str, prefix: &str) -> AssertionResult {
    let passed = response
        .to_lowercase()
        .trim_start()
        .starts_with(&prefix.to_lowercase());
    AssertionResult {
        assertion_type: "starts_with".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: if passed {
            format!("Response starts with '{}'", prefix)
        } else {
            let actual_start: String = response.chars().take(prefix.len() + 20).collect();
            format!(
                "Response starts with '{}...', expected '{}'",
                actual_start, prefix
            )
        },
    }
}

fn eval_memory_hit(capture: &ResponseCapture, expected: bool) -> AssertionResult {
    let recall_calls: Vec<&CapturedToolCall> = capture
        .tool_calls
        .iter()
        .filter(|tc| tc.tool_name == "recall_memories")
        .collect();

    let has_results = !capture.recalled_memories.is_empty();

    let passed = if expected {
        !recall_calls.is_empty() && has_results
    } else {
        recall_calls.is_empty() || !has_results
    };

    let detail = if expected {
        if recall_calls.is_empty() {
            "Agent did not call recall_memories".to_string()
        } else if has_results {
            format!(
                "recall_memories returned {} result(s)",
                capture.recalled_memories.len()
            )
        } else {
            "recall_memories returned no results".to_string()
        }
    } else if recall_calls.is_empty() {
        "Agent correctly did not call recall_memories".to_string()
    } else if has_results {
        format!(
            "recall_memories returned {} result(s) but expected none",
            capture.recalled_memories.len()
        )
    } else {
        "recall_memories returned no results (as expected)".to_string()
    };

    AssertionResult {
        assertion_type: "memory_hit".to_string(),
        passed,
        score: None,
        threshold: None,
        detail,
    }
}

fn eval_memory_source(capture: &ResponseCapture, expected_source: &str) -> AssertionResult {
    let source_suffix = expected_source
        .strip_prefix("source/")
        .unwrap_or(expected_source);

    let tag_to_find = format!("source/{}", source_suffix);

    let found = capture.recalled_memories.iter().any(|memory| {
        memory
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|tags| {
                tags.iter().any(|tag| {
                    tag.as_str()
                        .map(|s| s == tag_to_find || s.ends_with(&format!("/{}", source_suffix)))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    AssertionResult {
        assertion_type: "memory_source".to_string(),
        passed: found,
        score: None,
        threshold: None,
        detail: if found {
            format!("Found memory from source '{}'", expected_source)
        } else if capture.recalled_memories.is_empty() {
            format!(
                "No memories recalled; expected memory from source '{}'",
                expected_source
            )
        } else {
            let actual_sources: Vec<String> = capture
                .recalled_memories
                .iter()
                .filter_map(|m| {
                    m.get("tags")
                        .and_then(|t| t.as_array())
                        .and_then(|tags| {
                            tags.iter().find_map(|tag| {
                                tag.as_str()
                                    .filter(|s| s.starts_with("source/"))
                                    .map(String::from)
                            })
                        })
                })
                .collect();
            format!(
                "No memory from source '{}'; sources found: [{}]",
                expected_source,
                actual_sources.join(", ")
            )
        },
    }
}

fn eval_memory_query(capture: &ResponseCapture, expected_substring: &str) -> AssertionResult {
    let recall_queries: Vec<String> = capture
        .tool_calls
        .iter()
        .filter(|tc| tc.tool_name == "recall_memories")
        .filter_map(|tc| {
            tc.arguments
                .get("query")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();

    let expected_lower = expected_substring.to_lowercase();
    let found = recall_queries
        .iter()
        .any(|q| q.to_lowercase().contains(&expected_lower));

    AssertionResult {
        assertion_type: "memory_query".to_string(),
        passed: found,
        score: None,
        threshold: None,
        detail: if found {
            format!("recall_memories query contains '{}'", expected_substring)
        } else if recall_queries.is_empty() {
            "Agent did not call recall_memories".to_string()
        } else {
            format!(
                "recall_memories queries [{}] do not contain '{}'",
                recall_queries
                    .iter()
                    .map(|q| format!("'{q}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
                expected_substring
            )
        },
    }
}

fn eval_tool_called(capture: &ResponseCapture, tool_name: &str) -> AssertionResult {
    let called = capture
        .tool_calls
        .iter()
        .any(|tc| tc.tool_name == tool_name);
    AssertionResult {
        assertion_type: "tool_called".to_string(),
        passed: called,
        score: None,
        threshold: None,
        detail: if called {
            format!("Tool '{}' was called", tool_name)
        } else {
            let called_tools: Vec<&str> = capture
                .tool_calls
                .iter()
                .map(|tc| tc.tool_name.as_str())
                .collect();
            format!(
                "Tool '{}' was not called; tools called: [{}]",
                tool_name,
                called_tools.join(", ")
            )
        },
    }
}

fn eval_tool_not_called(capture: &ResponseCapture, tool_name: &str) -> AssertionResult {
    let called = capture
        .tool_calls
        .iter()
        .any(|tc| tc.tool_name == tool_name);
    AssertionResult {
        assertion_type: "tool_not_called".to_string(),
        passed: !called,
        score: None,
        threshold: None,
        detail: if !called {
            format!("Tool '{}' was correctly not called", tool_name)
        } else {
            let call_count = capture
                .tool_calls
                .iter()
                .filter(|tc| tc.tool_name == tool_name)
                .count();
            format!(
                "Tool '{}' was called {} time(s) but should not have been",
                tool_name, call_count
            )
        },
    }
}

fn eval_latency_ms(capture: &ResponseCapture, config: &LatencyConfig) -> AssertionResult {
    let passed = capture.latency_ms <= config.max;
    AssertionResult {
        assertion_type: "latency_ms".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: format!(
            "Latency {}ms {} {}ms max",
            capture.latency_ms,
            if passed { "<=" } else { ">" },
            config.max
        ),
    }
}

fn eval_token_count(capture: &ResponseCapture, config: &TokenCountConfig) -> AssertionResult {
    let total = capture.input_tokens + capture.output_tokens;
    let passed = total <= config.max;
    AssertionResult {
        assertion_type: "token_count".to_string(),
        passed,
        score: None,
        threshold: None,
        detail: format!(
            "Total tokens {} ({} input + {} output) {} {} max",
            total,
            capture.input_tokens,
            capture.output_tokens,
            if passed { "<=" } else { ">" },
            config.max
        ),
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "embedding dimensions must match");
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let mag_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

// ---------------------------------------------------------------------------
// LLM-as-judge helpers
// ---------------------------------------------------------------------------

fn build_judge_prompt(
    question: &str,
    response: &str,
    recalled_memories: &[serde_json::Value],
    criteria: &str,
) -> String {
    let context_section = if recalled_memories.is_empty() {
        "Retrieved Context: (none)".to_string()
    } else {
        let memories_text: String = recalled_memories
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let summary = m
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no summary)");
                format!("  {}. {}", i + 1, summary)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Retrieved Context:\n{}", memories_text)
    };

    format!(
        r#"You are evaluating an AI agent's response.

Question: {question}
Agent Response: {response}
{context_section}

Evaluation Criteria:
{criteria}

Score the response from 0.0 to 1.0 based on how well it meets the criteria.
- 1.0 means the response fully satisfies all criteria.
- 0.0 means the response completely fails the criteria.
- Use intermediate values for partial matches.

Respond with ONLY a JSON object on a single line, no other text:
{{"score": <float>, "reasoning": "<brief explanation>"}}"#
    )
}

#[derive(Debug, Deserialize)]
struct JudgeResponse {
    score: f64,
    reasoning: String,
}

fn parse_judge_response(raw: &str) -> Result<(f64, String), String> {
    let cleaned = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim());
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    let json_str = if let Some(start) = cleaned.find('{') {
        if let Some(end) = cleaned.rfind('}') {
            &cleaned[start..=end]
        } else {
            return Err("No closing brace in judge response".to_string());
        }
    } else {
        return Err("No JSON object found in judge response".to_string());
    };

    let parsed: JudgeResponse =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {e}"))?;

    let score = parsed.score.clamp(0.0, 1.0);

    Ok((score, parsed.reasoning))
}

// ---------------------------------------------------------------------------
// Faithfulness helpers
// ---------------------------------------------------------------------------

fn resolve_faithfulness_context(
    test_case: &TestCaseDef,
    capture: &ResponseCapture,
) -> Result<String, String> {
    if let Some(ref ctx) = test_case.context {
        return Ok(ctx.clone());
    }

    if capture.recalled_memories.is_empty() {
        return Err(
            "No context available for faithfulness scoring. \
             Provide a 'context' field in the test case or ensure \
             the agent calls recall_memories."
                .to_string(),
        );
    }

    let context = capture
        .recalled_memories
        .iter()
        .filter_map(|m| {
            let summary = m.get("summary").and_then(|v| v.as_str());
            let full_text = m.get("fullText").and_then(|v| v.as_str());
            full_text.or(summary).map(String::from)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    if context.is_empty() {
        Err("Recalled memories have no text content.".to_string())
    } else {
        Ok(context)
    }
}

fn build_claim_decomposition_prompt(response: &str) -> String {
    format!(
        r#"Break down the following response into individual, atomic factual claims.
Each claim should be a single, self-contained statement that can be independently verified.
Do not include opinions, hedging language, or meta-statements about the response itself.

Response:
{response}

Return a JSON array of strings, where each string is one atomic claim.
Example: ["PostgreSQL 15 is used for the payments database", "Read replicas handle read traffic"]

Return ONLY the JSON array, no other text:
"#
    )
}

fn build_entailment_prompt(claims: &[String], context: &str) -> String {
    let claims_list = claims
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are checking whether factual claims are supported by the given context.

Context:
{context}

Claims:
{claims_list}

For each claim, determine if it is SUPPORTED (the context contains information that confirms or implies this claim) or UNSUPPORTED (the context does not contain sufficient information to confirm this claim).

Return a JSON array of objects, one per claim, in the same order:
[{{"claim": 1, "verdict": "supported"}}, {{"claim": 2, "verdict": "unsupported"}}]

Return ONLY the JSON array, no other text:
"#
    )
}

fn strip_code_fences_str(s: &str) -> String {
    let trimmed = s.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```yaml"))
        .or_else(|| trimmed.strip_prefix("```yml"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    stripped
        .strip_suffix("```")
        .unwrap_or(stripped)
        .trim()
        .to_string()
}

fn extract_json_array(s: &str) -> Result<&str, String> {
    let start = s.find('[').ok_or("No opening bracket found")?;
    let end = s.rfind(']').ok_or("No closing bracket found")?;
    if end <= start {
        return Err("Invalid JSON array brackets".to_string());
    }
    Ok(&s[start..=end])
}

fn parse_json_string_array(raw: &str) -> Result<Vec<String>, String> {
    let cleaned = strip_code_fences_str(raw);
    let json_str = extract_json_array(&cleaned)?;
    serde_json::from_str::<Vec<String>>(json_str).map_err(|e| format!("JSON parse error: {e}"))
}

#[derive(Debug, Deserialize)]
struct EntailmentVerdict {
    claim: usize,
    verdict: String,
}

fn parse_entailment_verdicts(raw: &str) -> Result<Vec<EntailmentVerdict>, String> {
    let cleaned = strip_code_fences_str(raw);
    let json_str = extract_json_array(&cleaned)?;
    serde_json::from_str::<Vec<EntailmentVerdict>>(json_str)
        .map_err(|e| format!("JSON parse error: {e}"))
}

// ---------------------------------------------------------------------------
// Assertion priority and type name
// ---------------------------------------------------------------------------

fn assertion_priority(assertion: &Assertion) -> u8 {
    match assertion {
        Assertion::Contains(_)
        | Assertion::NotContains(_)
        | Assertion::ContainsAny(_)
        | Assertion::ContainsAll(_)
        | Assertion::Matches(_)
        | Assertion::NotMatches(_)
        | Assertion::StartsWith(_) => 1,

        Assertion::MemoryHit(_)
        | Assertion::MemorySource(_)
        | Assertion::MemoryQuery(_)
        | Assertion::ToolCalled(_)
        | Assertion::ToolNotCalled(_) => 2,

        Assertion::LatencyMs(_) | Assertion::TokenCount(_) => 3,

        Assertion::SemanticSimilarity(_) => 4,

        Assertion::LlmJudge(_) => 5,

        Assertion::Faithfulness(_) => 6,
    }
}

fn assertion_type_name(assertion: &Assertion) -> String {
    match assertion {
        Assertion::Contains(_) => "contains",
        Assertion::NotContains(_) => "not_contains",
        Assertion::ContainsAny(_) => "contains_any",
        Assertion::ContainsAll(_) => "contains_all",
        Assertion::Matches(_) => "matches",
        Assertion::NotMatches(_) => "not_matches",
        Assertion::StartsWith(_) => "starts_with",
        Assertion::MemoryHit(_) => "memory_hit",
        Assertion::MemorySource(_) => "memory_source",
        Assertion::MemoryQuery(_) => "memory_query",
        Assertion::ToolCalled(_) => "tool_called",
        Assertion::ToolNotCalled(_) => "tool_not_called",
        Assertion::LlmJudge(_) => "llm_judge",
        Assertion::SemanticSimilarity(_) => "semantic_similarity",
        Assertion::Faithfulness(_) => "faithfulness",
        Assertion::LatencyMs(_) => "latency_ms",
        Assertion::TokenCount(_) => "token_count",
    }
    .to_string()
}

fn resolve_threshold(
    assertion_threshold: Option<f64>,
    test_threshold: Option<f64>,
    config_threshold: f64,
) -> f64 {
    assertion_threshold
        .or(test_threshold)
        .unwrap_or(config_threshold)
}

fn evaluate_single_local(
    assertion: &Assertion,
    capture: &ResponseCapture,
    test_case: &TestCaseDef,
    config: &TestConfig,
) -> AssertionResult {
    match assertion {
        Assertion::Contains(expected) => eval_contains(&capture.response_text, expected),
        Assertion::NotContains(excluded) => eval_not_contains(&capture.response_text, excluded),
        Assertion::ContainsAny(candidates) => {
            eval_contains_any(&capture.response_text, candidates)
        }
        Assertion::ContainsAll(required) => eval_contains_all(&capture.response_text, required),
        Assertion::Matches(pattern) => eval_matches(&capture.response_text, pattern),
        Assertion::NotMatches(pattern) => eval_not_matches(&capture.response_text, pattern),
        Assertion::StartsWith(prefix) => eval_starts_with(&capture.response_text, prefix),
        Assertion::MemoryHit(expected) => eval_memory_hit(capture, *expected),
        Assertion::MemorySource(source) => eval_memory_source(capture, source),
        Assertion::MemoryQuery(substring) => eval_memory_query(capture, substring),
        Assertion::ToolCalled(name) => eval_tool_called(capture, name),
        Assertion::ToolNotCalled(name) => eval_tool_not_called(capture, name),
        Assertion::LatencyMs(cfg) => eval_latency_ms(capture, cfg),
        Assertion::TokenCount(cfg) => eval_token_count(capture, cfg),

        Assertion::LlmJudge(cfg) => {
            let threshold = resolve_threshold(cfg.threshold, test_case.threshold, config.threshold);
            AssertionResult {
                assertion_type: "llm_judge".to_string(),
                passed: false,
                score: None,
                threshold: Some(threshold),
                detail: "llm_judge evaluated in-container".to_string(),
            }
        }
        Assertion::SemanticSimilarity(cfg) => {
            let threshold = resolve_threshold(cfg.threshold, test_case.threshold, config.threshold);
            AssertionResult {
                assertion_type: "semantic_similarity".to_string(),
                passed: false,
                score: None,
                threshold: Some(threshold),
                detail: "semantic_similarity evaluated in-container".to_string(),
            }
        }
        Assertion::Faithfulness(cfg) => {
            let threshold = resolve_threshold(cfg.threshold, test_case.threshold, config.threshold);
            AssertionResult {
                assertion_type: "faithfulness".to_string(),
                passed: false,
                score: None,
                threshold: Some(threshold),
                detail: "faithfulness evaluated in-container".to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Glob matching
// ---------------------------------------------------------------------------

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();

    if !pattern.contains('*') {
        return text == pattern || text.contains(&pattern);
    }

    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 2 && parts[0].is_empty() && parts[1].is_empty() {
        return true;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match text[pos..].find(part) {
            Some(found) => {
                if i == 0 && found != 0 {
                    return false;
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }

    if !pattern.ends_with('*') {
        pos == text.len()
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn compute_summary(agent_name: &str, results: &[TestResult]) -> TestSummary {
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;
    let total_latency: u64 = results.iter().map(|r| r.latency_ms).sum();
    let total_input: u64 = results.iter().map(|r| r.input_tokens).sum();
    let total_output: u64 = results.iter().map(|r| r.output_tokens).sum();
    let pass_rate = if results.is_empty() {
        0.0
    } else {
        passed as f64 / results.len() as f64
    };

    TestSummary {
        agent: agent_name.to_string(),
        total: results.len(),
        passed,
        failed,
        pass_rate,
        total_latency_ms: total_latency,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        estimated_cost_usd: 0.0,
    }
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn emit_human_output(summary: &TestSummary, results: &[TestResult]) {
    eprintln!();
    eprintln!("  Testing {} ({} tests)", summary.agent, summary.total);
    eprintln!();

    for result in results {
        let status = if result.passed {
            console::style("PASS").green().bold()
        } else {
            console::style("FAIL").red().bold()
        };

        let assertion_count = result.assertions.len();
        let retries_note = if result.retries_used > 0 {
            format!(" (retry {})", result.retries_used)
        } else {
            String::new()
        };

        eprintln!(
            "  {status}  {:<35} {:>5}ms   {} assertions{retries_note}",
            result.name, result.latency_ms, assertion_count,
        );

        for assertion in &result.assertions {
            if !assertion.passed {
                let score_info = match (assertion.score, assertion.threshold) {
                    (Some(score), Some(threshold)) => {
                        format!("{:.2} < {:.2} threshold", score, threshold)
                    }
                    _ => String::new(),
                };

                eprintln!(
                    "        - {}: {}",
                    assertion.assertion_type,
                    if score_info.is_empty() {
                        assertion.detail.clone()
                    } else {
                        format!("{score_info}")
                    }
                );
                if !score_info.is_empty() && !assertion.detail.is_empty() {
                    eprintln!("          \"{}\"", assertion.detail);
                }
            }
        }
    }

    eprintln!();
    eprintln!(
        "  Results: {} passed, {} failed ({:.1}%)",
        summary.passed,
        summary.failed,
        summary.pass_rate * 100.0
    );
    eprintln!(
        "  Total time: {:.1}s",
        summary.total_latency_ms as f64 / 1000.0
    );
    eprintln!(
        "  Total tokens: {} input / {} output",
        format_number(summary.total_input_tokens),
        format_number(summary.total_output_tokens)
    );
    if summary.estimated_cost_usd > 0.0 {
        eprintln!("  Estimated cost: ${:.2}", summary.estimated_cost_usd);
    }
    eprintln!();
}

fn emit_json_output(summary: &TestSummary, results: &[TestResult]) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let output = JsonOutput {
        agent: summary.agent.clone(),
        timestamp: format!("{timestamp}"),
        summary: summary.clone(),
        tests: results.to_vec(),
    };
    let json = serde_json::to_string_pretty(&output).expect("TestResult is always serializable");
    println!("{json}");
}

fn emit_junit_output(
    summary: &TestSummary,
    results: &[TestResult],
    output_path: &Path,
) -> Result<(), CliError> {
    let mut report = Report::new(summary.agent.clone());

    let mut suite = JunitSuite::new(summary.agent.clone());
    suite.set_time(Duration::from_millis(summary.total_latency_ms));

    for result in results {
        let status = if result.passed {
            quick_junit::TestCaseStatus::success()
        } else {
            let failure_messages: Vec<String> = result
                .assertions
                .iter()
                .filter(|a| !a.passed)
                .map(|a| {
                    let score_part = match (a.score, a.threshold) {
                        (Some(s), Some(t)) => format!("{:.2} < {:.2} threshold", s, t),
                        _ => String::new(),
                    };
                    if score_part.is_empty() {
                        format!("{}: {}", a.assertion_type, a.detail)
                    } else {
                        format!("{}: {} - {}", a.assertion_type, score_part, a.detail)
                    }
                })
                .collect();

            let message = failure_messages.first().cloned().unwrap_or_default();
            let description = failure_messages.join("\n");

            quick_junit::TestCaseStatus::NonSuccess {
                kind: NonSuccessKind::Failure,
                message: Some(message.into()),
                ty: None,
                description: Some(description.into()),
                reruns: vec![],
            }
        };

        let mut case = JunitTestCase::new(result.name.clone(), status);
        case.set_time(Duration::from_millis(result.latency_ms));
        suite.add_test_case(case);
    }

    report.add_test_suite(suite);

    let mut writer = std::io::BufWriter::new(std::fs::File::create(output_path)?);
    report.serialize(&mut writer).map_err(|e| {
        CliError::ContainerRuntimeError {
            message: format!("Failed to serialize JUnit XML: {e}"),
        }
    })?;

    eprintln!("JUnit XML written to {}", output_path.display());

    Ok(())
}

fn emit_output(
    summary: &TestSummary,
    results: &[TestResult],
    args: &TestArgs,
) -> Result<(), CliError> {
    emit_human_output(summary, results);

    if args.json {
        emit_json_output(summary, results);
    }

    if let Some(ref junit_path) = args.junit {
        emit_junit_output(summary, results, junit_path)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Container lifecycle helpers
// ---------------------------------------------------------------------------

async fn start_test_container(
    runtime: &dyn ContainerRuntime,
    image: &str,
    config: &AgentConfig,
    agent_dir: &Path,
    test_file_path: &Path,
) -> Result<container::ContainerId, CliError> {
    let (key_name, _key_set) = resolve_api_key(&config.model);
    let mut env_vars = HashMap::new();

    if key_name == "VERTEX_API_KEY" {
        crate::agent_config::resolve_vertex_env(&mut env_vars);
    } else if let Ok(val) = std::env::var(&key_name) {
        env_vars.insert(key_name.clone(), val);
    }

    // Also forward keys for common providers the judge might use
    for var in &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "VERTEX_PROJECT_ID",
        "VERTEX_LOCATION",
        "VERTEX_API_KEY",
    ] {
        if !env_vars.contains_key(*var) {
            if let Ok(val) = std::env::var(var) {
                env_vars.insert(var.to_string(), val);
            }
        }
    }

    // Forward Ollama host and recalld embedding config
    let ollama_url = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://host.docker.internal:11434".to_string());
    env_vars.insert("OLLAMA_BASE_URL".to_string(), ollama_url.clone());
    env_vars.insert("RECALLD_EMBEDDING_PROVIDER".to_string(), "ollama".to_string());
    env_vars.insert("RECALLD_EMBEDDING_MODEL".to_string(), "embeddinggemma:latest".to_string());
    env_vars.insert("RECALLD_EMBEDDING_BASE_URL".to_string(), ollama_url);
    env_vars.insert("RECALLD_EMBEDDING_DIMENSIONS".to_string(), "768".to_string());
    env_vars.insert("RECALLD_STORAGE_DATA_DIR".to_string(), "/data/recalld".to_string());
    env_vars.insert("RECALLD_DAEMON_SOCKET".to_string(), "/data/recalld/recalld.sock".to_string());

    let config_path = agent_dir.join("pupil.yaml").canonicalize().map_err(|e| {
        CliError::Io(e)
    })?;
    let test_path = test_file_path.canonicalize().map_err(|e| {
        CliError::Io(e)
    })?;

    let mut test_volumes = vec![
        format!("{}:/tmp/pupil.yaml:ro", config_path.display()),
        format!("{}:/tmp/tests.yaml:ro", test_path.display()),
    ];

    let gcp_creds_path = dirs::home_dir()
        .map(|h| h.join(".config/gcloud/application_default_credentials.json"));
    if let Some(ref creds) = gcp_creds_path {
        if creds.exists() {
            test_volumes.push(format!(
                "{}:/tmp/gcp-credentials.json:ro",
                creds.to_string_lossy()
            ));
            env_vars.insert(
                "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
                "/tmp/gcp-credentials.json".to_string(),
            );
        }
    }

    let opts = RunOptions {
        read_only: false,
        tmpfs: vec![],
        volumes: test_volumes,
        cap_drop_all: false,
        extra_flags: vec!["--user".to_string(), "root".to_string()],
        env: env_vars,
        network: None,
        detach: true,
        name: None,
        entrypoint: Some("pupil-agent".to_string()),
        command: vec!["idle".to_string()],
        add_host: Some("host.docker.internal:host-gateway".to_string()),
        ..Default::default()
    };
    runtime
        .run(image, &opts)
        .await
        .map_err(|e| CliError::RunContainerFailed {
            message: format!("Failed to start test container: {e}"),
        })
}

async fn wait_for_agent_ready(
    runtime: &dyn ContainerRuntime,
    container_id: &container::ContainerId,
    timeout: Duration,
) -> Result<(), CliError> {
    let start = std::time::Instant::now();
    loop {
        let result = runtime
            .exec(container_id, &["pupil-agent", "--version"], &[])
            .await;
        if result.is_ok() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(CliError::ContainerRuntimeError {
                message: format!("Agent did not become ready within {timeout:?}"),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ---------------------------------------------------------------------------
// Test generation
// ---------------------------------------------------------------------------

fn build_generation_prompt(curriculum_text: &str, count: usize) -> String {
    format!(
        r#"You are generating test questions for an AI agent that has learned
the following material. Generate exactly {count} test cases.

For each question:

1. Write a question that tests whether the agent understood the material
   (not just memorized keywords).
2. Choose appropriate assertion types:
   - Use `contains` or `contains_any` for questions with specific expected
     terms (names, tools, versions).
   - Use `llm_judge` with clear criteria for open-ended questions where
     the answer could be phrased many ways.
   - Include `memory_hit: true` for questions that should be answerable
     from the curriculum.
   - Include `memory_source` when the answer clearly comes from a
     specific document.
3. Include 1-2 negative tests: questions the agent should NOT be able
   to answer from its curriculum. Use `memory_hit: false` and
   `llm_judge` with criteria checking for honest uncertainty.
4. Vary difficulty: include both factual recall ("What database does X use?")
   and conceptual understanding ("Why was Y chosen over Z?").
5. Give each test a descriptive kebab-case name.

Output valid YAML matching this schema:

```yaml
config:
  temperature: 0
  retries: 1
  threshold: 0.8

tests:
  - name: example-test-name
    question: "..."
    expects:
      - memory_hit: true
      - contains: "expected term"
```

Curriculum material:

{curriculum_text}

Generate exactly {count} test cases as valid YAML. Output ONLY the YAML, no other text:
"#
    )
}

fn is_supported_format(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "txt" | "html" | "htm" | "json" | "yaml" | "yml")
    )
}

fn read_curriculum_sources(config: &AgentConfig, agent_dir: &Path) -> Result<String, CliError> {
    let mut parts = Vec::new();

    for source in &config.curriculum.sources {
        match source {
            SourceEntry::Short(s) => {
                if s.starts_with("http://") || s.starts_with("https://") {
                    parts.push(format!(
                        "--- Source: {} ---\n(URL source, content not included)\n",
                        s
                    ));
                } else {
                    let path = agent_dir.join(s);
                    if path.is_file() {
                        match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                parts.push(format!(
                                    "--- Source: {} ---\n{}\n",
                                    path.display(),
                                    content
                                ));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "Failed to read curriculum source"
                                );
                            }
                        }
                    } else if path.is_dir() {
                        for entry in WalkDir::new(&path)
                            .into_iter()
                            .filter_map(|e| e.ok())
                            .filter(|e| e.file_type().is_file())
                            .filter(|e| is_supported_format(e.path()))
                        {
                            match std::fs::read_to_string(entry.path()) {
                                Ok(content) => {
                                    parts.push(format!(
                                        "--- Source: {} ---\n{}\n",
                                        entry.path().display(),
                                        content
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        path = %entry.path().display(),
                                        error = %e,
                                        "Failed to read curriculum file"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            SourceEntry::Long(sc) => {
                if let Some(ref url) = sc.url {
                    parts.push(format!(
                        "--- Source: {} ---\n(URL source, content not included)\n",
                        url
                    ));
                } else if let Some(ref p) = sc.path {
                    let path = agent_dir.join(p);
                    if path.is_file() {
                        match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                parts.push(format!(
                                    "--- Source: {} ---\n{}\n",
                                    path.display(),
                                    content
                                ));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "Failed to read curriculum source"
                                );
                            }
                        }
                    } else if path.is_dir() {
                        for entry in WalkDir::new(&path)
                            .into_iter()
                            .filter_map(|e| e.ok())
                            .filter(|e| e.file_type().is_file())
                            .filter(|e| is_supported_format(e.path()))
                        {
                            match std::fs::read_to_string(entry.path()) {
                                Ok(content) => {
                                    parts.push(format!(
                                        "--- Source: {} ---\n{}\n",
                                        entry.path().display(),
                                        content
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        path = %entry.path().display(),
                                        error = %e,
                                        "Failed to read curriculum file"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Also look in curriculum/ directory if it exists
    let curriculum_dir = agent_dir.join("curriculum");
    if curriculum_dir.is_dir() {
        for entry in WalkDir::new(&curriculum_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_supported_format(e.path()))
        {
            let already_included = parts.iter().any(|p| {
                p.contains(&format!("--- Source: {} ---", entry.path().display()))
            });
            if !already_included {
                match std::fs::read_to_string(entry.path()) {
                    Ok(content) => {
                        parts.push(format!(
                            "--- Source: {} ---\n{}\n",
                            entry.path().display(),
                            content
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %entry.path().display(),
                            error = %e,
                            "Failed to read curriculum file"
                        );
                    }
                }
            }
        }
    }

    Ok(parts.join("\n"))
}

async fn generate_tests(
    agent_name: &str,
    config: &AgentConfig,
    agent_dir: &Path,
    count: usize,
    output_path: Option<&Path>,
) -> Result<(), CliError> {
    let curriculum_text = read_curriculum_sources(config, agent_dir)?;

    if curriculum_text.trim().is_empty() {
        return Err(CliError::NoCurriculumSources {
            path: agent_dir.join("curriculum"),
        });
    }

    let max_chars = 100_000;
    let truncated = if curriculum_text.len() > max_chars {
        format!(
            "{}\n\n... (truncated, {} more characters)",
            &curriculum_text[..max_chars],
            curriculum_text.len() - max_chars
        )
    } else {
        curriculum_text
    };

    let prompt = build_generation_prompt(&truncated, count);

    eprintln!("Generating {} test cases for '{}'...", count, agent_name);
    eprintln!(
        "This requires an LLM call. Model: {}",
        config.model
    );

    // The test generation prompt is written to stdout as raw YAML.
    // In a full implementation, this would call the LLM via the container.
    // For the host-side CLI, we generate via container exec.

    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;
    let image = image_ref(&config.name, None);

    let config_path = agent_dir.join("pupil.yaml");
    let dummy_test_path = agent_dir.join("tests.yaml");
    let container_id = start_test_container(
        runtime.as_ref(), &image, config, agent_dir,
        if dummy_test_path.exists() { &dummy_test_path } else { &config_path },
    ).await?;

    let _prompt_json =
        serde_json::to_string(&prompt).map_err(|e| CliError::Json(e))?;

    let result = runtime
        .exec(
            &container_id,
            &["pupil-agent", "test", "--file", "/dev/stdin", "--json"],
            &[],
        )
        .await;

    let _ = runtime.rm(&container_id, true).await;

    let generated_yaml = match result {
        Ok(output) => {
            if output.stdout.trim().is_empty() {
                // Fallback: output the generation prompt for manual use
                eprintln!(
                    "Agent container did not produce test output. \
                     Outputting generation prompt for manual use."
                );
                format!(
                    "# Generated test template for {}\n\
                     # Use this prompt with your LLM to generate tests:\n\
                     #\n\
                     config:\n  temperature: 0\n  retries: 1\n  threshold: 0.8\n\n\
                     tests:\n\
                     {}",
                    agent_name,
                    (1..=count)
                        .map(|i| format!(
                            "  - name: test-{i}\n    question: \"TODO\"\n    expects:\n      - contains: \"TODO\"\n"
                        ))
                        .collect::<String>()
                )
            } else {
                output.stdout
            }
        }
        Err(_) => {
            // Container exec failed; generate a template instead
            eprintln!(
                "Could not run test generation in container. \
                 Generating template instead."
            );
            format!(
                "config:\n  temperature: 0\n  retries: 1\n  threshold: 0.8\n\n\
                 tests:\n\
                 {}",
                (1..=count)
                    .map(|i| format!(
                        "  - name: test-{i}\n    question: \"TODO: Write a question testing the agent's knowledge\"\n    expects:\n      - memory_hit: true\n      - contains: \"TODO: expected term\"\n"
                    ))
                    .collect::<String>()
            )
        }
    };

    let cleaned = strip_code_fences_str(&generated_yaml);

    match serde_yml::from_str::<TestFile>(&cleaned) {
        Ok(test_file) => {
            if let Err(validation_errors) = test_file.validate() {
                eprintln!("Generated tests have validation errors:");
                for err in &validation_errors {
                    eprintln!("  - {err}");
                }
                eprintln!("Writing raw output anyway; please fix manually.");
            }
        }
        Err(e) => {
            eprintln!("Generated output is not valid YAML: {e}");
            eprintln!("Writing raw output; please fix manually.");
        }
    }

    match output_path {
        Some(path) => {
            std::fs::write(path, &cleaned)?;
            eprintln!("Generated {} tests written to {}", count, path.display());
        }
        None => {
            println!("{}", cleaned);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub async fn execute(args: TestArgs) -> Result<(), CliError> {
    let (agent_dir, config) = resolve_agent(args.name.as_deref())?;

    if args.generate {
        return generate_tests(
            &config.name,
            &config,
            &agent_dir,
            args.count,
            args.output.as_deref(),
        )
        .await;
    }

    // 1. Load and validate test file
    let test_file_path = args
        .file
        .clone()
        .unwrap_or_else(|| PathBuf::from("tests.yaml"));
    let resolved_path = if test_file_path.is_absolute() {
        test_file_path.clone()
    } else {
        agent_dir.join(&test_file_path)
    };
    let test_yaml = std::fs::read_to_string(&resolved_path).map_err(|e| {
        CliError::ConfigInvalid {
            message: format!("Cannot read test file '{}': {e}", resolved_path.display()),
        }
    })?;
    let test_file: TestFile =
        serde_yml::from_str(&test_yaml).map_err(|e| CliError::Yaml(e))?;
    if let Err(validation_errors) = test_file.validate() {
        for err in &validation_errors {
            eprintln!("  Validation error: {err}");
        }
        std::process::exit(2);
    }

    // 2. Apply CLI overrides
    let mut test_config = test_file.config.clone();
    if let Some(retries) = args.retries {
        test_config.retries = retries;
    }
    if let Some(timeout) = args.timeout {
        test_config.timeout_secs = timeout;
    }
    if let Some(threshold) = args.threshold {
        test_config.threshold = threshold;
    }

    // 3. Filter tests
    let tests: Vec<&TestCaseDef> = if let Some(ref filter) = args.filter {
        test_file
            .tests
            .iter()
            .filter(|t| {
                glob_match(filter, &t.name) || t.tags.iter().any(|tag| glob_match(filter, tag))
            })
            .collect()
    } else {
        test_file.tests.iter().collect()
    };

    if tests.is_empty() {
        eprintln!(
            "No tests match filter '{}'",
            args.filter.as_deref().unwrap_or("")
        );
        std::process::exit(2);
    }

    // 4. Detect runtime and start container
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let image = image_ref(&config.name, None);
    let container_id = start_test_container(
        runtime.as_ref(), &image, &config, &agent_dir, &resolved_path,
    ).await?;

    // 5. Wait for agent readiness
    if let Err(e) = wait_for_agent_ready(
        runtime.as_ref(),
        &container_id,
        Duration::from_secs(30),
    )
    .await
    {
        let _ = runtime.rm(&container_id, true).await;
        return Err(e);
    }

    // 6. Execute tests inside the container
    let output = runtime
        .exec(
            &container_id,
            &[
                "pupil-agent",
                "--config", "/tmp/pupil.yaml",
                "test",
                "--file",
                "/tmp/tests.yaml",
                "--json",
            ],
            &[],
        )
        .await;

    // 7. Stop and remove container
    let _ = runtime.rm(&container_id, true).await;

    // 8. Parse results
    let results: Vec<TestResult> = match output {
        Ok(ref out) => {
            if !out.stderr.is_empty() {
                tracing::debug!(stderr = %out.stderr, "Container test stderr");
            }
            eprintln!("[DEBUG] Raw stdout length: {}", out.stdout.len());
            if let Some(first_test) = out.stdout.find("\"response\"") {
                let snippet = &out.stdout[first_test..out.stdout.len().min(first_test + 800)];
                eprintln!("[DEBUG] First response: ...{}", snippet);
            }
            let raw: serde_json::Value = serde_json::from_str(&out.stdout).map_err(|e| {
                CliError::ConfigInvalid {
                    message: format!("Failed to parse test results from container: {e}"),
                }
            })?;
            // The container outputs {"summary": {...}, "tests": [...]}.
            // Extract the tests array.
            let tests_value = if let Some(tests) = raw.get("tests") {
                tests.clone()
            } else {
                raw
            };
            serde_json::from_value(tests_value).map_err(|e| {
                CliError::ConfigInvalid {
                    message: format!("Failed to parse test results: {e}"),
                }
            })?
        }
        Err(container_err) => {
            return Err(CliError::ContainerRuntimeError {
                message: format!("Test execution failed: {container_err}"),
            });
        }
    };

    // 9. Emit output
    let summary = compute_summary(&config.name, &results);
    emit_output(&summary, &results, &args)?;

    // 10. Exit code
    if summary.failed == 0 {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- contains ---

    #[test]
    fn contains_passes_on_substring() {
        let result = eval_contains("Clone the repo and run npm install", "clone the repo");
        assert!(result.passed);
    }

    #[test]
    fn contains_is_case_insensitive() {
        let result = eval_contains("Use PostgreSQL for the database", "postgresql");
        assert!(result.passed);
    }

    #[test]
    fn contains_fails_when_absent() {
        let result = eval_contains("Use npm to install dependencies", "pip install");
        assert!(!result.passed);
        assert!(result.detail.contains("not found"));
    }

    #[test]
    fn contains_handles_empty_response() {
        let result = eval_contains("", "anything");
        assert!(!result.passed);
    }

    // --- not_contains ---

    #[test]
    fn not_contains_passes_when_absent() {
        let result = eval_not_contains("Use npm install", "pip install");
        assert!(result.passed);
    }

    #[test]
    fn not_contains_fails_when_present() {
        let result = eval_not_contains("Run pip install to set up", "pip install");
        assert!(!result.passed);
    }

    // --- contains_any ---

    #[test]
    fn contains_any_passes_on_first_match() {
        let result =
            eval_contains_any("Contact Alice for help", &["Alice".into(), "Bob".into()]);
        assert!(result.passed);
    }

    #[test]
    fn contains_any_passes_on_second_match() {
        let result = eval_contains_any("Contact Bob for help", &["Alice".into(), "Bob".into()]);
        assert!(result.passed);
    }

    #[test]
    fn contains_any_fails_when_none_match() {
        let result =
            eval_contains_any("Contact Charlie for help", &["Alice".into(), "Bob".into()]);
        assert!(!result.passed);
    }

    // --- contains_all ---

    #[test]
    fn contains_all_passes_when_all_present() {
        let result = eval_contains_all(
            "Clone the repo and run npm install, then npm test",
            &[
                "clone the repo".into(),
                "npm install".into(),
                "npm test".into(),
            ],
        );
        assert!(result.passed);
    }

    #[test]
    fn contains_all_fails_when_one_missing() {
        let result = eval_contains_all(
            "Clone the repo and run npm install",
            &[
                "clone the repo".into(),
                "npm install".into(),
                "npm test".into(),
            ],
        );
        assert!(!result.passed);
        assert!(result.detail.contains("npm test"));
    }

    // --- matches ---

    #[test]
    fn matches_passes_on_regex_match() {
        let result = eval_matches("Version v2.3.1 is the latest", r"v\d+\.\d+\.\d+");
        assert!(result.passed);
    }

    #[test]
    fn matches_fails_on_no_match() {
        let result = eval_matches("No version info here", r"v\d+\.\d+\.\d+");
        assert!(!result.passed);
    }

    // --- not_matches ---

    #[test]
    fn not_matches_passes_when_no_match() {
        let result = eval_not_matches("Clean code here", r"\b(TODO|FIXME)\b");
        assert!(result.passed);
    }

    #[test]
    fn not_matches_fails_when_match_found() {
        let result = eval_not_matches("TODO: fix this later", r"\b(TODO|FIXME)\b");
        assert!(!result.passed);
    }

    // --- starts_with ---

    #[test]
    fn starts_with_passes_on_prefix() {
        let result = eval_starts_with("To set up your environment, first clone", "To set up");
        assert!(result.passed);
    }

    #[test]
    fn starts_with_is_case_insensitive() {
        let result = eval_starts_with("TO SET UP your environment", "to set up");
        assert!(result.passed);
    }

    #[test]
    fn starts_with_trims_leading_whitespace() {
        let result = eval_starts_with("  To set up your environment", "To set up");
        assert!(result.passed);
    }

    #[test]
    fn starts_with_fails_on_wrong_prefix() {
        let result = eval_starts_with("First, clone the repo", "To set up");
        assert!(!result.passed);
    }
}

#[cfg(test)]
mod retrieval_tests {
    use super::*;

    fn make_recall_call(query: &str, results: Vec<serde_json::Value>) -> CapturedToolCall {
        CapturedToolCall {
            tool_name: "recall_memories".to_string(),
            arguments: serde_json::json!({"query": query}),
            result: serde_json::json!({"memories": results}),
            duration_ms: 50,
        }
    }

    fn make_memory(summary: &str, tags: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "summary": summary,
            "tags": tags,
        })
    }

    #[test]
    fn memory_hit_true_passes_with_results() {
        let memory = make_memory("Dev setup", &["source/handbook.md"]);
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![make_recall_call("dev setup", vec![memory.clone()])],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![memory],
        };
        let result = eval_memory_hit(&capture, true);
        assert!(result.passed);
    }

    #[test]
    fn memory_hit_true_fails_with_no_results() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![make_recall_call("unknown topic", vec![])],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_memory_hit(&capture, true);
        assert!(!result.passed);
    }

    #[test]
    fn memory_hit_false_passes_with_no_results() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![make_recall_call("pet policy", vec![])],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_memory_hit(&capture, false);
        assert!(result.passed);
    }

    #[test]
    fn memory_hit_false_passes_when_no_recall_called() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_memory_hit(&capture, false);
        assert!(result.passed);
    }

    #[test]
    fn memory_source_passes_with_matching_tag() {
        let memory =
            make_memory("Deploy process", &["source/deploy-guide.md", "type/procedure"]);
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![memory],
        };
        let result = eval_memory_source(&capture, "deploy-guide.md");
        assert!(result.passed);
    }

    #[test]
    fn memory_source_fails_with_wrong_tag() {
        let memory = make_memory("Dev setup", &["source/handbook.md"]);
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![memory],
        };
        let result = eval_memory_source(&capture, "deploy-guide.md");
        assert!(!result.passed);
    }

    #[test]
    fn memory_query_passes_with_matching_substring() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![make_recall_call("deployment process staging", vec![])],
            latency_ms: 100,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_memory_query(&capture, "deploy");
        assert!(result.passed);
    }

    #[test]
    fn tool_called_passes_when_called() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![CapturedToolCall {
                tool_name: "web_search".into(),
                arguments: serde_json::json!({}),
                result: serde_json::json!({}),
                duration_ms: 200,
            }],
            latency_ms: 500,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_tool_called(&capture, "web_search");
        assert!(result.passed);
    }

    #[test]
    fn tool_not_called_passes_when_not_called() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![CapturedToolCall {
                tool_name: "recall_memories".into(),
                arguments: serde_json::json!({}),
                result: serde_json::json!({}),
                duration_ms: 50,
            }],
            latency_ms: 300,
            input_tokens: 500,
            output_tokens: 200,
            recalled_memories: vec![],
        };
        let result = eval_tool_not_called(&capture, "web_search");
        assert!(result.passed);
    }
}

#[cfg(test)]
mod operational_tests {
    use super::*;

    #[test]
    fn latency_passes_under_max() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 500,
            input_tokens: 100,
            output_tokens: 50,
            recalled_memories: vec![],
        };
        let result = eval_latency_ms(&capture, &LatencyConfig { max: 1000 });
        assert!(result.passed);
    }

    #[test]
    fn latency_fails_over_max() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 15000,
            input_tokens: 100,
            output_tokens: 50,
            recalled_memories: vec![],
        };
        let result = eval_latency_ms(&capture, &LatencyConfig { max: 10000 });
        assert!(!result.passed);
    }

    #[test]
    fn latency_passes_at_exact_max() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 10000,
            input_tokens: 100,
            output_tokens: 50,
            recalled_memories: vec![],
        };
        let result = eval_latency_ms(&capture, &LatencyConfig { max: 10000 });
        assert!(result.passed);
    }

    #[test]
    fn token_count_passes_under_max() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 500,
            input_tokens: 2000,
            output_tokens: 500,
            recalled_memories: vec![],
        };
        let result = eval_token_count(&capture, &TokenCountConfig { max: 5000 });
        assert!(result.passed);
    }

    #[test]
    fn token_count_fails_over_max() {
        let capture = ResponseCapture {
            response_text: "...".into(),
            tool_calls: vec![],
            latency_ms: 500,
            input_tokens: 4000,
            output_tokens: 2000,
            recalled_memories: vec![],
        };
        let result = eval_token_count(&capture, &TokenCountConfig { max: 5000 });
        assert!(!result.passed);
        assert!(result.detail.contains("6000"));
    }
}

#[cfg(test)]
mod judge_tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let (score, reasoning) =
            parse_judge_response(r#"{"score": 0.85, "reasoning": "Good response"}"#).unwrap();
        assert!((score - 0.85).abs() < f64::EPSILON);
        assert_eq!(reasoning, "Good response");
    }

    #[test]
    fn parse_json_in_code_fence() {
        let (score, _) = parse_judge_response(
            "```json\n{\"score\": 0.7, \"reasoning\": \"Partial\"}\n```",
        )
        .unwrap();
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_json_with_surrounding_text() {
        let (score, _) = parse_judge_response(
            "Here is my evaluation:\n{\"score\": 0.9, \"reasoning\": \"Great\"}\n\nDone.",
        )
        .unwrap();
        assert!((score - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_score_above_one() {
        let (score, _) =
            parse_judge_response(r#"{"score": 1.5, "reasoning": "Perfect"}"#).unwrap();
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_score_below_zero() {
        let (score, _) =
            parse_judge_response(r#"{"score": -0.2, "reasoning": "Bad"}"#).unwrap();
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fails_on_no_json() {
        let result = parse_judge_response("I think the response is pretty good");
        assert!(result.is_err());
    }

    #[test]
    fn fails_on_malformed_json() {
        let result = parse_judge_response("{score: 0.8}");
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    #[test]
    fn string_assertions_are_cheapest() {
        assert_eq!(assertion_priority(&Assertion::Contains("x".into())), 1);
        assert_eq!(assertion_priority(&Assertion::NotContains("x".into())), 1);
        assert_eq!(assertion_priority(&Assertion::Matches("x".into())), 1);
    }

    #[test]
    fn retrieval_assertions_are_second() {
        assert_eq!(assertion_priority(&Assertion::MemoryHit(true)), 2);
        assert_eq!(assertion_priority(&Assertion::ToolCalled("x".into())), 2);
    }

    #[test]
    fn operational_assertions_are_third() {
        assert_eq!(
            assertion_priority(&Assertion::LatencyMs(LatencyConfig { max: 100 })),
            3
        );
    }

    #[test]
    fn semantic_similarity_is_fourth() {
        assert_eq!(
            assertion_priority(&Assertion::SemanticSimilarity(SemanticSimilarityConfig {
                reference: "x".into(),
                threshold: None,
            })),
            4
        );
    }

    #[test]
    fn llm_judge_is_fifth() {
        assert_eq!(
            assertion_priority(&Assertion::LlmJudge(LlmJudgeConfig {
                criteria: "x".into(),
                threshold: None,
            })),
            5
        );
    }

    #[test]
    fn faithfulness_is_most_expensive() {
        assert_eq!(
            assertion_priority(&Assertion::Faithfulness(FaithfulnessConfig {
                threshold: None,
            })),
            6
        );
    }

    #[test]
    fn priorities_are_strictly_ordered() {
        assert!(
            assertion_priority(&Assertion::Contains("".into()))
                < assertion_priority(&Assertion::MemoryHit(true))
        );
        assert!(
            assertion_priority(&Assertion::MemoryHit(true))
                < assertion_priority(&Assertion::LatencyMs(LatencyConfig { max: 1 }))
        );
        assert!(
            assertion_priority(&Assertion::LatencyMs(LatencyConfig { max: 1 }))
                < assertion_priority(&Assertion::SemanticSimilarity(SemanticSimilarityConfig {
                    reference: "".into(),
                    threshold: None,
                }))
        );
        assert!(
            assertion_priority(&Assertion::SemanticSimilarity(SemanticSimilarityConfig {
                reference: "".into(),
                threshold: None,
            })) < assertion_priority(&Assertion::LlmJudge(LlmJudgeConfig {
                criteria: "".into(),
                threshold: None,
            }))
        );
        assert!(
            assertion_priority(&Assertion::LlmJudge(LlmJudgeConfig {
                criteria: "".into(),
                threshold: None,
            })) < assertion_priority(&Assertion::Faithfulness(FaithfulnessConfig {
                threshold: None,
            }))
        );
    }
}

#[cfg(test)]
mod similarity_tests {
    use super::*;

    #[test]
    fn identical_vectors_have_similarity_one() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn orthogonal_vectors_have_similarity_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-10);
    }

    #[test]
    fn opposite_vectors_have_negative_similarity() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-10);
    }

    #[test]
    fn zero_vector_returns_zero() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-10);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn minimal_test_file() -> TestFile {
        TestFile {
            config: TestConfig::default(),
            tests: vec![TestCaseDef {
                name: "valid-test".into(),
                description: None,
                question: "What is X?".into(),
                context: None,
                tags: vec![],
                sources: vec![],
                threshold: None,
                expects: vec![Assertion::Contains("answer".into())],
            }],
        }
    }

    #[test]
    fn valid_file_passes() {
        let file = minimal_test_file();
        assert!(file.validate().is_ok());
    }

    #[test]
    fn empty_tests_fails() {
        let file = TestFile {
            config: TestConfig::default(),
            tests: vec![],
        };
        assert!(file.validate().is_err());
    }

    #[test]
    fn duplicate_names_fails() {
        let test = TestCaseDef {
            name: "dupe".into(),
            description: None,
            question: "Q?".into(),
            context: None,
            tags: vec![],
            sources: vec![],
            threshold: None,
            expects: vec![Assertion::Contains("a".into())],
        };
        let file = TestFile {
            config: TestConfig::default(),
            tests: vec![test.clone(), test],
        };
        let err = file.validate().unwrap_err();
        assert!(err.iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn invalid_regex_fails() {
        let file = TestFile {
            config: TestConfig::default(),
            tests: vec![TestCaseDef {
                name: "bad-regex".into(),
                description: None,
                question: "Q?".into(),
                context: None,
                tags: vec![],
                sources: vec![],
                threshold: None,
                expects: vec![Assertion::Matches("[invalid".into())],
            }],
        };
        let err = file.validate().unwrap_err();
        assert!(err.iter().any(|e| e.contains("regex")));
    }

    #[test]
    fn threshold_out_of_range_fails() {
        let file = TestFile {
            config: TestConfig {
                threshold: 1.5,
                ..TestConfig::default()
            },
            tests: vec![TestCaseDef {
                name: "t".into(),
                description: None,
                question: "Q?".into(),
                context: None,
                tags: vec![],
                sources: vec![],
                threshold: None,
                expects: vec![Assertion::Contains("a".into())],
            }],
        };
        let err = file.validate().unwrap_err();
        assert!(err.iter().any(|e| e.contains("threshold")));
    }

    #[test]
    fn empty_expects_fails() {
        let file = TestFile {
            config: TestConfig::default(),
            tests: vec![TestCaseDef {
                name: "no-expects".into(),
                description: None,
                question: "Q?".into(),
                context: None,
                tags: vec![],
                sources: vec![],
                threshold: None,
                expects: vec![],
            }],
        };
        let err = file.validate().unwrap_err();
        assert!(err.iter().any(|e| e.contains("at least one")));
    }

    #[test]
    fn empty_contains_any_fails() {
        let file = TestFile {
            config: TestConfig::default(),
            tests: vec![TestCaseDef {
                name: "empty-any".into(),
                description: None,
                question: "Q?".into(),
                context: None,
                tags: vec![],
                sources: vec![],
                threshold: None,
                expects: vec![Assertion::ContainsAny(vec![])],
            }],
        };
        let err = file.validate().unwrap_err();
        assert!(err.iter().any(|e| e.contains("must not be empty")));
    }
}

#[cfg(test)]
mod glob_tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(glob_match("deploy-staging", "deploy-staging"));
    }

    #[test]
    fn wildcard_suffix() {
        assert!(glob_match("deploy*", "deploy-staging"));
        assert!(glob_match("deploy*", "deploy-production"));
    }

    #[test]
    fn wildcard_prefix() {
        assert!(glob_match("*staging", "deploy-staging"));
    }

    #[test]
    fn wildcard_both() {
        assert!(glob_match("*deploy*", "test-deploy-staging"));
    }

    #[test]
    fn star_matches_everything() {
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn no_match() {
        assert!(!glob_match("deploy*", "test-staging"));
    }

    #[test]
    fn case_insensitive() {
        assert!(glob_match("Deploy*", "deploy-staging"));
    }

    #[test]
    fn substring_match_without_glob() {
        assert!(glob_match("deploy", "test-deploy-staging"));
    }
}
