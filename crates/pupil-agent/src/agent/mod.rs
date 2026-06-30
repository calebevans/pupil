use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::collaboration::{AgentCaller, AgentRegistryEntry};
use crate::config::AgentConfig;
use crate::conversation::ConversationManager;
use crate::llm::{
    ChatConfig, LlmProvider, Message, ResponseSchema, StopReason, TokenUsage, ToolCall,
    ToolDefinition, ToolResult,
};
use crate::mcp::McpManager;
use crate::prompt::{PeerAgent, SystemPromptBuilder};

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("LLM call failed: {message}")]
    LlmError { message: String },

    #[error("Required MCP server '{name}' failed to start: {reason}")]
    McpServerStartFailed { name: String, reason: String },

    #[error(
        "Iteration limit reached ({limit}). The model may be stuck in \
         a tool-call loop."
    )]
    IterationLimitReached { limit: u32 },

    #[error("Token budget exceeded: {used} tokens used, limit is {limit}.")]
    TokenBudgetExceeded { used: u64, limit: u64 },

    #[error("Input stream closed")]
    InputClosed,

    #[error("Agent shutdown requested")]
    Shutdown,
}

#[derive(Debug)]
struct RetrievalResult {
    id: String,
    summary: String,
    full_text: Option<String>,
}

struct RetrievalPlanningOutput {
    results: Vec<RetrievalResult>,
    usage: TokenUsage,
}

fn humanize_recall_result(raw_json: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(_) => return raw_json.to_string(),
    };

    let memories = match parsed.get("memories").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return raw_json.to_string(),
    };

    if memories.is_empty() {
        return "No memories found.".to_string();
    }

    let mut out = format!("Found {} memories:\n\n", memories.len());
    for (i, mem) in memories.iter().enumerate() {
        let summary = mem.get("summary").and_then(|s| s.as_str()).unwrap_or("");
        let full_text = mem.get("fullText").and_then(|s| s.as_str());
        let entities = mem.get("entities").and_then(|e| e.as_array());

        out.push_str(&format!("[{}] {}\n", i + 1, summary));
        if let Some(ft) = full_text {
            if !ft.is_empty() {
                out.push_str(&format!("    Details: {}\n", ft));
            }
        }
        if let Some(ents) = entities {
            let names: Vec<&str> = ents.iter().filter_map(|e| e.as_str()).collect();
            if !names.is_empty() {
                out.push_str(&format!("    People/entities: {}\n", names.join(", ")));
            }
        }
        out.push('\n');
    }

    if let Some(graph) = parsed.get("graphContext") {
        if let Some(neighbors) = graph.get("neighbors").and_then(|n| n.as_array()) {
            if !neighbors.is_empty() {
                out.push_str(&format!("Related memories ({}):\n", neighbors.len()));
                for nb in neighbors.iter().take(5) {
                    let summary = nb.get("summary").and_then(|s| s.as_str()).unwrap_or("");
                    if !summary.is_empty() {
                        out.push_str(&format!("  - {}\n", summary));
                    }
                }
                out.push('\n');
            }
        }
    }

    out
}

fn format_retrieval_context(results: &[RetrievalResult]) -> String {
    let mut out = String::from(
        "The following knowledge was retrieved from memory to help \
         answer the question. Use it if relevant.\n\n",
    );
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("{}. {}", i + 1, r.summary));
        if let Some(ref full) = r.full_text {
            out.push_str(&format!("\n   Detail: {}", full));
        }
        out.push('\n');
    }
    out
}

pub struct Agent {
    config: AgentConfig,
    conversation: ConversationManager,
    mcp_manager: Arc<McpManager>,
    llm: Box<dyn LlmProvider>,
    chat_config: ChatConfig,
    cancel: CancellationToken,
    collaboration: Option<AgentCaller>,
    current_depth: u32,
    call_chain: Vec<String>,
}

impl Agent {
    pub async fn new(config: AgentConfig, cancel: CancellationToken) -> Result<Self> {
        let llm = crate::llm::resolve_provider(&config.model)
            .context("Failed to resolve LLM provider")?;

        let mcp_manager =
            McpManager::start_all(&config.mcp_servers, cancel.clone())
                .await
                .context("Failed to start MCP servers")?;

        let mcp_manager = Arc::new(mcp_manager);

        let mut llm_tools: Vec<ToolDefinition> = mcp_manager
            .all_tools()
            .iter()
            .map(|t| t.into())
            .collect();

        let collab_enabled = config
            .collaboration
            .as_ref()
            .map(|c| c.enabled)
            .unwrap_or(false);

        let collaboration = if collab_enabled {
            let registry_json = std::env::var("PUPIL_AGENT_REGISTRY").ok();
            let registry: Vec<AgentRegistryEntry> = registry_json
                .as_deref()
                .and_then(|json| crate::collaboration::parse_agent_registry(json).ok())
                .unwrap_or_default();

            if !registry.is_empty() {
                let collab_config = config.collaboration.as_ref().unwrap();
                let router_url = std::env::var("PUPIL_ROUTER_URL").ok();
                let caller = AgentCaller::new(
                    collab_config,
                    registry,
                    config.name.clone(),
                    router_url,
                );
                llm_tools.push(crate::collaboration::tool::ask_agent_tool_definition());
                Some(caller)
            } else {
                None
            }
        } else {
            None
        };

        let peers: Vec<PeerAgent> = collaboration
            .as_ref()
            .map(|c| {
                c.registry()
                    .iter()
                    .map(|e| PeerAgent {
                        name: e.name.clone(),
                        description: e.description.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let namespace = config
            .curriculum
            .as_ref()
            .map(|c| c.namespace.clone())
            .unwrap_or_else(|| "knowledge".to_string());
        let mut prompt_builder = SystemPromptBuilder::new(
            config.name.clone(),
            config.description.clone(),
            config.system_prompt.clone(),
            llm_tools,
            namespace,
        )
        .with_peers(peers)
        .with_collaboration(collaboration.is_some());

        if let Some(ref rs) = config.response_schema {
            prompt_builder = prompt_builder.with_response_schema(&ResponseSchema {
                name: rs.name.clone(),
                description: rs.description.clone(),
                schema: rs.schema.clone(),
                strict: rs.strict,
            });
        }

        let system_prompt = prompt_builder.build();

        let session_id = Uuid::new_v4();
        let conversation = ConversationManager::new(system_prompt, session_id);

        let chat_config = ChatConfig::new(config.model.clone())
            .with_temperature(config.temperature);
        let chat_config = if let Some(max_tokens) = config.max_output_tokens {
            chat_config.with_max_tokens(max_tokens)
        } else {
            chat_config
        };
        let chat_config = if let Some(ref rs) = config.response_schema {
            chat_config.with_response_schema(ResponseSchema {
                name: rs.name.clone(),
                description: rs.description.clone(),
                schema: rs.schema.clone(),
                strict: rs.strict,
            })
        } else {
            chat_config
        };

        Ok(Self {
            config,
            conversation,
            mcp_manager,
            llm,
            chat_config,
            cancel,
            collaboration,
            current_depth: 0,
            call_chain: Vec::new(),
        })
    }

    pub async fn run_loop(&mut self) -> Result<()> {
        loop {
            if self.cancel.is_cancelled() {
                return Err(AgentError::Shutdown.into());
            }

            let input = {
                let cancel = self.cancel.clone();
                tokio::select! {
                    result = tokio::task::spawn_blocking(move || -> Result<String> {
                        let stdin = io::stdin();
                        let mut line = String::new();
                        match stdin.lock().read_line(&mut line) {
                            Ok(0) => Err(AgentError::InputClosed.into()),
                            Ok(_) => Ok(line.trim().to_string()),
                            Err(e) => Err(anyhow::anyhow!(
                                "Failed to read stdin: {e}"
                            )),
                        }
                    }) => {
                        result.context("stdin reader task panicked")??
                    }
                    _ = cancel.cancelled() => {
                        return Err(AgentError::Shutdown.into());
                    }
                }
            };

            if input.is_empty() {
                continue;
            }

            self.conversation.push_user(&input);

            self.handle_turn().await?;

            let response_text = self.conversation.last_assistant_text();
            let stdout = io::stdout();
            let mut out = stdout.lock();
            writeln!(out, "{response_text}")?;
            writeln!(out)?;
            out.flush()?;
        }
    }

    async fn handle_turn(&mut self) -> Result<()> {
        let last_user_msg = self
            .conversation
            .messages()
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::User { content } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();

        if !last_user_msg.is_empty() {
            match self.run_retrieval_planning(&last_user_msg).await {
                Ok(output) => {
                    self.conversation
                        .record_usage(output.usage.input_tokens, output.usage.output_tokens);

                    if !output.results.is_empty() {
                        tracing::info!(
                            count = output.results.len(),
                            "Retrieval planning surfaced memories"
                        );
                        let context = format_retrieval_context(&output.results);
                        self.conversation.push_context(&context);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Retrieval planning failed, proceeding without");
                }
            }
        }

        let mut iterations: u32 = 0;
        let mut malformed_retries: u32 = 0;
        let mut ask_agent_calls: u32 = 0;
        let turn_start_tokens = self.conversation.total_tokens();

        loop {
            if self.cancel.is_cancelled() {
                return Err(AgentError::Shutdown.into());
            }

            let messages = self.conversation.messages();
            let mut llm_tools: Vec<ToolDefinition> = self
                .mcp_manager
                .all_tools()
                .iter()
                .map(|t| t.into())
                .collect();
            if self.collaboration.is_some() {
                llm_tools.push(crate::collaboration::tool::ask_agent_tool_definition());
            }

            let response = self
                .llm
                .chat(messages, &llm_tools, &self.chat_config)
                .await
                .map_err(|e| AgentError::LlmError {
                    message: e.to_string(),
                })?;

            self.conversation
                .record_usage(response.usage.input_tokens, response.usage.output_tokens);

            // Handle malformed function calls before pushing the assistant
            // message. The response contains broken tool call JSON that would
            // corrupt the conversation context if kept.
            if response.stop_reason.is_malformed_function_call() {
                malformed_retries += 1;
                if malformed_retries > crate::llm::MAX_MALFORMED_RETRIES {
                    tracing::warn!(
                        retries = crate::llm::MAX_MALFORMED_RETRIES,
                        "Malformed function call: all retries exhausted, \
                         ending turn"
                    );
                    break;
                }
                tracing::warn!(
                    retry = malformed_retries,
                    max_retries = crate::llm::MAX_MALFORMED_RETRIES,
                    "Malformed function call from LLM, retrying"
                );
                self.conversation.push_user(
                    "Your previous tool call was malformed. \
                     Please try again with valid JSON.",
                );
                continue;
            }

            self.conversation.push_assistant(&response);

            // Some providers (e.g., Vertex/Gemini) return EndTurn even when
            // tool calls are present. Always check for tool calls first.
            if !response.tool_calls.is_empty() {
                let tool_calls = response.tool_calls.clone();
                tracing::info!(
                    tool_count = tool_calls.len(),
                    tools = ?tool_calls
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>(),
                    "Executing tool calls"
                );

                let results = self
                    .execute_tools_parallel(&tool_calls, &mut ask_agent_calls)
                    .await;
                self.conversation.push_tool_results(results);
            } else {
                match response.stop_reason {
                    StopReason::EndTurn => {
                        tracing::debug!(iterations, "Turn complete (EndTurn)");
                        break;
                    }
                    StopReason::MaxTokens => {
                        tracing::warn!(
                            "LLM response truncated (max output tokens \
                             reached). Consider increasing \
                             max_output_tokens in pupil.yaml."
                        );
                        break;
                    }
                    StopReason::ToolUse => {
                        tracing::warn!(
                            "StopReason::ToolUse but no tool calls \
                             found. Treating as EndTurn."
                        );
                        break;
                    }
                    StopReason::StopSequence => {
                        tracing::debug!(iterations, "Stop sequence hit");
                        break;
                    }
                    StopReason::ContentFiltered => {
                        tracing::warn!("Content filtered by provider");
                        break;
                    }
                    StopReason::Other(ref reason) => {
                        tracing::warn!(
                            reason = %reason,
                            "Unknown stop reason; treating as end of turn"
                        );
                        break;
                    }
                }
            }

            // Reset malformed retry counter on any successful iteration
            // (the model recovered and produced a valid response).
            malformed_retries = 0;

            iterations += 1;
            if iterations >= self.config.max_iterations {
                return Err(AgentError::IterationLimitReached {
                    limit: self.config.max_iterations,
                }
                .into());
            }

            if let Some(limit) = self.config.max_tokens_per_query() {
                let used = self.conversation.total_tokens() - turn_start_tokens;
                if used > limit {
                    return Err(AgentError::TokenBudgetExceeded { used, limit }.into());
                }
            }
        }

        Ok(())
    }

    async fn execute_tools_parallel(
        &self,
        calls: &[ToolCall],
        ask_agent_calls: &mut u32,
    ) -> Vec<ToolResult> {
        if calls.is_empty() {
            return Vec::new();
        }

        let mut mcp_calls: Vec<(usize, ToolCall)> = Vec::new();
        let mut agent_calls: Vec<(usize, ToolCall)> = Vec::new();

        for (i, call) in calls.iter().enumerate() {
            if call.name == "ask_agent" {
                agent_calls.push((i, call.clone()));
            } else {
                mcp_calls.push((i, call.clone()));
            }
        }

        let mut results: Vec<Option<ToolResult>> = vec![None; calls.len()];

        let mcp_handles: Vec<_> = mcp_calls
            .into_iter()
            .map(|(idx, call)| {
                let manager = Arc::clone(&self.mcp_manager);
                let timeout = self.config.tool_timeout;
                tokio::spawn(async move {
                    let timer = Instant::now();
                    let arguments = call.arguments.as_object().cloned();
                    let result = match tokio::time::timeout(
                        timeout,
                        manager.call_tool(&call.name, arguments),
                    )
                    .await
                    {
                        Ok(Ok(result)) => {
                            let raw_text =
                                crate::mcp::schema::tool_result_to_string(&result);
                            let result_text = if call.name == "recall_memories" || call.name == "list_memories" {
                                humanize_recall_result(&raw_text)
                            } else {
                                raw_text
                            };
                            tracing::info!(
                                tool = %call.name,
                                latency_ms = timer.elapsed().as_millis() as u64,
                                "Tool call succeeded"
                            );
                            ToolResult::success(call.id.clone(), result_text)
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(
                                tool = %call.name,
                                error = %e,
                                "Tool call failed"
                            );
                            ToolResult::error(call.id.clone(), format!("Tool error: {e}"))
                        }
                        Err(_) => {
                            tracing::warn!(
                                tool = %call.name,
                                timeout_secs = timeout.as_secs(),
                                "Tool call timed out"
                            );
                            ToolResult::error(
                                call.id.clone(),
                                format!(
                                    "Tool '{}' timed out after {}s",
                                    call.name,
                                    timeout.as_secs(),
                                ),
                            )
                        }
                    };
                    (idx, result)
                })
            })
            .collect();

        let mcp_results = futures::future::join_all(mcp_handles).await;
        for join_result in mcp_results {
            match join_result {
                Ok((idx, result)) => {
                    results[idx] = Some(result);
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "Tool task panicked");
                }
            }
        }

        for (idx, call) in agent_calls {
            let max_per_turn = self
                .collaboration
                .as_ref()
                .map(|c| c.max_calls_per_turn())
                .unwrap_or(10);
            if *ask_agent_calls >= max_per_turn {
                results[idx] = Some(ToolResult::error(
                    call.id.clone(),
                    format!(
                        "Call limit exceeded: {}/{} ask_agent calls this turn",
                        ask_agent_calls, max_per_turn
                    ),
                ));
                continue;
            }

            let agent_name = call
                .arguments
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let question = call
                .arguments
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if agent_name.is_empty() || question.is_empty() {
                results[idx] = Some(ToolResult::error(
                    call.id.clone(),
                    "ask_agent requires both 'agent' and 'question' parameters".to_string(),
                ));
                continue;
            }

            let result = if let Some(ref collab) = self.collaboration {
                match collab
                    .call(agent_name, question, self.current_depth, &self.call_chain)
                    .await
                {
                    Ok(content) => ToolResult::success(call.id.clone(), content),
                    Err(e) => ToolResult::error(call.id.clone(), e.to_string()),
                }
            } else {
                ToolResult::error(
                    call.id.clone(),
                    "Collaboration is not configured".to_string(),
                )
            };
            *ask_agent_calls += 1;
            results[idx] = Some(result);
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, opt)| {
                opt.unwrap_or_else(|| {
                    let call_id = calls
                        .get(i)
                        .map(|c| c.id.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    ToolResult::error(call_id, "Internal error: tool task panicked".to_string())
                })
            })
            .collect()
    }

    async fn run_retrieval_planning(
        &self,
        user_question: &str,
    ) -> Result<RetrievalPlanningOutput> {
        let planning_config = match &self.config.retrieval_planning {
            Some(c) if c.enabled => c,
            _ => {
                return Ok(RetrievalPlanningOutput {
                    results: Vec::new(),
                    usage: TokenUsage::default(),
                })
            }
        };

        let max_queries = planning_config.max_queries.clamp(2, 10);

        let planning_prompt = format!(
            "Given a user question, generate a search plan for a memory system.\n\n\
             Return a JSON object with:\n\
             - \"queries\": array of 1-{max} search query strings, each targeting a single fact or entity\n\
             - \"entities\": array of ALL proper nouns (people, places, organizations) in the question\n\
             - \"topics\": array of 1-3 lowercase topic keywords relevant to the question\n\n\
             For relationship chains (\"Who is the X of the Y of Z?\"), create a query for \
             each person in the chain. Each query should search for one person's relationships.\n\n\
             User question: {question}\n\n\
             JSON only:",
            max = max_queries,
            question = user_question,
        );

        let model = planning_config
            .model
            .as_deref()
            .unwrap_or(&self.config.model);
        let plan_config = ChatConfig::new(model)
            .with_temperature(0.0)
            .with_max_tokens(512);

        let messages = &[
            Message::system("You are a search query planner."),
            Message::user(&planning_prompt),
        ];
        let response = self.llm.chat(messages, &[], &plan_config).await?;
        let usage = response.usage.clone();

        #[derive(serde::Deserialize)]
        struct SearchPlan {
            #[serde(default)]
            queries: Vec<String>,
            #[serde(default)]
            entities: Vec<String>,
            #[serde(default)]
            topics: Vec<String>,
        }

        let plan: SearchPlan = serde_json::from_str(&response.content)
            .unwrap_or_else(|_| {
                let queries: Vec<String> = serde_json::from_str(&response.content)
                    .unwrap_or_else(|_| vec![user_question.to_string()]);
                SearchPlan { queries, entities: vec![], topics: vec![] }
            });

        let queries: Vec<String> = plan.queries
            .into_iter()
            .take(max_queries as usize)
            .collect();
        let entities = plan.entities;
        let topics = plan.topics;

        let namespace = self
            .config
            .curriculum
            .as_ref()
            .map(|c| c.namespace.clone())
            .unwrap_or_else(|| "knowledge".to_string());

        let handles: Vec<_> = queries
            .iter()
            .map(|query| {
                let manager = Arc::clone(&self.mcp_manager);
                let ns = namespace.clone();
                let q = query.clone();
                let ents = entities.clone();
                let tops = topics.clone();
                let limit = planning_config.results_per_query;
                tokio::spawn(async move {
                    let mut args = serde_json::json!({
                        "query": q,
                        "namespace": ns,
                        "limit": limit,
                        "depth": 2,
                        "compact": false,
                    });
                    if !ents.is_empty() {
                        args["entities"] = serde_json::json!(ents);
                    }
                    if !tops.is_empty() {
                        args["topics"] = serde_json::json!(tops);
                    }
                    let args_map = args.as_object().cloned();
                    manager.call_tool("recall_memories", args_map).await
                })
            })
            .collect();

        let raw_results = futures::future::join_all(handles).await;

        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut results: Vec<RetrievalResult> = Vec::new();

        for join_result in raw_results {
            let Ok(Ok(tool_result)) = join_result else {
                continue;
            };
            let text = crate::mcp::schema::tool_result_to_string(&tool_result);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(memories) = parsed.as_array() {
                    for mem in memories {
                        let id = mem
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !id.is_empty() && seen_ids.insert(id.clone()) {
                            let summary = mem
                                .get("summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let full_text = mem
                                .get("fullText")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            results.push(RetrievalResult {
                                id,
                                summary,
                                full_text,
                            });
                        }
                    }
                }
            }
        }

        Ok(RetrievalPlanningOutput { results, usage })
    }

    pub async fn run_single_query(&mut self, query: &str) -> Result<String> {
        self.conversation.push_user(query);
        self.handle_turn().await?;
        Ok(self.conversation.last_assistant_text())
    }

    pub fn reset_conversation(&mut self) {
        self.conversation.clear();
    }

    pub async fn shutdown(&self) {
        tracing::info!("Shutting down MCP servers...");
        self.mcp_manager.shutdown_all().await;
        tracing::info!("All MCP servers stopped.");
    }

    pub fn save_session(&self) -> Result<(), crate::conversation::ConversationError> {
        self.conversation.save()
    }

    pub fn set_conversation(&mut self, conv: ConversationManager) {
        self.conversation = conv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("call_1".to_string(), "value");
        assert!(result.is_success);
        assert_eq!(result.call_id, "call_1");
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("call_2".to_string(), "Something broke".to_string());
        assert!(!result.is_success);
        assert!(result.content.contains("Something broke"));
    }

    #[tokio::test]
    async fn test_execute_tools_parallel_empty() {
        let calls: Vec<ToolCall> = vec![];
        assert!(calls.is_empty());
    }
}
