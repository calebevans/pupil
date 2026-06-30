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
const MAX_ITERATIONS_SYNTHESIS: usize = 50;

#[cfg(feature = "learn")]
const SYNTHESIS_BATCH_SIZE: usize = 10;

#[cfg(feature = "learn")]
struct FetchedMemory {
    id: String,
    summary: String,
    entities: Vec<String>,
    topics: Vec<String>,
}

#[cfg(feature = "learn")]
async fn fetch_memory(mcp: &McpManager, id: &str) -> anyhow::Result<FetchedMemory> {
    let args = serde_json::json!({"id": id});
    let arguments = args.as_object().cloned();
    let result = mcp.call_tool("get_memory", arguments).await?;
    let result_text = crate::mcp::schema::tool_result_to_string(&result);
    let parsed: serde_json::Value = serde_json::from_str(&result_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse get_memory response: {}", e))?;

    let fetched_id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(id)
        .to_string();

    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    fn extract_string_array(val: &serde_json::Value, key: &str) -> Vec<String> {
        val.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    Ok(FetchedMemory {
        id: fetched_id,
        summary,
        entities: extract_string_array(&parsed, "entities"),
        topics: extract_string_array(&parsed, "topics"),
    })
}

#[cfg(feature = "learn")]
fn build_batch_message(
    batch_memories: &[FetchedMemory],
    batch_number: usize,
    total_batches: usize,
    source_key: &str,
) -> String {
    let mut msg = format!(
        "## Synthesis Batch {}/{}\n\n\
         Below are memories you stored while reading \"{}\". For each memory, \
         use find_similar_memories to discover related memories across the knowledge \
         base. When you find meaningful connections, create a new relationship memory \
         using store_memory.\n",
        batch_number, total_batches, source_key,
    );

    for (i, mem) in batch_memories.iter().enumerate() {
        msg.push_str(&format!(
            "\n### Memory {} (ID: {})\nSummary: {}\nEntities: {}\nTopics: {}\n",
            i + 1,
            mem.id,
            mem.summary,
            if mem.entities.is_empty() {
                "(none)".to_string()
            } else {
                mem.entities.join(", ")
            },
            if mem.topics.is_empty() {
                "(none)".to_string()
            } else {
                mem.topics.join(", ")
            },
        ));
    }

    msg
}

#[cfg(feature = "learn")]
pub async fn synthesize_relationships(
    llm: &dyn LlmProvider,
    mcp: &McpManager,
    memory_ids: &[String],
    system_prompt: &str,
    source_key: &str,
    namespace: &str,
) -> anyhow::Result<LearningResult> {
    if memory_ids.is_empty() {
        return Ok(LearningResult {
            memory_ids: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
        });
    }

    let mut conv = LearningConversation {
        messages: vec![Message::system(system_prompt)],
        memory_ids: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        system_prompt: system_prompt.to_string(),
    };

    let llm_tools: Vec<crate::llm::ToolDefinition> = mcp
        .tools_for_stage("learning")
        .into_iter()
        .map(|t| crate::llm::ToolDefinition::from(t))
        .collect();

    let chat_config = ChatConfig {
        temperature: Some(0.3),
        max_tokens: Some(4096),
        ..Default::default()
    };

    let total_batches = (memory_ids.len() + SYNTHESIS_BATCH_SIZE - 1) / SYNTHESIS_BATCH_SIZE;
    let needs_resets = total_batches > 5;

    tracing::info!(
        source = source_key,
        memory_count = memory_ids.len(),
        total_batches,
        "Starting relationship synthesis"
    );

    for (batch_index, batch_start) in
        (0..memory_ids.len()).step_by(SYNTHESIS_BATCH_SIZE).enumerate()
    {
        let batch_end = (batch_start + SYNTHESIS_BATCH_SIZE).min(memory_ids.len());
        let batch_ids = &memory_ids[batch_start..batch_end];

        let mut batch_memories = Vec::new();
        for id in batch_ids {
            match fetch_memory(mcp, id).await {
                Ok(mem) => batch_memories.push(mem),
                Err(e) => {
                    tracing::warn!(
                        memory_id = %id,
                        error = %e,
                        "Skipping unfetchable memory"
                    );
                }
            }
        }

        if batch_memories.is_empty() {
            continue;
        }

        let user_msg =
            build_batch_message(&batch_memories, batch_index + 1, total_batches, source_key);
        conv.messages.push(Message::user(&user_msg));

        let mut iterations = 0;
        let mut malformed_retries: u32 = 0;
        loop {
            let response = llm
                .chat(&conv.messages, &llm_tools, &chat_config)
                .await
                .map_err(|e| anyhow::anyhow!("LLM error during synthesis: {}", e))?;

            conv.input_tokens += response.usage.input_tokens;
            conv.output_tokens += response.usage.output_tokens;

            tracing::info!(
                source = source_key,
                batch = batch_index + 1,
                stop_reason = ?response.stop_reason,
                tool_call_count = response.tool_calls.len(),
                "Synthesis LLM response received"
            );

            if response.stop_reason.is_malformed_function_call() {
                malformed_retries += 1;
                if malformed_retries > crate::llm::MAX_MALFORMED_RETRIES {
                    tracing::warn!(
                        source = source_key,
                        batch = batch_index + 1,
                        "Malformed function call in synthesis: retries exhausted"
                    );
                    break;
                }
                conv.messages.push(Message::user(
                    "Your previous tool call was malformed. \
                     Please try again with valid JSON.",
                ));
                continue;
            }

            push_assistant_message(&mut conv.messages, &response);

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

                if matches!(response.stop_reason, StopReason::EndTurn) {
                    continue;
                }
            } else {
                match response.stop_reason {
                    StopReason::EndTurn => {
                        tracing::info!(
                            source = source_key,
                            batch = batch_index + 1,
                            "Synthesis batch complete"
                        );
                        break;
                    }
                    StopReason::MaxTokens => {
                        tracing::warn!(
                            source = source_key,
                            batch = batch_index + 1,
                            "Synthesis response truncated (max tokens)"
                        );
                        break;
                    }
                    other => {
                        tracing::warn!(
                            source = source_key,
                            batch = batch_index + 1,
                            reason = ?other,
                            "Unexpected stop reason in synthesis"
                        );
                        break;
                    }
                }
            }

            malformed_retries = 0;

            iterations += 1;
            if iterations >= MAX_ITERATIONS_SYNTHESIS {
                tracing::warn!(
                    source = source_key,
                    batch = batch_index + 1,
                    iterations,
                    "Max iterations reached for synthesis batch"
                );
                break;
            }
        }

        if needs_resets
            && batch_index > 0
            && batch_index % 5 == 0
            && batch_index < total_batches - 1
        {
            let summary_prompt = "Summarize the relationship memories you have created so far \
                during this synthesis pass. This summary will carry forward as context when we \
                continue synthesizing the remaining batches. Be concise but comprehensive.";

            conv.messages.push(Message::user(summary_prompt));

            let mut summary_iterations = 0;
            let mut malformed_retries: u32 = 0;
            let summary_text;
            loop {
                let response = llm
                    .chat(&conv.messages, &llm_tools, &chat_config)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("LLM error during synthesis summary: {}", e)
                    })?;

                conv.input_tokens += response.usage.input_tokens;
                conv.output_tokens += response.usage.output_tokens;

                if response.stop_reason.is_malformed_function_call() {
                    malformed_retries += 1;
                    if malformed_retries > crate::llm::MAX_MALFORMED_RETRIES {
                        summary_text = response.content.clone();
                        break;
                    }
                    conv.messages.push(Message::user(
                        "Your previous tool call was malformed. \
                         Please try again with valid JSON.",
                    ));
                    continue;
                }

                push_assistant_message(&mut conv.messages, &response);

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

                    if matches!(response.stop_reason, StopReason::EndTurn) {
                        continue;
                    }
                } else {
                    match response.stop_reason {
                        StopReason::EndTurn | StopReason::MaxTokens => {
                            summary_text = response.content.clone();
                            break;
                        }
                        other => {
                            tracing::warn!(
                                reason = ?other,
                                "Unexpected stop reason in synthesis summary"
                            );
                            summary_text = response.content.clone();
                            break;
                        }
                    }
                }

                malformed_retries = 0;

                summary_iterations += 1;
                if summary_iterations >= MAX_SUMMARY_ITERATIONS {
                    summary_text =
                        String::from("(synthesis summary truncated due to iteration limit)");
                    break;
                }
            }

            tracing::info!(
                source = source_key,
                batch = batch_index + 1,
                relationships_so_far = conv.memory_ids.len(),
                "Synthesis checkpoint: resetting conversation"
            );

            conv.messages = vec![
                Message::system(&conv.system_prompt),
                Message::user(&format!(
                    "You are continuing relationship synthesis for \"{}\". Here is a summary \
                     of the relationship memories you have created so far:\n\n{}\n\n\
                     Continue synthesizing relationships for the remaining batches.",
                    source_key, summary_text,
                )),
                Message::assistant(
                    "Understood. I have reviewed the relationship memories created so far \
                     and will continue synthesizing connections for the remaining batches, \
                     avoiding duplicates.",
                ),
            ];
        }
    }

    tracing::info!(
        source = source_key,
        relationships_created = conv.memory_ids.len(),
        input_tokens = conv.input_tokens,
        output_tokens = conv.output_tokens,
        "Relationship synthesis complete"
    );

    Ok(LearningResult {
        memory_ids: conv.memory_ids,
        input_tokens: conv.input_tokens,
        output_tokens: conv.output_tokens,
    })
}

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
        .tools_for_stage("synthesis")
        .into_iter()
        .map(|t| crate::llm::ToolDefinition::from(t))
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

                    if matches!(response.stop_reason, StopReason::EndTurn) {
                        continue;
                    }
                } else {
                    match response.stop_reason {
                        StopReason::EndTurn => {
                            summary_text = response.content.clone();
                            break;
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
    if tool_calls.is_empty() {
        return Vec::new();
    }

    let handles: Vec<_> = tool_calls
        .iter()
        .enumerate()
        .map(|(idx, call)| {
            let mcp = mcp.clone();
            let call = call.clone();
            let namespace = namespace.to_string();
            tokio::spawn(async move {
                let mut args = call.arguments.clone();

                if call.name == "store_memory" {
                    if let serde_json::Value::Object(ref mut map) = args {
                        if !map.contains_key("namespace") {
                            map.insert(
                                "namespace".to_string(),
                                serde_json::Value::String(namespace),
                            );
                        }
                        let entities_val = map.get("entities");
                        let has_entities = entities_val
                            .and_then(|v| v.as_array())
                            .map_or(false, |a| !a.is_empty());
                        tracing::warn!(
                            has_entities = has_entities,
                            entities_raw = ?entities_val,
                            summary = ?map.get("summary").and_then(|s| s.as_str()).unwrap_or(""),
                            "store_memory entity check"
                        );
                        if !has_entities {
                            return (idx, call.name.clone(), ToolResult::error(
                                call.id.clone(),
                                "Missing required field 'entities'. You MUST include an entities \
                                 array with every proper noun (person name, place, organization) \
                                 mentioned in the summary. Retry this store_memory call with \
                                 the entities field populated.".to_string(),
                            ));
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
                        let result_text =
                            crate::mcp::schema::tool_result_to_string(&mcp_result);
                        tracing::info!(
                            tool = %call.name,
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
                        ToolResult::error(
                            call.id.clone(),
                            format!("Tool call failed: {}", e),
                        )
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

                (idx, call.name.clone(), result)
            })
        })
        .collect();

    let join_results = futures::future::join_all(handles).await;
    let mut results: Vec<Option<ToolResult>> = vec![None; tool_calls.len()];

    for join_result in join_results {
        match join_result {
            Ok((idx, tool_name, result)) => {
                if tool_name == "store_memory" && result.is_success {
                    if let Some(id) = extract_memory_id_from_result(&result.content) {
                        memory_ids.push(id);
                    }
                }
                results[idx] = Some(result);
            }
            Err(join_err) => {
                tracing::error!(error = %join_err, "Tool task panicked");
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(i, opt)| {
            opt.unwrap_or_else(|| {
                let call_id = tool_calls
                    .get(i)
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                ToolResult::error(call_id, "Internal error: tool task panicked")
            })
        })
        .collect()
}

#[cfg(feature = "learn")]
pub(super) fn extract_memory_id_from_result(result_text: &str) -> Option<String> {
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

#[cfg(feature = "learn")]
fn extract_proper_nouns(text: &str) -> Vec<String> {
    let stop_words: std::collections::HashSet<&str> = [
        "The", "This", "That", "These", "Those", "A", "An", "And", "Or", "But",
        "In", "On", "At", "To", "For", "Of", "With", "By", "From", "As",
        "Is", "Are", "Was", "Were", "Be", "Been", "Being", "Have", "Has", "Had",
        "Do", "Does", "Did", "Will", "Would", "Could", "Should", "May", "Might",
        "It", "Its", "He", "She", "His", "Her", "They", "Their", "We", "Our",
        "If", "When", "Where", "How", "What", "Who", "Which", "Not", "No",
        "All", "Each", "Every", "Both", "Some", "Any", "Many", "Much", "More",
        "Also", "However", "Although", "Because", "Since", "While", "During",
        "Before", "After", "Here", "There", "Then", "Now", "Details",
        "Found", "People", "Related", "Summary", "Memory",
    ].into_iter().collect();

    let mut names: Vec<String> = Vec::new();
    let mut current_name: Vec<&str> = Vec::new();

    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
        if clean.is_empty() {
            if current_name.len() >= 2 {
                names.push(current_name.join(" "));
            }
            current_name.clear();
            continue;
        }

        let first_char = clean.chars().next().unwrap();
        if first_char.is_uppercase() && !stop_words.contains(clean) {
            current_name.push(clean);
        } else {
            if current_name.len() >= 2 {
                names.push(current_name.join(" "));
            }
            current_name.clear();
        }
    }
    if current_name.len() >= 2 {
        names.push(current_name.join(" "));
    }

    names.sort();
    names.dedup();
    names
}
