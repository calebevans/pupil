use std::pin::Pin;

use async_openai::{
    config::{AzureConfig, OpenAIConfig},
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionStreamOptions,
        ChatCompletionTool, ChatCompletionTools, CreateChatCompletionRequestArgs, FinishReason,
        FunctionCall, FunctionObject, ResponseFormat, ResponseFormatJsonSchema,
        StopConfiguration,
    },
    Client as OpenAiClient,
};
use futures::Stream;

use super::{
    ChatConfig, ChatResponse, LlmError, LlmProvider, Message, StopReason, StreamChunk,
    TokenUsage, ToolCall, ToolDefinition,
};

enum InnerClient {
    Azure(OpenAiClient<AzureConfig>),
    Custom(OpenAiClient<OpenAIConfig>),
}

pub struct OpenAiCompatProvider {
    inner: InnerClient,
    model_name: String,
}

impl OpenAiCompatProvider {
    pub fn new_azure(model_name: impl Into<String>) -> Result<Self, LlmError> {
        let model_name = model_name.into();

        let api_key = std::env::var("AZURE_OPENAI_API_KEY").map_err(|_| {
            LlmError::MissingApiKey {
                var: "AZURE_OPENAI_API_KEY".to_string(),
            }
        })?;

        let endpoint =
            std::env::var("AZURE_OPENAI_ENDPOINT").map_err(|_| LlmError::MissingApiKey {
                var: "AZURE_OPENAI_ENDPOINT".to_string(),
            })?;

        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .unwrap_or_else(|_| "2024-10-21".to_string());

        let azure_config = AzureConfig::new()
            .with_api_base(endpoint)
            .with_api_key(api_key)
            .with_api_version(api_version)
            .with_deployment_id(&model_name);

        let client = OpenAiClient::with_config(azure_config);

        Ok(Self {
            inner: InnerClient::Azure(client),
            model_name,
        })
    }

    pub fn new_custom(
        base_url: impl Into<String>,
        model_name: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();

        let config = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);

        let client = OpenAiClient::with_config(config);

        Ok(Self {
            inner: InnerClient::Custom(client),
            model_name: model_name.into(),
        })
    }
}

fn convert_messages_to_openai(messages: &[Message]) -> Vec<ChatCompletionRequestMessage> {
    messages
        .iter()
        .map(|msg| match msg {
            Message::System { content } => ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(content.clone()),
                    name: None,
                },
            ),
            Message::User { content } => {
                ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                    content: ChatCompletionRequestUserMessageContent::Text(content.clone()),
                    name: None,
                })
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let oai_tool_calls: Option<Vec<ChatCompletionMessageToolCalls>> =
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(
                            tool_calls
                                .iter()
                                .map(|tc| {
                                    ChatCompletionMessageToolCalls::Function(
                                        ChatCompletionMessageToolCall {
                                            id: tc.id.clone(),
                                            function: FunctionCall {
                                                name: tc.name.clone(),
                                                arguments: serde_json::to_string(&tc.arguments)
                                                    .unwrap_or_else(|_| "{}".to_string()),
                                            },
                                        },
                                    )
                                })
                                .collect(),
                        )
                    };

                let content_field = if content.is_empty() {
                    None
                } else {
                    Some(ChatCompletionRequestAssistantMessageContent::Text(
                        content.clone(),
                    ))
                };

                ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessage {
                        content: content_field,
                        tool_calls: oai_tool_calls,
                        name: None,
                        refusal: None,
                        ..Default::default()
                    },
                )
            }
            Message::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let content_str = if *is_error {
                    format!("ERROR: {}", content)
                } else {
                    content.clone()
                };
                ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
                    tool_call_id: tool_call_id.clone(),
                    content: ChatCompletionRequestToolMessageContent::Text(content_str),
                })
            }
        })
        .collect()
}

fn convert_tools_to_openai(tools: &[ToolDefinition]) -> Vec<ChatCompletionTools> {
    tools
        .iter()
        .map(|td| {
            ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: td.name.clone(),
                    description: Some(td.description.clone()),
                    parameters: Some(td.input_schema.clone()),
                    strict: None,
                },
            })
        })
        .collect()
}

fn extract_tool_calls_from_response(
    tool_calls: &[ChatCompletionMessageToolCalls],
) -> Vec<ToolCall> {
    tool_calls
        .iter()
        .filter_map(|tc| match tc {
            ChatCompletionMessageToolCalls::Function(ftc) => Some(ToolCall {
                id: ftc.id.clone(),
                name: ftc.function.name.clone(),
                arguments: serde_json::from_str(&ftc.function.arguments)
                    .unwrap_or(serde_json::Value::String(ftc.function.arguments.clone())),
            }),
            _ => None,
        })
        .collect()
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ChatConfig,
    ) -> Result<ChatResponse, LlmError> {
        let oai_messages = convert_messages_to_openai(messages);

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model_name).messages(oai_messages);

        if !tools.is_empty() {
            builder.tools(convert_tools_to_openai(tools));
        }
        if let Some(temp) = config.temperature {
            builder.temperature(temp as f32);
        }
        if let Some(max) = config.max_tokens {
            builder.max_completion_tokens(max);
        }
        if let Some(p) = config.top_p {
            builder.top_p(p as f32);
        }
        if !config.stop_sequences.is_empty() {
            builder.stop(StopConfiguration::StringArray(
                config.stop_sequences.clone(),
            ));
        }

        if let Some(ref rs) = config.response_schema {
            builder.response_format(ResponseFormat::JsonSchema {
                json_schema: ResponseFormatJsonSchema {
                    name: rs.name.clone(),
                    description: rs.description.clone(),
                    schema: Some(rs.schema.clone()),
                    strict: Some(rs.strict),
                },
            });
        } else if config.json_mode {
            builder.response_format(ResponseFormat::JsonObject);
        }

        let request = builder
            .build()
            .map_err(|e| LlmError::Other(anyhow::anyhow!("failed to build request: {e}")))?;

        let response = match &self.inner {
            InnerClient::Azure(client) => client.chat().create(request).await?,
            InnerClient::Custom(client) => client.chat().create(request).await?,
        };

        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::UnexpectedResponse {
                detail: "no choices in response".to_string(),
            })?;

        let content = choice.message.content.clone().unwrap_or_default();

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|tcs| extract_tool_calls_from_response(tcs))
            .unwrap_or_default();

        let stop_reason = match choice.finish_reason {
            Some(FinishReason::Stop) => StopReason::EndTurn,
            Some(FinishReason::ToolCalls) => StopReason::ToolUse,
            Some(FinishReason::Length) => StopReason::MaxTokens,
            Some(FinishReason::ContentFilter) => StopReason::ContentFiltered,
            _ => {
                if !tool_calls.is_empty() {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                }
            }
        };

        let usage = response
            .usage
            .map(|u| TokenUsage {
                input_tokens: u.prompt_tokens as u64,
                output_tokens: u.completion_tokens as u64,
            })
            .unwrap_or_default();

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
        let oai_messages = convert_messages_to_openai(messages);

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model_name).messages(oai_messages);

        if !tools.is_empty() {
            builder.tools(convert_tools_to_openai(tools));
        }
        if let Some(temp) = config.temperature {
            builder.temperature(temp as f32);
        }
        if let Some(max) = config.max_tokens {
            builder.max_completion_tokens(max);
        }
        if let Some(p) = config.top_p {
            builder.top_p(p as f32);
        }
        if !config.stop_sequences.is_empty() {
            builder.stop(StopConfiguration::StringArray(
                config.stop_sequences.clone(),
            ));
        }

        if let Some(ref rs) = config.response_schema {
            builder.response_format(ResponseFormat::JsonSchema {
                json_schema: ResponseFormatJsonSchema {
                    name: rs.name.clone(),
                    description: rs.description.clone(),
                    schema: Some(rs.schema.clone()),
                    strict: Some(rs.strict),
                },
            });
        } else if config.json_mode {
            builder.response_format(ResponseFormat::JsonObject);
        }

        builder.stream_options(ChatCompletionStreamOptions {
            include_usage: Some(true),
            include_obfuscation: None,
        });

        let request = builder
            .build()
            .map_err(|e| LlmError::Other(anyhow::anyhow!("failed to build request: {e}")))?;

        let raw_stream = match &self.inner {
            InnerClient::Azure(client) => client.chat().create_stream(request).await?,
            InnerClient::Custom(client) => client.chat().create_stream(request).await?,
        };

        let mapped = futures::stream::unfold(
            (raw_stream, 0usize, TokenUsage::default()),
            |(mut stream, mut tool_index, mut accumulated_usage)| async move {
                use futures::StreamExt;

                loop {
                    match stream.next().await {
                        None => return None,
                        Some(Err(e)) => {
                            return Some((
                                Err(LlmError::OpenAiCompat(e)),
                                (stream, tool_index, accumulated_usage),
                            ));
                        }
                        Some(Ok(response)) => {
                            if let Some(ref usage) = response.usage {
                                accumulated_usage = TokenUsage {
                                    input_tokens: usage.prompt_tokens as u64,
                                    output_tokens: usage.completion_tokens as u64,
                                };
                            }

                            let choice = match response.choices.first() {
                                Some(c) => c,
                                None => continue,
                            };

                            if let Some(ref reason) = choice.finish_reason {
                                let stop_reason = match reason {
                                    FinishReason::Stop => StopReason::EndTurn,
                                    FinishReason::ToolCalls => StopReason::ToolUse,
                                    FinishReason::Length => StopReason::MaxTokens,
                                    FinishReason::ContentFilter => StopReason::ContentFiltered,
                                    _ => StopReason::EndTurn,
                                };
                                return Some((
                                    Ok(StreamChunk::Done {
                                        usage: accumulated_usage.clone(),
                                        stop_reason,
                                    }),
                                    (stream, tool_index, accumulated_usage),
                                ));
                            }

                            if let Some(ref text) = choice.delta.content {
                                if !text.is_empty() {
                                    return Some((
                                        Ok(StreamChunk::TextDelta(text.clone())),
                                        (stream, tool_index, accumulated_usage),
                                    ));
                                }
                            }

                            if let Some(ref tcs) = choice.delta.tool_calls {
                                for tc in tcs {
                                    if let Some(ref func) = tc.function {
                                        if let Some(ref name) = func.name {
                                            let id = tc.id.clone().unwrap_or_else(|| {
                                                format!("call_{}", tool_index)
                                            });
                                            let idx = tool_index;
                                            tool_index += 1;
                                            return Some((
                                                Ok(StreamChunk::ToolCallStart {
                                                    index: idx,
                                                    id,
                                                    name: name.clone(),
                                                }),
                                                (stream, tool_index, accumulated_usage),
                                            ));
                                        }
                                        if let Some(ref args) = func.arguments {
                                            if !args.is_empty() {
                                                let idx = if tool_index > 0 {
                                                    tool_index - 1
                                                } else {
                                                    0
                                                };
                                                return Some((
                                                    Ok(StreamChunk::ToolCallDelta {
                                                        index: idx,
                                                        arguments_delta: args.clone(),
                                                    }),
                                                    (stream, tool_index, accumulated_usage),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }

                            continue;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(mapped))
    }
}
