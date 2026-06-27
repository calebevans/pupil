use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::AgentConfig;
use crate::conversation::ConversationManager;
use crate::llm::{ChatConfig, LlmProvider, StopReason, ToolCall, ToolDefinition, ToolResult};
use crate::mcp::McpManager;
use crate::prompt::SystemPromptBuilder;

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

pub struct Agent {
    config: AgentConfig,
    conversation: ConversationManager,
    mcp_manager: Arc<McpManager>,
    llm: Box<dyn LlmProvider>,
    chat_config: ChatConfig,
    cancel: CancellationToken,
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

        let llm_tools: Vec<ToolDefinition> = mcp_manager
            .all_tools()
            .iter()
            .map(|t| t.into())
            .collect();

        let namespace = config
            .curriculum
            .as_ref()
            .map(|c| c.namespace.clone())
            .unwrap_or_else(|| "knowledge".to_string());
        let system_prompt = SystemPromptBuilder::new(
            config.name.clone(),
            config.description.clone(),
            config.system_prompt.clone(),
            llm_tools,
            namespace,
        )
        .build();

        let session_id = Uuid::new_v4();
        let conversation = ConversationManager::new(system_prompt, session_id);

        let chat_config = ChatConfig::new(config.model.clone())
            .with_temperature(config.temperature);
        let chat_config = if let Some(max_tokens) = config.max_output_tokens {
            chat_config.with_max_tokens(max_tokens)
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
        let mut iterations: u32 = 0;
        let mut malformed_retries: u32 = 0;
        let turn_start_tokens = self.conversation.total_tokens();

        loop {
            if self.cancel.is_cancelled() {
                return Err(AgentError::Shutdown.into());
            }

            let messages = self.conversation.messages();
            let llm_tools: Vec<ToolDefinition> = self
                .mcp_manager
                .all_tools()
                .iter()
                .map(|t| t.into())
                .collect();

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

                let results = self.execute_tools_parallel(&tool_calls).await;
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

    async fn execute_tools_parallel(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        if calls.is_empty() {
            return Vec::new();
        }

        let handles: Vec<_> = calls
            .iter()
            .map(|call| {
                let manager = Arc::clone(&self.mcp_manager);
                let timeout = self.config.tool_timeout;
                let call = call.clone();
                tokio::spawn(async move {
                    let timer = Instant::now();
                    let arguments = call.arguments.as_object().cloned();
                    match tokio::time::timeout(timeout, manager.call_tool(&call.name, arguments))
                        .await
                    {
                        Ok(Ok(result)) => {
                            let result_text = crate::mcp::schema::tool_result_to_string(&result);
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
                    }
                })
            })
            .collect();

        let results = futures::future::join_all(handles).await;
        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.unwrap_or_else(|join_err| {
                    let call_id = calls
                        .get(i)
                        .map(|c| c.id.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    tracing::error!(
                        error = %join_err,
                        "Tool task panicked"
                    );
                    ToolResult::error(call_id, "Internal error: tool task panicked".to_string())
                })
            })
            .collect()
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
