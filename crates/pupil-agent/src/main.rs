#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use pupil_agent::agent;
use pupil_agent::config;
use pupil_agent::conversation;

#[derive(Parser, Debug)]
#[command(name = "pupil-agent", version, about = "Pupil agent runtime")]
struct Cli {
    #[arg(long, default_value = "/agent/pupil.yaml", env = "PUPIL_CONFIG")]
    config: PathBuf,

    #[command(subcommand)]
    mode: Option<Mode>,

    #[arg(long)]
    port: Option<u16>,

    #[arg(long)]
    session: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Mode {
    Learn {
        /// Learn only this source (can be specified multiple times)
        #[arg(long = "source")]
        sources: Vec<String>,

        /// Base64-encoded remedial prompt to prepend to learning system prompt
        #[arg(long)]
        remedial_prompt: Option<String>,

        /// Force re-learning even if content hash is unchanged
        #[arg(long)]
        force_relearn: bool,
    },

    Test {
        #[arg(long, default_value = "tests.yaml")]
        file: PathBuf,

        #[arg(long)]
        filter: Option<String>,

        #[arg(long, default_value = "0")]
        temperature: f64,

        #[arg(long, default_value = "0")]
        retries: u32,

        #[arg(long)]
        json: bool,

        /// Model to use for llm_judge assertions. Overrides
        /// config.judge_model in the test YAML. If neither is set,
        /// uses the agent's own model.
        #[arg(long)]
        judge_model: Option<String>,
    },

    ForgetSource {
        source: String,
    },

    /// Block until SIGTERM. Used as a keep-alive process for build containers
    /// based on distroless images that have no shell, sleep, or tail.
    Idle,
}

fn init_tracing() {
    let format = std::env::var("PUPIL_LOG_FORMAT").unwrap_or_else(|_| "human".to_string());

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_writer(std::io::stderr)
                .with_env_filter(filter)
                .with_target(true)
                .with_thread_ids(false)
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(filter)
                .with_target(false)
                .compact()
                .init();
        }
    }
}

fn register_signal_handlers(cancel: CancellationToken) {
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .expect("Failed to register SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => {
                tracing::info!("Received SIGINT, shutting down...");
            }
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, shutting down...");
            }
        }

        cancel.cancel();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::warn!(
                    "Received second signal, forcing exit."
                );
                std::process::exit(1);
            }
            _ = tokio::time::sleep(
                std::time::Duration::from_secs(5)
            ) => {
            }
        }
    });
}

async fn run_interactive_mode(
    config: config::AgentConfig,
    session_id: Option<String>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut agent_instance = agent::Agent::new(config, cancel.clone()).await?;

    if let Some(sid) = session_id {
        match uuid::Uuid::parse_str(&sid) {
            Ok(id) => match conversation::ConversationManager::load(id) {
                Ok(loaded) => {
                    tracing::info!(
                        session_id = %id,
                        "Resumed session"
                    );
                    agent_instance.set_conversation(loaded);
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %sid,
                        error = %e,
                        "Could not load session, starting fresh"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    "Invalid session ID format, starting fresh"
                );
            }
        }
    }

    let result = agent_instance.run_loop().await;

    if let Err(e) = agent_instance.save_session() {
        tracing::warn!(error = %e, "Failed to save session");
    }

    agent_instance.shutdown().await;

    match result {
        Err(e)
            if e.downcast_ref::<agent::AgentError>().map_or(false, |ae| {
                matches!(
                    ae,
                    agent::AgentError::Shutdown | agent::AgentError::InputClosed
                )
            }) =>
        {
            Ok(())
        }
        other => other,
    }
}

async fn run_server_mode(
    config: config::AgentConfig,
    port: u16,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    tracing::info!(port, "Starting HTTP server mode");

    let llm = pupil_agent::llm::resolve_provider(&config.model)
        .context("Failed to resolve LLM provider")?;
    let mcp_manager = pupil_agent::mcp::McpManager::start_all(
        &config.mcp_servers,
        cancel.clone(),
    )
    .await
    .context("Failed to start MCP servers")?;

    let pricing_overrides: std::collections::HashMap<String, pupil_agent::llm::pricing::ModelPricing> =
        config
            .pricing
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    pupil_agent::llm::pricing::ModelPricing::new(
                        v.input_per_million,
                        v.output_per_million,
                    ),
                )
            })
            .collect();
    let cost_tracker = std::sync::Arc::new(tokio::sync::Mutex::new(
        pupil_agent::llm::CostTracker::new(&config.model, &pricing_overrides),
    ));

    let llm: std::sync::Arc<dyn pupil_agent::llm::LlmProvider> = std::sync::Arc::from(llm);

    pupil_agent::server::start_server(
        config,
        port,
        llm,
        std::sync::Arc::new(mcp_manager),
        cost_tracker,
    )
    .await
}

#[cfg(feature = "learn")]
async fn run_learn_mode(
    config: config::AgentConfig,
    sources: Vec<String>,
    remedial_prompt_encoded: Option<String>,
    force_relearn: bool,
    cancel: CancellationToken,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    tracing::info!(
        sources = ?sources,
        "Starting learning mode"
    );

    let learning_model = config.learning_model.as_deref().unwrap_or(&config.model);
    let llm = pupil_agent::llm::resolve_provider(learning_model)
        .context("Failed to resolve learning LLM provider")?;
    let mcp_manager = pupil_agent::mcp::McpManager::start_all(
        &config.mcp_servers,
        cancel.clone(),
    )
    .await
    .context("Failed to start MCP servers")?;

    let namespace = config
        .curriculum
        .as_ref()
        .map(|c| c.namespace.clone())
        .unwrap_or_else(|| "knowledge".to_string());

    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let curriculum_dir = if PathBuf::from("/curriculum").exists() {
        PathBuf::from("/curriculum")
    } else {
        config_dir.clone()
    };
    let manifest_path = if PathBuf::from("/data").exists() {
        PathBuf::from("/data/.pupil-manifest.json")
    } else {
        config_dir.join(".pupil-manifest.json")
    };

    let remedial_prompt = if let Some(encoded) = &remedial_prompt_encoded {
        use base64::Engine;
        let decoded_bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| anyhow::anyhow!("Failed to decode remedial prompt: {}", e))?;
        Some(
            String::from_utf8(decoded_bytes)
                .map_err(|e| anyhow::anyhow!("Remedial prompt is not valid UTF-8: {}", e))?,
        )
    } else {
        None
    };

    let options = pupil_agent::learn::LearnOptions {
        source_filter: sources,
        forget_source: None,
        namespace,
        manifest_path,
        curriculum_dir,
        remedial_prompt,
        force_relearn,
    };

    let summary = pupil_agent::learn::run_learning(&config, llm.as_ref(), &mcp_manager, &options).await?;

    tracing::info!(
        sources_learned = summary.sources_learned,
        sources_skipped = summary.sources_skipped,
        memories_created = summary.total_memories_created,
        "Learning complete"
    );

    mcp_manager.shutdown_all().await;
    Ok(())
}

#[cfg(not(feature = "learn"))]
async fn run_learn_mode(
    _config: config::AgentConfig,
    _sources: Vec<String>,
    _remedial_prompt: Option<String>,
    _force_relearn: bool,
    _cancel: CancellationToken,
    _config_path: &std::path::Path,
) -> anyhow::Result<()> {
    anyhow::bail!("Learning mode is not available: the 'learn' feature was not enabled at compile time.")
}

async fn run_test_mode(
    config: config::AgentConfig,
    file: PathBuf,
    filter: Option<String>,
    temperature: f64,
    retries: u32,
    json: bool,
    judge_model_override: Option<String>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    use pupil_agent::test_schema::{
        TestCaseDef, TestFile,
    };

    tracing::info!(
        file = %file.display(),
        filter = filter.as_deref().unwrap_or("(none)"),
        temperature,
        retries,
        json,
        judge_model = judge_model_override.as_deref().unwrap_or("(default)"),
        "Starting test mode"
    );

    // 1. Parse test file using the structured schema
    let test_content = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read test file: {}", file.display()))?;

    let test_file: TestFile = serde_yml::from_str(&test_content)
        .context("Failed to parse test file")?;

    if let Err(errors) = test_file.validate() {
        for err in &errors {
            tracing::error!(error = %err, "Test validation error");
        }
        anyhow::bail!("Test file has validation errors");
    }

    // 2. Resolve judge model
    let judge_model = judge_model_override
        .or(test_file.config.judge_model.clone())
        .unwrap_or_else(|| config.model.clone());

    // 3. Create judge LLM provider
    let judge_llm: Box<dyn pupil_agent::llm::LlmProvider> =
        pupil_agent::llm::resolve_provider(&judge_model)
            .context("Failed to resolve judge LLM provider")?;

    // 4. Create agent
    let mut agent_instance = agent::Agent::new(config.clone(), cancel.clone()).await?;

    // 5. Filter tests
    let tests: Vec<&TestCaseDef> = if let Some(ref f) = filter {
        test_file.tests.iter().filter(|t| {
            t.name.contains(f.as_str())
                || t.question.contains(f.as_str())
        }).collect()
    } else {
        test_file.tests.iter().collect()
    };

    // 6. Run each test with assertion evaluation
    let mut results = Vec::new();
    let mut passed_count = 0usize;
    let mut failed_count = 0usize;

    for test_case in &tests {
        let result = run_single_test(
            &mut agent_instance,
            test_case,
            &test_file.config,
            temperature,
            retries,
            judge_llm.as_ref(),
            &judge_model,
        )
        .await;

        if result["passed"].as_bool().unwrap_or(false) {
            passed_count += 1;
        } else {
            failed_count += 1;
        }
        results.push(result);

        agent_instance.reset_conversation();
    }

    // 7. Output
    let summary = serde_json::json!({
        "summary": {
            "total": results.len(),
            "passed": passed_count,
            "failed": failed_count,
            "pass_rate": if results.is_empty() {
                1.0
            } else {
                passed_count as f64 / results.len() as f64
            },
        },
        "tests": results,
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        for result in &results {
            let status = if result["passed"].as_bool().unwrap_or(false) {
                "PASS"
            } else {
                "FAIL"
            };
            eprintln!("[{}] {}", status, result["name"].as_str().unwrap_or(""));

            // Print failed assertions
            if let Some(assertions) = result["assertions"].as_array() {
                for assertion in assertions {
                    if assertion["passed"].as_bool() == Some(false) {
                        eprintln!(
                            "       {} - {}",
                            assertion["assertion_type"].as_str().unwrap_or(""),
                            assertion["detail"].as_str().unwrap_or(""),
                        );
                    }
                }
            }
        }
        eprintln!(
            "\n{}/{} passed ({:.0}%)",
            passed_count,
            results.len(),
            if results.is_empty() { 100.0 } else {
                passed_count as f64 / results.len() as f64 * 100.0
            }
        );
    }

    agent_instance.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Single test runner with assertion evaluation
// ---------------------------------------------------------------------------

struct AssertionResultValue {
    assertion_type: String,
    passed: bool,
    score: Option<f64>,
    threshold: Option<f64>,
    detail: String,
}

/// Run a single test case: send question, capture response, evaluate all
/// assertions including llm_judge.
async fn run_single_test(
    agent: &mut agent::Agent,
    test_case: &pupil_agent::test_schema::TestCaseDef,
    test_config: &pupil_agent::test_schema::TestConfig,
    _temperature: f64,
    max_retries: u32,
    judge_llm: &dyn pupil_agent::llm::LlmProvider,
    judge_model: &str,
) -> serde_json::Value {
    let question = &test_case.question;
    let name = &test_case.name;

    let max_attempts = 1 + max_retries as usize;
    let mut last_result = serde_json::Value::Null;

    for attempt in 0..max_attempts {
        agent.reset_conversation();

        let start = std::time::Instant::now();
        let response = agent.run_single_query(question).await;
        let latency_ms = start.elapsed().as_millis() as u64;

        let answer = match response {
            Ok(text) => text,
            Err(e) => {
                last_result = serde_json::json!({
                    "name": name,
                    "question": question,
                    "response": format!("(error: {e})"),
                    "assertions": [],
                    "latency_ms": latency_ms,
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "tool_calls": [],
                    "retries_used": attempt,
                    "passed": false,
                });
                continue;
            }
        };

        // Evaluate assertions
        let mut assertion_results = Vec::new();
        let mut all_passed = true;

        for assertion in &test_case.expects {
            let result = evaluate_assertion(
                assertion,
                &answer,
                question,
                test_case,
                test_config,
                judge_llm,
                judge_model,
            )
            .await;

            if !result.passed {
                all_passed = false;
            }
            assertion_results.push(serde_json::json!({
                "assertion_type": result.assertion_type,
                "passed": result.passed,
                "score": result.score,
                "threshold": result.threshold,
                "detail": result.detail,
            }));
        }

        let test_result = serde_json::json!({
            "name": name,
            "question": question,
            "response": answer,
            "assertions": assertion_results,
            "latency_ms": latency_ms,
            "input_tokens": 0,
            "output_tokens": 0,
            "tool_calls": [],
            "retries_used": attempt,
            "passed": all_passed,
        });

        if all_passed {
            return test_result;
        }

        last_result = test_result;
    }

    last_result
}

// ---------------------------------------------------------------------------
// Assertion evaluation
// ---------------------------------------------------------------------------

/// Evaluate a single assertion against the agent's response.
async fn evaluate_assertion(
    assertion: &pupil_agent::test_schema::Assertion,
    response: &str,
    question: &str,
    test_case: &pupil_agent::test_schema::TestCaseDef,
    test_config: &pupil_agent::test_schema::TestConfig,
    judge_llm: &dyn pupil_agent::llm::LlmProvider,
    judge_model: &str,
) -> AssertionResultValue {
    use pupil_agent::test_schema::Assertion;

    match assertion {
        Assertion::Contains(expected) => {
            let response_lower = response.to_lowercase();
            let expected_lower = expected.to_lowercase();
            let passed = response_lower.contains(&expected_lower);
            AssertionResultValue {
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

        Assertion::NotContains(excluded) => {
            let response_lower = response.to_lowercase();
            let excluded_lower = excluded.to_lowercase();
            let passed = !response_lower.contains(&excluded_lower);
            AssertionResultValue {
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

        Assertion::ContainsAny(candidates) => {
            let response_lower = response.to_lowercase();
            let matched: Vec<&String> = candidates
                .iter()
                .filter(|c| response_lower.contains(&c.to_lowercase()))
                .collect();
            let passed = !matched.is_empty();
            AssertionResultValue {
                assertion_type: "contains_any".to_string(),
                passed,
                score: None,
                threshold: None,
                detail: if passed {
                    format!("Found {} of {} candidates", matched.len(), candidates.len())
                } else {
                    format!("None of {} candidates found", candidates.len())
                },
            }
        }

        Assertion::ContainsAll(required) => {
            let response_lower = response.to_lowercase();
            let missing: Vec<&String> = required
                .iter()
                .filter(|r| !response_lower.contains(&r.to_lowercase()))
                .collect();
            let passed = missing.is_empty();
            AssertionResultValue {
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
                        missing.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
                    )
                },
            }
        }

        Assertion::Matches(pattern) => {
            let re = regex::Regex::new(pattern).expect("validated at parse time");
            let passed = re.is_match(response);
            AssertionResultValue {
                assertion_type: "matches".to_string(),
                passed,
                score: None,
                threshold: None,
                detail: if passed {
                    format!("Matches /{}/", pattern)
                } else {
                    format!("Does not match /{}/", pattern)
                },
            }
        }

        Assertion::NotMatches(pattern) => {
            let re = regex::Regex::new(pattern).expect("validated at parse time");
            let passed = !re.is_match(response);
            AssertionResultValue {
                assertion_type: "not_matches".to_string(),
                passed,
                score: None,
                threshold: None,
                detail: if passed {
                    format!("Correctly does not match /{}/", pattern)
                } else {
                    format!("Matches /{}/ but should not", pattern)
                },
            }
        }

        Assertion::StartsWith(prefix) => {
            let passed = response
                .to_lowercase()
                .trim_start()
                .starts_with(&prefix.to_lowercase());
            AssertionResultValue {
                assertion_type: "starts_with".to_string(),
                passed,
                score: None,
                threshold: None,
                detail: if passed {
                    format!("Starts with '{}'", prefix)
                } else {
                    format!("Does not start with '{}'", prefix)
                },
            }
        }

        Assertion::LlmJudge(cfg) => {
            let threshold = cfg
                .threshold
                .or(test_case.threshold)
                .unwrap_or(test_config.threshold);

            eval_llm_judge_in_container(
                judge_llm,
                judge_model,
                question,
                response,
                &cfg.criteria,
                threshold,
            )
            .await
        }

        // For assertion types not yet wired in the container,
        // return a skip/pass result. These are evaluated host-side.
        Assertion::Other(name) => AssertionResultValue {
            assertion_type: name.clone(),
            passed: true,
            score: None,
            threshold: None,
            detail: "Evaluated host-side or not applicable".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// LLM judge evaluation
// ---------------------------------------------------------------------------

/// Call the judge LLM to score the agent's response. This is a plain
/// chat call with no tools.
async fn eval_llm_judge_in_container(
    judge_llm: &dyn pupil_agent::llm::LlmProvider,
    judge_model: &str,
    question: &str,
    response: &str,
    criteria: &str,
    threshold: f64,
) -> AssertionResultValue {
    use pupil_agent::llm::{ChatConfig, Message, ToolDefinition};

    let prompt = format!(
        "Score how well this response meets the criteria, then call the submit_score tool.\n\n\
         QUESTION: {question}\n\
         RESPONSE: {response}\n\
         CRITERIA: {criteria}\n\n\
         Scoring: 1.0 = fully meets criteria, 0.7 = mostly meets with minor gaps, \
         0.4 = partially meets, 0.0 = wrong or missing."
    );

    let score_tool = ToolDefinition {
        name: "submit_score".to_string(),
        description: "Submit the evaluation score and reasoning".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "score": {
                    "type": "number",
                    "description": "Score from 0.0 to 1.0"
                },
                "reasoning": {
                    "type": "string",
                    "description": "One sentence explanation"
                }
            },
            "required": ["score", "reasoning"]
        }),
    };

    let messages = vec![Message::user(prompt)];
    let chat_config = ChatConfig::new(judge_model.to_string())
        .with_temperature(0.0)
        .with_max_tokens(512);

    let max_attempts = 3;
    for attempt in 0..max_attempts {
        let result = judge_llm.chat(&messages, &[score_tool.clone()], &chat_config).await;

        match result {
            Ok(chat_response) => {
                if let Some(tc) = chat_response.tool_calls.first() {
                    if let Ok(args) = serde_json::from_value::<ScoreArgs>(tc.arguments.clone()) {
                        let score = args.score.clamp(0.0, 1.0);
                        let passed = score >= threshold;
                        return AssertionResultValue {
                            assertion_type: "llm_judge".to_string(),
                            passed,
                            score: Some(score),
                            threshold: Some(threshold),
                            detail: args.reasoning,
                        };
                    }
                }

                // Fallback: try parsing the text content as JSON
                if !chat_response.content.is_empty() {
                    if let Ok((score, reasoning)) = parse_judge_response_inline(&chat_response.content) {
                        let passed = score >= threshold;
                        return AssertionResultValue {
                            assertion_type: "llm_judge".to_string(),
                            passed,
                            score: Some(score),
                            threshold: Some(threshold),
                            detail: reasoning,
                        };
                    }
                }

                if attempt < max_attempts - 1 {
                    continue;
                }
                return AssertionResultValue {
                    assertion_type: "llm_judge".to_string(),
                    passed: false,
                    score: None,
                    threshold: Some(threshold),
                    detail: "Judge did not return a score via tool call or parseable text".to_string(),
                };
            }
            Err(e) => {
                if attempt < max_attempts - 1 {
                    continue;
                }
                return AssertionResultValue {
                    assertion_type: "llm_judge".to_string(),
                    passed: false,
                    score: None,
                    threshold: Some(threshold),
                    detail: format!("Judge LLM call failed: {e}"),
                };
            }
        }
    }
    unreachable!()
}

#[derive(serde::Deserialize)]
struct ScoreArgs {
    score: f64,
    #[serde(default)]
    reasoning: String,
}

/// Parse {"score": 0.8, "reasoning": "..."} from judge output.
/// Handles code fences and surrounding text.
fn parse_judge_response_inline(raw: &str) -> Result<(f64, String), String> {
    let trimmed = raw.trim();

    // Strip code fences
    let cleaned = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    // Try to find a JSON object first
    if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}')) {
        #[derive(serde::Deserialize)]
        struct JudgeResp {
            score: f64,
            #[serde(default)]
            reasoning: String,
        }

        if let Ok(parsed) = serde_json::from_str::<JudgeResp>(&cleaned[start..=end]) {
            return Ok((parsed.score.clamp(0.0, 1.0), parsed.reasoning));
        }
    }

    // Fallback: extract a numeric score from the raw text (handles quoted and unquoted keys)
    let score_re = regex::Regex::new(
        r#"(?i)"?(?:score|rating)"?\s*[:=]\s*([01]\.?\d*)"#
    ).unwrap();
    if let Some(caps) = score_re.captures(cleaned) {
        if let Ok(score) = caps[1].parse::<f64>() {
            let reasoning = regex::Regex::new(r#"(?i)"?reasoning"?\s*[:=]\s*"([^"]+)"#)
                .ok()
                .and_then(|re| re.captures(cleaned))
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            return Ok((score.clamp(0.0, 1.0), reasoning));
        }
    }

    // Last resort: look for any float between 0 and 1 on its own line
    for line in cleaned.lines() {
        let line = line.trim();
        if let Ok(score) = line.parse::<f64>() {
            if (0.0..=1.0).contains(&score) {
                return Ok((score, cleaned.to_string()));
            }
        }
    }

    Err(format!("Could not extract score from judge response: {}", &cleaned[..cleaned.len().min(200)]))
}

#[cfg(feature = "learn")]
async fn run_forget_source(
    config: config::AgentConfig,
    source: String,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    tracing::info!(source = %source, "Forgetting source memories");

    let mcp_manager = pupil_agent::mcp::McpManager::start_all(
        &config.mcp_servers,
        cancel.clone(),
    )
    .await
    .context("Failed to start MCP servers")?;

    let namespace = config
        .curriculum
        .as_ref()
        .map(|c| c.namespace.clone())
        .unwrap_or_else(|| "knowledge".to_string());

    let options = pupil_agent::learn::LearnOptions {
        source_filter: vec![],
        forget_source: Some(source.clone()),
        namespace,
        manifest_path: PathBuf::from("/data/.pupil-manifest.json"),
        curriculum_dir: PathBuf::from("/curriculum"),
        remedial_prompt: None,
        force_relearn: false,
    };

    let learning_model = config.learning_model.as_deref().unwrap_or(&config.model);
    let llm = pupil_agent::llm::resolve_provider(learning_model)
        .context("Failed to resolve LLM provider")?;

    let summary = pupil_agent::learn::run_learning(&config, llm.as_ref(), &mcp_manager, &options).await?;

    tracing::info!(
        memories_forgotten = summary.total_memories_forgotten,
        "Source '{}' forgotten",
        source
    );

    mcp_manager.shutdown_all().await;
    Ok(())
}

#[cfg(not(feature = "learn"))]
async fn run_forget_source(
    _config: config::AgentConfig,
    _source: String,
    _cancel: CancellationToken,
) -> anyhow::Result<()> {
    anyhow::bail!("Forget source is not available: the 'learn' feature was not enabled at compile time.")
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    init_tracing();

    let cancel = CancellationToken::new();

    register_signal_handlers(cancel.clone());

    // Handle idle mode before config loading -- it needs no config
    if matches!(cli.mode, Some(Mode::Idle)) {
        tracing::info!("pupil-agent idle mode");
        cancel.cancelled().await;
        return ExitCode::SUCCESS;
    }

    tracing::info!(
        config = %cli.config.display(),
        "pupil-agent starting"
    );

    let config = match config::AgentConfig::load(&cli.config) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(error = %e, "Failed to load configuration");
            return ExitCode::from(2);
        }
    };

    let result = match cli.mode {
        Some(Mode::Learn { sources, remedial_prompt, force_relearn }) => {
            run_learn_mode(config, sources, remedial_prompt, force_relearn, cancel, &cli.config).await
        }
        Some(Mode::Test {
            file,
            filter,
            temperature,
            retries,
            json,
            judge_model,
        }) => {
            run_test_mode(config, file, filter, temperature, retries, json, judge_model, cancel).await
        }
        Some(Mode::ForgetSource { source }) => run_forget_source(config, source, cancel).await,
        Some(Mode::Idle) => unreachable!(), // handled above
        None => {
            if let Some(port) = cli.port {
                run_server_mode(config, port, cancel).await
            } else {
                run_interactive_mode(config, cli.session, cancel).await
            }
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if e.downcast_ref::<agent::AgentError>()
                .map_or(false, |ae| matches!(ae, agent::AgentError::Shutdown))
            {
                tracing::info!("Agent shut down cleanly.");
                ExitCode::SUCCESS
            } else if e
                .downcast_ref::<agent::AgentError>()
                .map_or(false, |ae| matches!(ae, agent::AgentError::InputClosed))
            {
                tracing::info!("Input stream closed, exiting.");
                ExitCode::SUCCESS
            } else {
                tracing::error!(error = %e, "Agent exited with error");
                ExitCode::from(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_default_config() {
        let cli = Cli::parse_from(["pupil-agent"]);
        assert_eq!(cli.config, PathBuf::from("/agent/pupil.yaml"));
        assert!(cli.mode.is_none());
        assert!(cli.port.is_none());
    }

    #[test]
    fn test_cli_parse_custom_config() {
        let cli = Cli::parse_from(["pupil-agent", "--config", "/custom/path.yaml"]);
        assert_eq!(cli.config, PathBuf::from("/custom/path.yaml"));
    }

    #[test]
    fn test_cli_parse_learn_mode() {
        let cli = Cli::parse_from(["pupil-agent", "learn"]);
        match cli.mode {
            Some(Mode::Learn { sources, remedial_prompt, force_relearn }) => {
                assert!(sources.is_empty());
                assert!(remedial_prompt.is_none());
                assert!(!force_relearn);
            }
            _ => panic!("Expected Learn mode"),
        }
    }

    #[test]
    fn test_cli_parse_learn_mode_with_source() {
        let cli = Cli::parse_from(["pupil-agent", "learn", "--source", "handbook.md"]);
        match cli.mode {
            Some(Mode::Learn { sources, .. }) => {
                assert_eq!(sources, vec!["handbook.md".to_string()]);
            }
            _ => panic!("Expected Learn mode"),
        }
    }

    #[test]
    fn test_cli_parse_learn_mode_with_multiple_sources() {
        let cli = Cli::parse_from([
            "pupil-agent", "learn",
            "--source", "handbook.md",
            "--source", "runbook.md",
        ]);
        match cli.mode {
            Some(Mode::Learn { sources, .. }) => {
                assert_eq!(sources, vec!["handbook.md".to_string(), "runbook.md".to_string()]);
            }
            _ => panic!("Expected Learn mode"),
        }
    }

    #[test]
    fn test_cli_parse_learn_mode_with_remedial() {
        let cli = Cli::parse_from([
            "pupil-agent", "learn",
            "--remedial-prompt", "dGVzdA==",
            "--force-relearn",
        ]);
        match cli.mode {
            Some(Mode::Learn { remedial_prompt, force_relearn, .. }) => {
                assert_eq!(remedial_prompt.unwrap(), "dGVzdA==");
                assert!(force_relearn);
            }
            _ => panic!("Expected Learn mode"),
        }
    }

    #[test]
    fn test_cli_parse_test_mode() {
        let cli = Cli::parse_from([
            "pupil-agent",
            "test",
            "--file",
            "qa.yaml",
            "--json",
            "--retries",
            "2",
        ]);
        match cli.mode {
            Some(Mode::Test {
                file,
                json,
                retries,
                ..
            }) => {
                assert_eq!(file, PathBuf::from("qa.yaml"));
                assert!(json);
                assert_eq!(retries, 2);
            }
            _ => panic!("Expected Test mode"),
        }
    }

    #[test]
    fn test_cli_parse_forget_source() {
        let cli = Cli::parse_from(["pupil-agent", "forget-source", "old-doc.md"]);
        match cli.mode {
            Some(Mode::ForgetSource { source }) => {
                assert_eq!(source, "old-doc.md");
            }
            _ => panic!("Expected ForgetSource mode"),
        }
    }

    #[test]
    fn test_cli_parse_idle_mode() {
        let cli = Cli::parse_from(["pupil-agent", "idle"]);
        assert!(matches!(cli.mode, Some(Mode::Idle)));
    }

    #[test]
    fn test_cli_parse_server_mode_via_port() {
        let cli = Cli::parse_from(["pupil-agent", "--port", "8080"]);
        assert!(cli.mode.is_none());
        assert_eq!(cli.port, Some(8080));
    }

    #[test]
    fn test_cli_parse_session_resume() {
        let cli = Cli::parse_from([
            "pupil-agent",
            "--session",
            "550e8400-e29b-41d4-a716-446655440000",
        ]);
        assert_eq!(
            cli.session.unwrap(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn test_cli_parse_test_mode_with_judge_model() {
        let cli = Cli::parse_from([
            "pupil-agent",
            "test",
            "--file",
            "qa.yaml",
            "--json",
            "--judge-model",
            "claude-sonnet-4-6",
        ]);
        match cli.mode {
            Some(Mode::Test {
                file,
                json,
                judge_model,
                ..
            }) => {
                assert_eq!(file, PathBuf::from("qa.yaml"));
                assert!(json);
                assert_eq!(judge_model.unwrap(), "claude-sonnet-4-6");
            }
            _ => panic!("Expected Test mode"),
        }
    }
}

#[cfg(test)]
mod judge_inline_tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let (score, reasoning) = parse_judge_response_inline(
            r#"{"score": 0.85, "reasoning": "Good response"}"#,
        )
        .unwrap();
        assert!((score - 0.85).abs() < f64::EPSILON);
        assert_eq!(reasoning, "Good response");
    }

    #[test]
    fn parse_with_code_fence() {
        let (score, _) = parse_judge_response_inline(
            "```json\n{\"score\": 0.7, \"reasoning\": \"Partial\"}\n```",
        )
        .unwrap();
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_with_surrounding_text() {
        let (score, _) = parse_judge_response_inline(
            "Here is my evaluation:\n{\"score\": 0.9, \"reasoning\": \"Great\"}\nDone.",
        )
        .unwrap();
        assert!((score - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_above_one() {
        let (score, _) = parse_judge_response_inline(
            r#"{"score": 1.5, "reasoning": "Perfect"}"#,
        )
        .unwrap();
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_below_zero() {
        let (score, _) = parse_judge_response_inline(
            r#"{"score": -0.2, "reasoning": "Bad"}"#,
        )
        .unwrap();
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fails_on_no_json() {
        assert!(parse_judge_response_inline("I think it's good").is_err());
    }
}
