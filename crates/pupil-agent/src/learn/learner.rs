#[cfg(feature = "learn")]
use crate::llm::{
    ChatConfig, ChatResponse, LlmProvider, Message, StopReason, ToolCall, ToolResult,
};
#[cfg(feature = "learn")]
use crate::mcp::McpManager;
#[cfg(feature = "learn")]
use super::reader::ReadingSection;

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct LearningResult {
    pub memory_ids: Vec<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[cfg(feature = "learn")]
struct LearningConversation {
    messages: Vec<Message>,
    memory_ids: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    system_prompt: String,
}

#[cfg(feature = "learn")]
const MAX_ITERATIONS_PER_SECTION: usize = 50;

#[cfg(feature = "learn")]
const MAX_SUMMARY_ITERATIONS: usize = 10;

#[cfg(feature = "learn")]
pub async fn learn_source(
    llm: &dyn LlmProvider,
    mcp: &McpManager,
    sections: &[ReadingSection],
    system_prompt: &str,
    source_key: &str,
    namespace: &str,
) -> anyhow::Result<LearningResult> {
    let mut conv = LearningConversation {
        messages: vec![Message::system(system_prompt)],
        memory_ids: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        system_prompt: system_prompt.to_string(),
    };

    let llm_tools: Vec<crate::llm::ToolDefinition> = mcp
        .all_tools()
        .iter()
        .map(|t| t.into())
        .collect();

    tracing::info!(
        tool_count = llm_tools.len(),
        tools = ?llm_tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        "Learning agent initialized with tools"
    );

    let chat_config = ChatConfig {
        temperature: Some(0.3),
        max_tokens: Some(4096),
        ..Default::default()
    };

    for section in sections {
        let user_msg = format!(
            "## Reading Section {}/{} of \"{}\"\n\n\
             Heading: {}\n\n\
             ---\n\n\
             {}\n\n\
             ---\n\n\
             Read this section carefully. Extract the key knowledge and store it using \
             the store_memory tool. Use recall_memories to check what you already know \
             about related topics. Use find_similar_memories after storing to check for \
             duplicates.\n\n\
             Tag all memories with \"source/{}\" and namespace \"{}\".",
            section.section_number,
            section.total_sections,
            section.document_title,
            if section.heading_path.is_empty() {
                "(document root)"
            } else {
                &section.heading_path
            },
            section.text,
            source_key,
            namespace,
        );

        conv.messages.push(Message::user(&user_msg));

        let mut iterations = 0;
        let mut malformed_retries: u32 = 0;
        loop {
            let response = llm
                .chat(&conv.messages, &llm_tools, &chat_config)
                .await
                .map_err(|e| anyhow::anyhow!("LLM error during learning: {}", e))?;

            conv.input_tokens += response.usage.input_tokens;
            conv.output_tokens += response.usage.output_tokens;

            tracing::info!(
                source = source_key,
                section = section.section_number,
                stop_reason = ?response.stop_reason,
                tool_call_count = response.tool_calls.len(),
                content_len = response.content.len(),
                content_preview = %&response.content[..response.content.len().min(200)],
                "LLM response received"
            );

            // Handle malformed function calls before pushing the assistant
            // message. The response contains broken tool call JSON that would
            // corrupt the conversation context if kept.
            if response.stop_reason.is_malformed_function_call() {
                malformed_retries += 1;
                if malformed_retries > crate::llm::MAX_MALFORMED_RETRIES {
                    tracing::warn!(
                        source = source_key,
                        section = section.section_number,
                        retries = crate::llm::MAX_MALFORMED_RETRIES,
                        "Malformed function call: all retries exhausted, \
                         skipping section"
                    );
                    break;
                }
                tracing::warn!(
                    source = source_key,
                    section = section.section_number,
                    retry = malformed_retries,
                    max_retries = crate::llm::MAX_MALFORMED_RETRIES,
                    "Malformed function call from LLM, retrying"
                );
                conv.messages.push(Message::user(
                    "Your previous tool call was malformed. \
                     Please try again with valid JSON.",
                ));
                continue;
            }

            push_assistant_message(&mut conv.messages, &response);

            // Some providers (e.g., Vertex/Gemini) return EndTurn even when
            // tool calls are present. Always execute tool calls first,
            // regardless of stop_reason.
            if !response.tool_calls.is_empty() {
                let tool_results = execute_tool_calls(
                    mcp,
                    &response.tool_calls,
                    &mut conv.memory_ids,
                    namespace,
                )
                .await;
                for result in tool_results {
                    if result.is_success {
                        conv.messages.push(Message::tool_result_success(
                            result.call_id,
                            result.content,
                        ));
                    } else {
                        conv.messages.push(Message::tool_result_error(
                            result.call_id,
                            result.content,
                        ));
                    }
                }

                // After executing tools, if stop_reason was EndTurn,
                // we still need to send tool results back to the LLM
                // for it to continue (or finish).
                if matches!(response.stop_reason, StopReason::EndTurn) {
                    continue;
                }
            } else {
                match response.stop_reason {
                    StopReason::EndTurn => {
                        tracing::info!(
                            source = source_key,
                            section = section.section_number,
                            "Section learning complete"
                        );
                        break;
                    }
                    StopReason::MaxTokens => {
                        tracing::warn!(
                            source = source_key,
                            section = section.section_number,
                            "LLM response truncated (max tokens)"
                        );
                        break;
                    }
                    other => {
                        tracing::warn!(
                            source = source_key,
                            section = section.section_number,
                            reason = ?other,
                            "Unexpected stop reason; ending section"
                        );
                        break;
                    }
                }
            }

            // Reset malformed retry counter on any successful iteration
            // (the model recovered and produced a valid response).
            malformed_retries = 0;

            iterations += 1;
            if iterations >= MAX_ITERATIONS_PER_SECTION {
                tracing::warn!(
                    source = source_key,
                    section = section.section_number,
                    iterations,
                    "Max iterations reached for section"
                );
                break;
            }
        }

        if section.is_summary_checkpoint {
            let summary_prompt = "You have now read several sections. Before continuing, \
                summarize the key knowledge you have extracted and stored so far. This summary \
                will carry forward as context when we continue reading the remaining sections. \
                Be concise but comprehensive.";

            conv.messages.push(Message::user(summary_prompt));

            let mut summary_iterations = 0;
            let mut malformed_retries: u32 = 0;
            let summary_text;
            loop {
                let response = llm
                    .chat(&conv.messages, &llm_tools, &chat_config)
                    .await
                    .map_err(|e| anyhow::anyhow!("LLM error during summary: {}", e))?;

                conv.input_tokens += response.usage.input_tokens;
                conv.output_tokens += response.usage.output_tokens;

                // Handle malformed function calls before pushing the assistant
                // message. Same logic as the section loop.
                if response.stop_reason.is_malformed_function_call() {
                    malformed_retries += 1;
                    if malformed_retries > crate::llm::MAX_MALFORMED_RETRIES {
                        tracing::warn!(
                            source = source_key,
                            section = section.section_number,
                            retries = crate::llm::MAX_MALFORMED_RETRIES,
                            "Malformed function call in summary: all retries \
                             exhausted, using partial summary"
                        );
                        summary_text = response.content.clone();
                        break;
                    }
                    tracing::warn!(
                        source = source_key,
                        section = section.section_number,
                        retry = malformed_retries,
                        max_retries = crate::llm::MAX_MALFORMED_RETRIES,
                        "Malformed function call in summary, retrying"
                    );
                    conv.messages.push(Message::user(
                        "Your previous tool call was malformed. \
                         Please try again with valid JSON.",
                    ));
                    continue;
                }

                push_assistant_message(&mut conv.messages, &response);

                match response.stop_reason {
                    StopReason::EndTurn => {
                        summary_text = response.content.clone();
                        break;
                    }
                    StopReason::ToolUse => {
                        let tool_results = execute_tool_calls(
                            mcp,
                            &response.tool_calls,
                            &mut conv.memory_ids,
                            namespace,
                        )
                        .await;
                        for result in tool_results {
                            if result.is_success {
                                conv.messages.push(Message::tool_result_success(
                                    result.call_id,
                                    result.content,
                                ));
                            } else {
                                conv.messages.push(Message::tool_result_error(
                                    result.call_id,
                                    result.content,
                                ));
                            }
                        }
                    }
                    StopReason::MaxTokens => {
                        summary_text = response.content.clone();
                        break;
                    }
                    other => {
                        tracing::warn!(
                            reason = ?other,
                            "Unexpected stop reason in summary"
                        );
                        summary_text = response.content.clone();
                        break;
                    }
                }

                // Reset malformed retry counter on successful iteration.
                malformed_retries = 0;

                summary_iterations += 1;
                if summary_iterations >= MAX_SUMMARY_ITERATIONS {
                    summary_text =
                        String::from("(summary truncated due to iteration limit)");
                    break;
                }
            }

            tracing::info!(
                source = source_key,
                section = section.section_number,
                memories_so_far = conv.memory_ids.len(),
                "Summary checkpoint: resetting conversation"
            );

            conv.messages = vec![
                Message::system(&conv.system_prompt),
                Message::user(&format!(
                    "You are continuing to study \"{}\". Here is a summary of what you \
                     have learned from the first {} sections:\n\n{}\n\n\
                     Continue reading the remaining sections.",
                    section.document_title, section.section_number, summary_text,
                )),
                Message::assistant(
                    "Understood. I have reviewed my prior learning summary and am ready \
                     to continue studying the remaining sections. I will build on what I \
                     have already stored and look for new concepts, relationships, and details.",
                ),
            ];
        }
    }

    Ok(LearningResult {
        memory_ids: conv.memory_ids,
        input_tokens: conv.input_tokens,
        output_tokens: conv.output_tokens,
    })
}

#[cfg(feature = "learn")]
fn push_assistant_message(messages: &mut Vec<Message>, response: &ChatResponse) {
    messages.push(Message::assistant_with_tool_calls(
        response.content.clone(),
        response.tool_calls.clone(),
    ));
}

#[cfg(feature = "learn")]
async fn execute_tool_calls(
    mcp: &McpManager,
    tool_calls: &[ToolCall],
    memory_ids: &mut Vec<String>,
    namespace: &str,
) -> Vec<ToolResult> {
    let mut results = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        let mut args = call.arguments.clone();

        if call.name == "store_memory" {
            if let serde_json::Value::Object(ref mut map) = args {
                if !map.contains_key("namespace") {
                    map.insert(
                        "namespace".to_string(),
                        serde_json::Value::String(namespace.to_string()),
                    );
                }
            }
        }

        let arguments = args.as_object().cloned();

        let timeout_duration = std::time::Duration::from_secs(30);
        let result = match tokio::time::timeout(
            timeout_duration,
            mcp.call_tool(&call.name, arguments),
        )
        .await
        {
            Ok(Ok(mcp_result)) => {
                let result_text = crate::mcp::schema::tool_result_to_string(&mcp_result);

                if call.name == "store_memory" {
                    if let Some(id) = extract_memory_id_from_result(&result_text) {
                        tracing::debug!(
                            tool = "store_memory",
                            memory_id = %id,
                            "Memory stored"
                        );
                        memory_ids.push(id);
                    }
                }
                ToolResult::success(call.id.clone(), result_text)
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    tool = %call.name,
                    error = %e,
                    "Tool call failed"
                );
                ToolResult::error(call.id.clone(), format!("Tool call failed: {}", e))
            }
            Err(_) => {
                tracing::warn!(
                    tool = %call.name,
                    "Tool call timed out"
                );
                ToolResult::error(
                    call.id.clone(),
                    "Tool execution timed out after 30 seconds",
                )
            }
        };

        results.push(result);
    }

    results
}

#[cfg(feature = "learn")]
fn extract_memory_id_from_result(result_text: &str) -> Option<String> {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result_text) {
        if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }

    for line in result_text.lines() {
        let trimmed = line.trim();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }

    if result_text.contains("\"id\"") {
        if let Some(start) = result_text.find("\"id\"") {
            let after_key = &result_text[start + 4..];
            if let Some(colon_pos) = after_key.find(':') {
                let after_colon = after_key[colon_pos + 1..].trim_start();
                if after_colon.starts_with('"') {
                    let value_start = 1;
                    if let Some(end_quote) = after_colon[value_start..].find('"') {
                        return Some(after_colon[value_start..value_start + end_quote].to_string());
                    }
                }
            }
        }
    }

    tracing::warn!(
        "Could not extract memory ID from store_memory result: {}",
        &result_text[..result_text.len().min(200)]
    );
    None
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_extract_memory_id_json() {
        let result = r#"{"id": "550e8400-e29b-41d4-a716-446655440000"}"#;
        let id = extract_memory_id_from_result(result);
        assert_eq!(
            id,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_extract_memory_id_not_found() {
        let result = "some random text without an id";
        let id = extract_memory_id_from_result(result);
        assert!(id.is_none());
    }

    #[test]
    fn test_extract_memory_id_nested() {
        let result = r#"Memory stored: {"id": "abc-123", "status": "ok"}"#;
        let id = extract_memory_id_from_result(result);
        assert_eq!(id, Some("abc-123".to_string()));
    }
}
