//! LLM abstraction layer for the Pupil agent runtime.
//!
//! All agent code interacts with LLMs exclusively through the [`LlmProvider`]
//! trait. Two implementations exist:
//!
//! - [`GenaiProvider`] (default): Covers 25+ providers via the `genai` crate.
//! - [`OpenAiCompatProvider`]: Azure OpenAI and custom `/v1/chat/completions`
//!   endpoints via `async-openai`.

mod provider;
mod genai_provider;
mod openai_compat;
pub mod pricing;

pub use provider::resolve_provider;
pub use genai_provider::GenaiProvider;
pub use openai_compat::OpenAiCompatProvider;
pub use pricing::{CostTracker, ModelPricing, model_pricing, estimate_source_cost, PRICING};

use std::pin::Pin;
use futures::Stream;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("unknown model format: {model}")]
    UnknownModelFormat { model: String },

    #[error("missing API key: environment variable {var} is not set")]
    MissingApiKey { var: String },

    #[error("genai error: {0}")]
    Genai(#[from] genai::Error),

    #[error("openai-compat error: {0}")]
    OpenAiCompat(#[from] async_openai::error::OpenAIError),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("response truncated: max output tokens reached")]
    MaxTokens,

    #[error("content filtered by provider safety system")]
    ContentFiltered,

    #[error("rate limited after {retries} retries")]
    RateLimited { retries: u32 },

    #[error("unexpected response format: {detail}")]
    UnexpectedResponse { detail: String },

    #[error("stream interrupted: {0}")]
    StreamInterrupted(String),

    #[error("model {model} is not supported by provider {provider}")]
    UnsupportedModel { model: String, provider: String },

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    ContentFiltered,
    Other(String),
}

/// Maximum number of retries when the LLM returns a malformed function call
/// (e.g., Gemini's MALFORMED_FUNCTION_CALL finish reason). Applies per
/// section in the learning loop and per turn in the runtime agent loop.
pub const MAX_MALFORMED_RETRIES: u32 = 3;

impl StopReason {
    /// Returns `true` if this stop reason indicates the model attempted a tool
    /// call but produced invalid JSON. Currently only Gemini models return this
    /// (as `MALFORMED_FUNCTION_CALL`), but this method centralizes the check so
    /// additional patterns can be added later.
    pub fn is_malformed_function_call(&self) -> bool {
        matches!(self, StopReason::Other(s) if s == "MALFORMED_FUNCTION_CALL")
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_success: bool,
}

impl ToolResult {
    pub fn success(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_success: true,
        }
    }

    pub fn error(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_success: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Message::Assistant {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Message::Assistant {
            content: content.into(),
            tool_calls,
        }
    }

    pub fn tool_result_success(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Message::ToolResult {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_result_error(
        tool_call_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Message::ToolResult {
            tool_call_id: tool_call_id.into(),
            content: error.into(),
            is_error: true,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResponseSchema {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub schema: serde_json::Value,
    #[serde(default = "default_strict")]
    pub strict: bool,
}

fn default_strict() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatConfig {
    pub model: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub json_mode: bool,
    #[serde(default)]
    pub response_schema: Option<ResponseSchema>,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop_sequences: Vec::new(),
            json_mode: false,
            response_schema: None,
        }
    }
}

impl ChatConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn with_temperature(mut self, temp: f64) -> Self {
        self.temperature = Some(temp);
        self
    }

    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    pub fn with_top_p(mut self, p: f64) -> Self {
        self.top_p = Some(p);
        self
    }

    pub fn with_stop_sequence(mut self, seq: impl Into<String>) -> Self {
        self.stop_sequences.push(seq.into());
        self
    }

    pub fn with_response_schema(mut self, schema: ResponseSchema) -> Self {
        self.response_schema = Some(schema);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub stop_reason: StopReason,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta(String),
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        arguments_delta: String,
    },
    Done {
        usage: TokenUsage,
        stop_reason: StopReason,
    },
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ChatConfig,
    ) -> Result<ChatResponse, LlmError>;

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &ChatConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_malformed_function_call() {
        assert!(StopReason::Other("MALFORMED_FUNCTION_CALL".to_string())
            .is_malformed_function_call());
    }

    #[test]
    fn test_is_not_malformed_function_call_other_string() {
        assert!(!StopReason::Other("SOME_OTHER_REASON".to_string())
            .is_malformed_function_call());
    }

    #[test]
    fn test_is_not_malformed_function_call_end_turn() {
        assert!(!StopReason::EndTurn.is_malformed_function_call());
    }

    #[test]
    fn test_is_not_malformed_function_call_tool_use() {
        assert!(!StopReason::ToolUse.is_malformed_function_call());
    }
}
