use std::pin::Pin;

use futures::Stream;
use genai::chat::{
    ChatMessage as GenaiChatMessage, ChatOptions as GenaiChatOptions,
    ChatRequest as GenaiChatRequest, ChatStreamEvent, ContentPart as GenaiContentPart,
    MessageContent as GenaiMessageContent, StopReason as GenaiStopReason,
    Tool as GenaiTool, ToolCall as GenaiToolCall, ToolName as GenaiToolName,
    ToolResponse as GenaiToolResponse, Usage as GenaiUsage,
};
use genai::Client as GenaiClient;

use super::{
    ChatConfig, ChatResponse, LlmError, LlmProvider, Message, StopReason, StreamChunk,
    TokenUsage, ToolCall, ToolDefinition,
};

pub struct GenaiProvider {
    client: GenaiClient,
    model_name: String,
}

impl GenaiProvider {
    pub fn new(model_name: impl Into<String>) -> Result<Self, LlmError> {
        let client = GenaiClient::builder().build();
        Ok(Self {
            client,
            model_name: model_name.into(),
        })
    }
}

fn build_chat_request(messages: &[Message]) -> GenaiChatRequest {
    let mut chat_req = GenaiChatRequest::default();
    let mut genai_messages: Vec<GenaiChatMessage> = Vec::new();

    for msg in messages {
        match msg {
            Message::System { content } => {
                if chat_req.system.is_none() {
                    chat_req = chat_req.with_system(content.as_str());
                } else {
                    genai_messages.push(GenaiChatMessage::system(content.as_str()));
                }
            }
            Message::User { content } => {
                genai_messages.push(GenaiChatMessage::user(content.as_str()));
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                if tool_calls.is_empty() {
                    genai_messages.push(GenaiChatMessage::assistant(content.as_str()));
                } else {
                    let genai_tool_calls: Vec<GenaiToolCall> = tool_calls
                        .iter()
                        .map(|tc| GenaiToolCall {
                            call_id: tc.id.clone(),
                            fn_name: tc.name.clone(),
                            fn_arguments: tc.arguments.clone(),
                            thought_signatures: None,
                        })
                        .collect();

                    let mut mc = GenaiMessageContent::from_tool_calls(genai_tool_calls);
                    if !content.is_empty() {
                        mc.insert(0, GenaiContentPart::Text(content.clone()));
                    }
                    genai_messages.push(GenaiChatMessage {
                        role: genai::chat::ChatRole::Assistant,
                        content: mc,
                        options: None,
                    });
                }
            }
            Message::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let result_content = if *is_error {
                    format!("ERROR: {}", content)
                } else {
                    content.clone()
                };
                let tool_response = GenaiToolResponse {
                    call_id: tool_call_id.clone(),
                    fn_name: None,
                    content: result_content,
                };
                genai_messages.push(GenaiChatMessage::from(tool_response));
            }
        }
    }

    chat_req.messages = genai_messages;
    chat_req
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<GenaiTool> {
    tools
        .iter()
        .map(|td| GenaiTool {
            name: GenaiToolName::Custom(td.name.clone()),
            description: Some(td.description.clone()),
            schema: Some(td.input_schema.clone()),
            strict: None,
            config: None,
        })
        .collect()
}

fn convert_chat_options(config: &ChatConfig) -> GenaiChatOptions {
    let mut opts = GenaiChatOptions::default()
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true);

    if let Some(temp) = config.temperature {
        opts = opts.with_temperature(temp);
    }
    if let Some(max) = config.max_tokens {
        opts = opts.with_max_tokens(max);
    }
    if let Some(p) = config.top_p {
        opts = opts.with_top_p(p);
    }
    if !config.stop_sequences.is_empty() {
        opts = opts.with_stop_sequences(config.stop_sequences.clone());
    }

    opts
}

fn convert_stop_reason(reason: &GenaiStopReason) -> StopReason {
    match reason {
        GenaiStopReason::Completed(_) => StopReason::EndTurn,
        GenaiStopReason::ToolCall(_) => StopReason::ToolUse,
        GenaiStopReason::MaxTokens(_) => StopReason::MaxTokens,
        GenaiStopReason::StopSequence(_) => StopReason::StopSequence,
        GenaiStopReason::ContentFilter(_) => StopReason::ContentFiltered,
        GenaiStopReason::Other(s) => StopReason::Other(s.clone()),
    }
}

fn convert_usage(usage: &GenaiUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.prompt_tokens.unwrap_or(0).max(0) as u64,
        output_tokens: usage.completion_tokens.unwrap_or(0).max(0) as u64,
    }
}

fn extract_tool_calls(response: &genai::chat::ChatResponse) -> Vec<ToolCall> {
    response
        .tool_calls()
        .into_iter()
        .map(|tc| ToolCall {
            id: tc.call_id.clone(),
            name: tc.fn_name.clone(),
            arguments: tc.fn_arguments.clone(),
        })
        .collect()
}

#[async_trait::async_trait]
impl LlmProvider for GenaiProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ChatConfig,
    ) -> Result<ChatResponse, LlmError> {
        let mut chat_req = build_chat_request(messages);

        if !tools.is_empty() {
            chat_req = chat_req.with_tools(convert_tools(tools));
        }

        let opts = convert_chat_options(config);

        let response = self
            .client
            .exec_chat(&self.model_name, chat_req, Some(&opts))
            .await?;

        let content = response.first_text().unwrap_or("").to_string();

        let tool_calls = extract_tool_calls(&response);

        let stop_reason = match response.stop_reason {
            Some(ref sr) => convert_stop_reason(sr),
            None => {
                if !tool_calls.is_empty() {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
        };

        let usage = convert_usage(&response.usage);

        Ok(ChatResponse {
            stop_reason,
            content,
            tool_calls,
            usage,
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ChatConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let mut chat_req = build_chat_request(messages);

        if !tools.is_empty() {
            chat_req = chat_req.with_tools(convert_tools(tools));
        }

        let opts = convert_chat_options(config);

        let stream_response = self
            .client
            .exec_chat_stream(&self.model_name, chat_req, Some(&opts))
            .await?;

        // genai emits complete ToolCall objects per ToolCallChunk (not partial
        // deltas like OpenAI). We emit a ToolCallStart with the full arguments
        // serialized as a single delta for each chunk.
        let mapped = futures::stream::unfold(
            (stream_response.stream, 0usize),
            |(mut stream, mut tool_index)| async move {
                use futures::StreamExt;

                loop {
                    match stream.next().await {
                        None => return None,
                        Some(Err(e)) => {
                            return Some((Err(LlmError::Genai(e)), (stream, tool_index)));
                        }
                        Some(Ok(event)) => match event {
                            ChatStreamEvent::Start => {
                                continue;
                            }
                            ChatStreamEvent::Chunk(chunk) => {
                                return Some((
                                    Ok(StreamChunk::TextDelta(chunk.content)),
                                    (stream, tool_index),
                                ));
                            }
                            ChatStreamEvent::ReasoningChunk(_) => {
                                continue;
                            }
                            ChatStreamEvent::ThoughtSignatureChunk(_) => {
                                continue;
                            }
                            ChatStreamEvent::ToolCallChunk(tc) => {
                                let tool_call = &tc.tool_call;
                                let id = if tool_call.call_id.is_empty() {
                                    format!("call_{}", tool_index)
                                } else {
                                    tool_call.call_id.clone()
                                };
                                let idx = tool_index;
                                tool_index += 1;

                                // Emit ToolCallStart with the complete
                                // arguments as a single delta following it.
                                // We pack everything into ToolCallStart since
                                // genai delivers complete tool calls per chunk.
                                return Some((
                                    Ok(StreamChunk::ToolCallStart {
                                        index: idx,
                                        id,
                                        name: tool_call.fn_name.clone(),
                                    }),
                                    (stream, tool_index),
                                ));
                            }
                            ChatStreamEvent::End(end) => {
                                let usage = end
                                    .captured_usage
                                    .as_ref()
                                    .map(convert_usage)
                                    .unwrap_or_default();
                                let stop_reason = end
                                    .captured_stop_reason
                                    .as_ref()
                                    .map(convert_stop_reason)
                                    .unwrap_or(StopReason::EndTurn);
                                return Some((
                                    Ok(StreamChunk::Done { usage, stop_reason }),
                                    (stream, tool_index),
                                ));
                            }
                        },
                    }
                }
            },
        );

        Ok(Box::pin(mapped))
    }
}
