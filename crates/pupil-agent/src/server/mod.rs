use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::collaboration::{AgentCaller, AgentRegistryEntry};
use crate::config::AgentConfig;
use crate::conversation::ConversationManager;
use crate::llm::{
    ChatConfig, CostTracker, LlmProvider, Message, ResponseSchema, StopReason, TokenUsage,
    ToolCall, ToolDefinition, ToolResult,
};
use crate::mcp::McpManager;
use crate::prompt::{PeerAgent, SystemPromptBuilder};

// ---------------------------------------------------------------------------
// 1. Error Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct ApiErrorBody {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Serialize, Clone)]
pub struct ApiErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Session not found: {0}")]
    SessionNotFound(Uuid),

    #[error("Invalid model: {0}")]
    InvalidModel(String),

    #[error("Messages array must not be empty")]
    EmptyMessages,

    #[error("Last message must have role 'user'")]
    LastMessageNotUser,

    #[error("MCP server unhealthy: {0}")]
    McpServerUnhealthy(String),

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("Rate limit exceeded, retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("Service is shutting down")]
    ShuttingDown,

    #[error(
        "Invalid response_schema: name must match [a-zA-Z0-9_-] \
         and be at most 64 characters"
    )]
    InvalidResponseSchema,

    #[error(
        "Structured response was truncated (max output tokens reached). \
         Increase max_tokens or simplify the schema."
    )]
    StructuredOutputTruncated,
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::EmptyMessages => StatusCode::BAD_REQUEST,
            ApiError::LastMessageNotUser => StatusCode::BAD_REQUEST,
            ApiError::InvalidModel(_) => StatusCode::BAD_REQUEST,
            ApiError::InvalidResponseSchema => StatusCode::BAD_REQUEST,
            ApiError::SessionNotFound(_) => StatusCode::NOT_FOUND,
            ApiError::McpServerUnhealthy(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::StreamError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::StructuredOutputTruncated => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            ApiError::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn error_type(&self) -> &str {
        match self {
            ApiError::BadRequest(_)
            | ApiError::EmptyMessages
            | ApiError::LastMessageNotUser
            | ApiError::InvalidModel(_)
            | ApiError::InvalidResponseSchema => "invalid_request_error",
            ApiError::SessionNotFound(_) => "not_found",
            ApiError::McpServerUnhealthy(_) | ApiError::ShuttingDown => "service_unavailable",
            ApiError::Internal(_)
            | ApiError::StreamError(_)
            | ApiError::StructuredOutputTruncated => "server_error",
            ApiError::RateLimited { .. } => "rate_limit_error",
        }
    }

    fn error_code(&self) -> &str {
        match self {
            ApiError::BadRequest(_) => "invalid_request",
            ApiError::EmptyMessages => "empty_messages",
            ApiError::LastMessageNotUser => "last_message_not_user",
            ApiError::InvalidModel(_) => "invalid_model",
            ApiError::InvalidResponseSchema => "invalid_response_schema",
            ApiError::SessionNotFound(_) => "session_not_found",
            ApiError::McpServerUnhealthy(_) => "mcp_server_unhealthy",
            ApiError::Internal(_) => "internal_error",
            ApiError::StreamError(_) => "stream_failed",
            ApiError::StructuredOutputTruncated => "structured_output_truncated",
            ApiError::RateLimited { .. } => "rate_limit_exceeded",
            ApiError::ShuttingDown => "shutting_down",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                message: self.to_string(),
                error_type: self.error_type().to_string(),
                code: self.error_code().to_string(),
            },
        };

        match &self {
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "Internal server error");
            }
            ApiError::StreamError(e) => {
                tracing::error!(error = %e, "Stream error");
            }
            _ => {
                tracing::warn!(
                    error_code = self.error_code(),
                    error_message = %self,
                    "Client error"
                );
            }
        }

        (status, Json(body)).into_response()
    }
}

// ---------------------------------------------------------------------------
// 2. Request and Response Types
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RequestResponseSchema {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub schema: serde_json::Value,
    #[serde(default = "default_true")]
    pub strict: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: Option<String>,

    pub messages: Vec<ChatMessage>,

    #[serde(default)]
    pub stream: bool,

    #[serde(default)]
    pub temperature: Option<f64>,

    #[serde(default)]
    pub max_tokens: Option<u32>,

    #[serde(default)]
    pub session_id: Option<Uuid>,

    #[serde(default)]
    pub response_schema: Option<RequestResponseSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,

    #[serde(default)]
    pub content: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ChatToolCall>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ChatToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCallFunction {
    pub name: String,
    pub arguments: String,
}

// 2.2 Chat Completion Response (Non-Streaming)

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: ChatUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// 2.3 SSE Streaming Types

#[derive(Debug, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunkChoice {
    pub index: u32,
    pub delta: ChatChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<ChatRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// 2.4 Session Types

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub system_prompt_override: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub created_at: u64,
    pub message_count: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteSessionResponse {
    pub deleted: bool,
    pub session_id: Uuid,
}

// 2.5 Health Response

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_seconds: u64,
    pub model: String,
    pub mcp_servers: HashMap<String, McpServerHealth>,
    pub memory: MemoryStats,
    pub usage_today: UsageStats,
}

#[derive(Debug, Serialize)]
pub struct McpServerHealth {
    pub status: String,
    pub tools: usize,
}

#[derive(Debug, Serialize)]
pub struct MemoryStats {
    pub total_memories: i64,
    pub recall_hit_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct UsageStats {
    pub queries: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub cost_usd: f64,
}

// ---------------------------------------------------------------------------
// 3. Shared Application State
// ---------------------------------------------------------------------------

pub struct AppState {
    pub config: AgentConfig,
    pub system_prompt: String,
    pub llm: Arc<dyn LlmProvider>,
    pub mcp_manager: Arc<McpManager>,
    pub collaboration: Option<AgentCaller>,
    pub sessions: DashMap<Uuid, Arc<tokio::sync::Mutex<ConversationManager>>>,
    pub cost_tracker: Arc<tokio::sync::Mutex<CostTracker>>,
    pub started_at: Instant,
    pub shutdown_token: CancellationToken,
    pub is_shutting_down: AtomicBool,
    pub queries_today: AtomicU64,
    pub tokens_input_today: AtomicU64,
    pub tokens_output_today: AtomicU64,
    pub recall_hits: AtomicU64,
    pub recall_total: AtomicU64,
}

// ---------------------------------------------------------------------------
// 4. Type Conversion Functions
// ---------------------------------------------------------------------------

fn chat_message_to_internal(msg: &ChatMessage) -> Message {
    match msg.role {
        ChatRole::System => Message::system(msg.content.clone().unwrap_or_default()),
        ChatRole::User => Message::user(msg.content.clone().unwrap_or_default()),
        ChatRole::Assistant => {
            let tool_calls: Vec<ToolCall> = msg
                .tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                })
                .collect();
            if tool_calls.is_empty() {
                Message::assistant(msg.content.clone().unwrap_or_default())
            } else {
                Message::assistant_with_tool_calls(
                    msg.content.clone().unwrap_or_default(),
                    tool_calls,
                )
            }
        }
        ChatRole::Tool => Message::tool_result_success(
            msg.tool_call_id.clone().unwrap_or_default(),
            msg.content.clone().unwrap_or_default(),
        ),
    }
}

#[allow(dead_code)]
fn internal_to_chat_message(msg: &Message) -> ChatMessage {
    match msg {
        Message::System { content } => ChatMessage {
            role: ChatRole::System,
            content: if content.is_empty() {
                None
            } else {
                Some(content.clone())
            },
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        },
        Message::User { content } => ChatMessage {
            role: ChatRole::User,
            content: if content.is_empty() {
                None
            } else {
                Some(content.clone())
            },
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        },
        Message::Assistant {
            content,
            tool_calls,
        } => ChatMessage {
            role: ChatRole::Assistant,
            content: if content.is_empty() {
                None
            } else {
                Some(content.clone())
            },
            tool_calls: tool_calls
                .iter()
                .map(|tc| ChatToolCall {
                    id: tc.id.clone(),
                    call_type: "function".to_string(),
                    function: ChatToolCallFunction {
                        name: tc.name.clone(),
                        arguments: serde_json::to_string(&tc.arguments)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                })
                .collect(),
            tool_call_id: None,
            name: None,
        },
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => ChatMessage {
            role: ChatRole::Tool,
            content: if content.is_empty() {
                None
            } else {
                Some(content.clone())
            },
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.clone()),
            name: None,
        },
    }
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs()
}

// ---------------------------------------------------------------------------
// 5. Router Setup
// ---------------------------------------------------------------------------

pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);

    Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/health", get(handle_health))
        .route("/v1/sessions", post(handle_create_session))
        .route("/v1/sessions/{id}", get(handle_get_session))
        .route("/v1/sessions/{id}", delete(handle_delete_session))
        .route("/metrics", get(handle_metrics))
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(1_048_576))
        .with_state(state)
}

pub async fn start_server(
    config: AgentConfig,
    port: u16,
    llm: Arc<dyn LlmProvider>,
    mcp_manager: Arc<McpManager>,
    cost_tracker: Arc<tokio::sync::Mutex<CostTracker>>,
) -> anyhow::Result<()> {
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

    let shutdown_token = CancellationToken::new();

    let state = Arc::new(AppState {
        config,
        system_prompt,
        llm,
        mcp_manager,
        collaboration,
        sessions: DashMap::new(),
        cost_tracker,
        started_at: Instant::now(),
        shutdown_token: shutdown_token.clone(),
        is_shutting_down: AtomicBool::new(false),
        queries_today: AtomicU64::new(0),
        tokens_input_today: AtomicU64::new(0),
        tokens_output_today: AtomicU64::new(0),
        recall_hits: AtomicU64::new(0),
        recall_total: AtomicU64::new(0),
    });

    let router = build_router(state.clone());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    tracing::info!(
        port = port,
        model = %state.config.model,
        mcp_servers = state.config.mcp_servers.len(),
        "HTTP API server starting"
    );

    let signal_token = shutdown_token.clone();
    let signal_state = state.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("Shutdown signal received, draining connections...");
        signal_state.is_shutting_down.store(true, Ordering::SeqCst);
        signal_token.cancel();
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_token.cancelled().await;
        })
        .await?;

    tracing::info!("Connections drained, shutting down MCP servers...");
    state.mcp_manager.shutdown_all().await;

    tracing::info!("Server stopped");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT");
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
        tracing::info!("Received Ctrl+C");
    }
}

// ---------------------------------------------------------------------------
// 6. Handler Implementations
// ---------------------------------------------------------------------------

// 6.1 POST /v1/chat/completions

async fn handle_chat_completions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    if state.is_shutting_down.load(Ordering::SeqCst) {
        return Err(ApiError::ShuttingDown);
    }

    let incoming_depth: u32 = headers
        .get("x-pupil-depth")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let incoming_chain: Vec<String> = headers
        .get("x-pupil-chain")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(',')
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
                .collect()
        })
        .unwrap_or_default();

    if let Some(ref collab) = state.collaboration {
        let max_depth = state
            .config
            .collaboration
            .as_ref()
            .map(|c| c.max_depth)
            .unwrap_or(3);
        if incoming_depth >= max_depth {
            return Err(ApiError::BadRequest(format!(
                "Inter-agent call depth limit exceeded (depth={}, max={})",
                incoming_depth, max_depth
            )));
        }
        let _ = collab;
    }

    if request.messages.is_empty() {
        return Err(ApiError::EmptyMessages);
    }
    if request.messages.last().map(|m| &m.role) != Some(&ChatRole::User) {
        return Err(ApiError::LastMessageNotUser);
    }
    if let Some(ref model) = request.model {
        if model != &state.config.model {
            return Err(ApiError::InvalidModel(format!(
                "This agent uses model '{}', but request specified '{}'",
                state.config.model, model
            )));
        }
    }

    if let Some(temp) = request.temperature {
        if !(0.0..=2.0).contains(&temp) {
            return Err(ApiError::BadRequest(
                "Temperature must be between 0.0 and 2.0".to_string(),
            ));
        }
    }

    if let Some(max_tokens) = request.max_tokens {
        if max_tokens == 0 {
            return Err(ApiError::BadRequest(
                "max_tokens must be positive".to_string(),
            ));
        }
    }

    if let Some(ref rs) = request.response_schema {
        if rs.name.is_empty()
            || rs.name.len() > 64
            || !rs
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ApiError::InvalidResponseSchema);
        }
    }

    let session_id = request.session_id.unwrap_or_else(Uuid::new_v4);
    let is_new_session = request.session_id.is_none();

    let session = if is_new_session {
        let conv = ConversationManager::new(state.system_prompt.clone(), session_id);
        let session = Arc::new(tokio::sync::Mutex::new(conv));
        state.sessions.insert(session_id, session.clone());
        session
    } else {
        state
            .sessions
            .get(&session_id)
            .map(|entry| entry.value().clone())
            .ok_or(ApiError::SessionNotFound(session_id))?
    };

    let mut conv = session.lock().await;

    if is_new_session {
        for msg in &request.messages {
            if msg.role == ChatRole::System {
                continue;
            }
            match msg.role {
                ChatRole::User => conv.push_user(msg.content.as_deref().unwrap_or("")),
                _ => {
                    let internal = chat_message_to_internal(msg);
                    match internal {
                        Message::Assistant { .. } => conv.push_assistant_raw(internal),
                        Message::ToolResult { .. } => {
                            conv.push_tool_result_raw(internal);
                        }
                        _ => {}
                    }
                }
            }
        }
    } else if let Some(last) = request.messages.last() {
        conv.push_user(last.content.as_deref().unwrap_or(""));
    }

    let mut llm_tools: Vec<ToolDefinition> = state
        .mcp_manager
        .all_tools()
        .iter()
        .map(|t| t.into())
        .collect();
    if state.collaboration.is_some() {
        llm_tools.push(crate::collaboration::tool::ask_agent_tool_definition());
    }

    let effective_schema = request
        .response_schema
        .as_ref()
        .map(|rs| ResponseSchema {
            name: rs.name.clone(),
            description: rs.description.clone(),
            schema: rs.schema.clone(),
            strict: rs.strict,
        })
        .or_else(|| {
            state.config.response_schema.as_ref().map(|rs| ResponseSchema {
                name: rs.name.clone(),
                description: rs.description.clone(),
                schema: rs.schema.clone(),
                strict: rs.strict,
            })
        });

    let chat_config = ChatConfig::new(state.config.model.clone())
        .with_temperature(request.temperature.unwrap_or(state.config.temperature));
    let chat_config = if let Some(max_tokens) = request.max_tokens.or(state.config.max_output_tokens)
    {
        chat_config.with_max_tokens(max_tokens)
    } else {
        chat_config
    };
    let chat_config = if let Some(schema) = effective_schema.clone() {
        chat_config.with_response_schema(schema)
    } else {
        chat_config
    };

    let completion_id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = unix_timestamp();
    let mut total_usage = TokenUsage::default();
    let mut iterations: u32 = 0;
    let mut ask_agent_calls: u32 = 0;
    let mut final_text = String::new();
    let mut finish_reason = "stop".to_string();

    loop {
        let response = state
            .llm
            .chat(conv.messages(), &llm_tools, &chat_config)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("{}", e)))?;

        total_usage.accumulate(&response.usage);

        conv.push_assistant(&response);

        match response.stop_reason {
            StopReason::EndTurn => {
                final_text = response.content.clone();
                finish_reason = "stop".to_string();
                break;
            }
            StopReason::MaxTokens => {
                final_text = response.content.clone();
                finish_reason = "length".to_string();
                break;
            }
            StopReason::ToolUse => {
                let tool_calls = &response.tool_calls;
                let results = execute_tools_parallel(
                    &state.mcp_manager,
                    state.collaboration.as_ref(),
                    tool_calls,
                    state.config.tool_timeout,
                    incoming_depth,
                    &incoming_chain,
                    &mut ask_agent_calls,
                )
                .await;

                for (call, result) in tool_calls.iter().zip(results.iter()) {
                    if call.name == "recall_memories" {
                        state.recall_total.fetch_add(1, Ordering::Relaxed);
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&result.content) {
                            if v.as_array().map_or(false, |a| !a.is_empty()) {
                                state.recall_hits.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }

                conv.push_tool_results(results);
            }
            _ => {
                tracing::warn!(reason = ?response.stop_reason, "Unexpected stop reason");
                final_text = response.content.clone();
                finish_reason = "stop".to_string();
                break;
            }
        }

        iterations += 1;
        if iterations >= state.config.max_iterations {
            tracing::warn!(
                session_id = %session_id,
                iterations = iterations,
                "ReAct loop hit max iterations"
            );
            let text = conv.last_assistant_text();
            final_text = if text.is_empty() {
                "Maximum iterations reached.".to_string()
            } else {
                text
            };
            finish_reason = "stop".to_string();
            break;
        }

        if state.shutdown_token.is_cancelled() {
            tracing::warn!(
                session_id = %session_id,
                iterations = iterations,
                "Aborting ReAct loop due to shutdown"
            );
            let text = conv.last_assistant_text();
            final_text = if text.is_empty() {
                "Server is shutting down.".to_string()
            } else {
                text
            };
            finish_reason = "stop".to_string();
            break;
        }
    }

    if effective_schema.is_some() && !final_text.is_empty() {
        if serde_json::from_str::<serde_json::Value>(&final_text).is_err() {
            if finish_reason == "length" {
                return Err(ApiError::StructuredOutputTruncated);
            }
            return Err(ApiError::Internal(anyhow::anyhow!(
                "LLM produced invalid JSON for structured output: {}",
                final_text
            )));
        }
    }

    conv.record_usage(total_usage.input_tokens, total_usage.output_tokens);
    state.queries_today.fetch_add(1, Ordering::Relaxed);
    state
        .tokens_input_today
        .fetch_add(total_usage.input_tokens, Ordering::Relaxed);
    state
        .tokens_output_today
        .fetch_add(total_usage.output_tokens, Ordering::Relaxed);

    {
        let mut tracker = state.cost_tracker.lock().await;
        tracker.record(&total_usage, &session_id.to_string());
    }

    tracing::info!(
        session_id = %session_id,
        input_tokens = total_usage.input_tokens,
        output_tokens = total_usage.output_tokens,
        iterations = iterations,
        "Chat completion finished"
    );

    if request.stream {
        Ok(build_sse_response(
            completion_id,
            created,
            state.config.model.clone(),
            final_text,
            finish_reason,
            total_usage,
            session_id,
        )
        .into_response())
    } else {
        let response = ChatCompletionResponse {
            id: completion_id,
            object: "chat.completion".to_string(),
            created,
            model: state.config.model.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: ChatRole::Assistant,
                    content: if final_text.is_empty() {
                        None
                    } else {
                        Some(final_text)
                    },
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    name: None,
                },
                finish_reason,
            }],
            usage: ChatUsage {
                prompt_tokens: total_usage.input_tokens,
                completion_tokens: total_usage.output_tokens,
                total_tokens: total_usage.input_tokens + total_usage.output_tokens,
            },
            session_id: Some(session_id),
        };
        Ok(Json(response).into_response())
    }
}

async fn execute_tools_parallel(
    mcp: &McpManager,
    collaboration: Option<&AgentCaller>,
    calls: &[ToolCall],
    timeout: std::time::Duration,
    current_depth: u32,
    current_chain: &[String],
    ask_agent_calls: &mut u32,
) -> Vec<ToolResult> {
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
            let mcp = mcp.clone();
            tokio::spawn(async move {
                let arguments = call.arguments.as_object().cloned();
                let result =
                    match tokio::time::timeout(timeout, mcp.call_tool(&call.name, arguments)).await
                    {
                        Ok(Ok(result)) => {
                            let text = crate::mcp::schema::tool_result_to_string(&result);
                            ToolResult::success(call.id.clone(), text)
                        }
                        Ok(Err(e)) => ToolResult::error(
                            call.id.clone(),
                            format!("Tool '{}' failed: {}", call.name, e),
                        ),
                        Err(_) => ToolResult::error(
                            call.id.clone(),
                            format!(
                                "Tool '{}' timed out after {}ms",
                                call.name,
                                timeout.as_millis()
                            ),
                        ),
                    };
                (idx, result)
            })
        })
        .collect();

    for handle in mcp_handles {
        match handle.await {
            Ok((idx, result)) => {
                results[idx] = Some(result);
            }
            Err(e) => {
                tracing::error!(error = %e, "Tool task panicked");
            }
        }
    }

    for (idx, call) in agent_calls {
        let max_per_turn = collaboration
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

        let result = if let Some(collab) = collaboration {
            match collab
                .call(agent_name, question, current_depth, current_chain)
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

// 6.2 SSE Streaming Implementation

fn build_sse_response(
    id: String,
    created: u64,
    model: String,
    text: String,
    finish_reason: String,
    usage: TokenUsage,
    session_id: Uuid,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(32);

    tokio::spawn(async move {
        let chunks = split_into_chunks(&text, 80);

        let first_chunk = ChatCompletionChunk {
            id: id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model.clone(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta {
                    role: Some(ChatRole::Assistant),
                    content: if chunks.is_empty() {
                        Some(String::new())
                    } else {
                        None
                    },
                },
                finish_reason: if chunks.is_empty() {
                    Some(finish_reason.clone())
                } else {
                    None
                },
            }],
            usage: None,
            session_id: Some(session_id),
        };
        let data = serde_json::to_string(&first_chunk).unwrap_or_default();
        if tx.send(Ok(Event::default().data(data))).await.is_err() {
            return;
        }

        for (i, chunk_text) in chunks.iter().enumerate() {
            let is_last = i == chunks.len() - 1;
            let chunk = ChatCompletionChunk {
                id: id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model.clone(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: Some(chunk_text.clone()),
                    },
                    finish_reason: if is_last {
                        Some(finish_reason.clone())
                    } else {
                        None
                    },
                }],
                usage: None,
                session_id: None,
            };
            let data = serde_json::to_string(&chunk).unwrap_or_default();
            if tx.send(Ok(Event::default().data(data))).await.is_err() {
                return;
            }
        }

        let usage_chunk = ChatCompletionChunk {
            id: id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model.clone(),
            choices: Vec::new(),
            usage: Some(ChatUsage {
                prompt_tokens: usage.input_tokens,
                completion_tokens: usage.output_tokens,
                total_tokens: usage.input_tokens + usage.output_tokens,
            }),
            session_id: None,
        };
        let data = serde_json::to_string(&usage_chunk).unwrap_or_default();
        let _ = tx.send(Ok(Event::default().data(data))).await;

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let stream = ReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn split_into_chunks(text: &str, target_size: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= target_size {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        if start + target_size >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }

        // Find the nearest char boundary at or before start + target_size
        let end = {
            let candidate = start + target_size;
            if text.is_char_boundary(candidate) {
                candidate
            } else {
                text[start..].char_indices()
                    .take_while(|(i, _)| *i <= target_size)
                    .last()
                    .map(|(i, c)| start + i + c.len_utf8())
                    .unwrap_or(text.len())
            }
        };

        let search_slice = &text[start..end];
        let break_at = search_slice
            .rfind(' ')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

// 6.3 GET /health

async fn handle_health(
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<HealthResponse>), ApiError> {
    let health_results = state.mcp_manager.health_check().await;

    let mut mcp_statuses = HashMap::new();
    let mut any_required_down = false;
    let mut any_optional_down = false;

    for (name, config) in &state.config.mcp_servers {
        let (status, tool_count) = match health_results.get(name) {
            Some(Ok(count)) => ("connected".to_string(), *count),
            Some(Err(_)) => {
                if config.required {
                    any_required_down = true;
                } else {
                    any_optional_down = true;
                }
                ("disconnected".to_string(), 0)
            }
            None => {
                if config.required {
                    any_required_down = true;
                } else {
                    any_optional_down = true;
                }
                ("disconnected".to_string(), 0)
            }
        };
        mcp_statuses.insert(
            name.clone(),
            McpServerHealth {
                status,
                tools: tool_count,
            },
        );
    }

    let overall_status = if any_required_down {
        "unhealthy"
    } else if any_optional_down {
        "degraded"
    } else {
        "healthy"
    };

    let uptime = state.started_at.elapsed().as_secs();

    let recall_total = state.recall_total.load(Ordering::Relaxed);
    let recall_hits = state.recall_hits.load(Ordering::Relaxed);
    let hit_rate = if recall_total == 0 {
        -1.0
    } else {
        recall_hits as f64 / recall_total as f64
    };

    let total_memories: i64 = -1;

    let cost_usd = {
        let tracker = state.cost_tracker.lock().await;
        tracker.estimated_cost_usd()
    };

    let response = HealthResponse {
        status: overall_status.to_string(),
        uptime_seconds: uptime,
        model: state.config.model.clone(),
        mcp_servers: mcp_statuses,
        memory: MemoryStats {
            total_memories,
            recall_hit_rate: hit_rate,
        },
        usage_today: UsageStats {
            queries: state.queries_today.load(Ordering::Relaxed),
            tokens_input: state.tokens_input_today.load(Ordering::Relaxed),
            tokens_output: state.tokens_output_today.load(Ordering::Relaxed),
            cost_usd,
        },
    };

    let status_code = if any_required_down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    Ok((status_code, Json(response)))
}

// 6.4 POST /v1/sessions

async fn handle_create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>), ApiError> {
    if state.is_shutting_down.load(Ordering::SeqCst) {
        return Err(ApiError::ShuttingDown);
    }

    let session_id = Uuid::new_v4();

    let system_prompt = request
        .system_prompt_override
        .unwrap_or_else(|| state.system_prompt.clone());

    let conv = ConversationManager::new(system_prompt, session_id);

    let response = SessionResponse {
        session_id,
        created_at: unix_timestamp(),
        message_count: conv.messages().len(),
        total_input_tokens: 0,
        total_output_tokens: 0,
        model: state.config.model.clone(),
    };

    state
        .sessions
        .insert(session_id, Arc::new(tokio::sync::Mutex::new(conv)));

    tracing::info!(session_id = %session_id, "Session created");

    Ok((StatusCode::CREATED, Json(response)))
}

// 6.5 GET /v1/sessions/:id

async fn handle_get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionResponse>, ApiError> {
    let session = state
        .sessions
        .get(&id)
        .ok_or(ApiError::SessionNotFound(id))?;

    let conv = session.value().lock().await;

    let response = SessionResponse {
        session_id: id,
        created_at: unix_timestamp(),
        message_count: conv.messages().len(),
        total_input_tokens: conv.total_input_tokens(),
        total_output_tokens: conv.total_output_tokens(),
        model: state.config.model.clone(),
    };

    Ok(Json(response))
}

// 6.6 DELETE /v1/sessions/:id

async fn handle_delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeleteSessionResponse>, ApiError> {
    let removed = state.sessions.remove(&id);

    if removed.is_none() {
        return Err(ApiError::SessionNotFound(id));
    }

    if let Some((_, session)) = removed {
        if let Ok(conv) = session.try_lock() {
            tracing::info!(
                session_id = %id,
                message_count = conv.messages().len(),
                input_tokens = conv.total_input_tokens(),
                output_tokens = conv.total_output_tokens(),
                "Session deleted"
            );
        } else {
            tracing::info!(session_id = %id, "Session deleted (in use, stats unavailable)");
        }
    }

    Ok(Json(DeleteSessionResponse {
        deleted: true,
        session_id: id,
    }))
}

// 6.7 GET /metrics

async fn handle_metrics() -> (StatusCode, &'static str) {
    (
        StatusCode::NOT_IMPLEMENTED,
        "# Prometheus metrics endpoint (available in Phase 2)\n",
    )
}

// ---------------------------------------------------------------------------
// 12. Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_empty_text() {
        let chunks = split_into_chunks("", 80);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_split_short_text() {
        let chunks = split_into_chunks("Hello world", 80);
        assert_eq!(chunks, vec!["Hello world"]);
    }

    #[test]
    fn test_split_exact_boundary() {
        let text = "Hello world this is a test of the chunking system here";
        let chunks = split_into_chunks(text, 12);
        assert!(chunks.len() > 1);
        let rejoined: String = chunks.into_iter().collect();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn test_split_preserves_all_text() {
        let text = "The quick brown fox jumps over the lazy dog. This is a longer text that should be split into multiple chunks for streaming to the client.";
        let chunks = split_into_chunks(text, 40);
        let rejoined: String = chunks.into_iter().collect();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn test_split_no_spaces() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let chunks = split_into_chunks(text, 10);
        let rejoined: String = chunks.into_iter().collect();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn test_chat_message_to_internal_user() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: Some("Hello".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        };
        let internal = chat_message_to_internal(&msg);
        assert!(matches!(&internal, Message::User { content } if content == "Hello"));
    }

    #[test]
    fn test_chat_message_to_internal_system() {
        let msg = ChatMessage {
            role: ChatRole::System,
            content: Some("You are helpful.".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        };
        let internal = chat_message_to_internal(&msg);
        assert!(
            matches!(&internal, Message::System { content } if content == "You are helpful.")
        );
    }

    #[test]
    fn test_chat_message_to_internal_missing_content() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        };
        let internal = chat_message_to_internal(&msg);
        assert!(matches!(&internal, Message::User { content } if content.is_empty()));
    }

    #[test]
    fn test_chat_message_to_internal_tool() {
        let msg = ChatMessage {
            role: ChatRole::Tool,
            content: Some("{\"result\": 42}".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: Some("call_123".to_string()),
            name: Some("calculator".to_string()),
        };
        let internal = chat_message_to_internal(&msg);
        assert!(matches!(
            &internal,
            Message::ToolResult {
                tool_call_id,
                content,
                is_error: false,
            }
            if tool_call_id == "call_123" && content == "{\"result\": 42}"
        ));
    }

    #[test]
    fn test_internal_to_chat_message_roundtrip() {
        let original = ChatMessage {
            role: ChatRole::Assistant,
            content: Some("Here is the answer.".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        };
        let internal = chat_message_to_internal(&original);
        let back = internal_to_chat_message(&internal);
        assert_eq!(back.role, ChatRole::Assistant);
        assert_eq!(back.content, Some("Here is the answer.".to_string()));
    }

    #[test]
    fn test_api_error_status_codes() {
        assert_eq!(
            ApiError::BadRequest("test".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::SessionNotFound(Uuid::new_v4()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::ShuttingDown.status_code(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError::RateLimited {
                retry_after_secs: 60
            }
            .status_code(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn test_api_error_serialization() {
        let error = ApiError::EmptyMessages;
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                message: error.to_string(),
                error_type: error.error_type().to_string(),
                code: error.error_code().to_string(),
            },
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["code"], "empty_messages");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("empty"));
    }

    #[test]
    fn test_deserialize_minimal_request() {
        let json = r#"{
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(req.model.is_none());
        assert!(!req.stream);
        assert!(req.temperature.is_none());
        assert!(req.session_id.is_none());
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_deserialize_full_request() {
        let json = r#"{
            "model": "claude-haiku-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true,
            "temperature": 0.5,
            "max_tokens": 1000,
            "session_id": "550e8400-e29b-41d4-a716-446655440000"
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, Some("claude-haiku-4".to_string()));
        assert!(req.stream);
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.max_tokens, Some(1000));
        assert!(req.session_id.is_some());
    }

    #[test]
    fn test_deserialize_request_ignores_unknown_fields() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Hello"}],
            "top_p": 0.9,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.3,
            "n": 1,
            "logprobs": false
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_serialize_chat_completion_response() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-test-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1719331200,
            model: "claude-haiku-4".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: ChatRole::Assistant,
                    content: Some("Hello!".to_string()),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    name: None,
                },
                finish_reason: "stop".to_string(),
            }],
            usage: ChatUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            },
            session_id: None,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["total_tokens"], 150);
        assert!(json.get("session_id").is_none());
    }

    #[test]
    fn test_serialize_chunk_first() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1719331200,
            model: "claude-haiku-4".to_string(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta {
                    role: Some(ChatRole::Assistant),
                    content: None,
                },
                finish_reason: None,
            }],
            usage: None,
            session_id: Some(Uuid::nil()),
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["choices"][0]["delta"]["role"], "assistant");
        assert!(json["choices"][0]["delta"].get("content").is_none());
        assert!(json["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_serialize_chunk_content() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1719331200,
            model: "claude-haiku-4".to_string(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatChunkDelta {
                    role: None,
                    content: Some("Hello ".to_string()),
                },
                finish_reason: None,
            }],
            usage: None,
            session_id: None,
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert!(json["choices"][0]["delta"].get("role").is_none());
        assert_eq!(json["choices"][0]["delta"]["content"], "Hello ");
    }

    #[test]
    fn test_serialize_health_response() {
        let mut servers = HashMap::new();
        servers.insert(
            "recalld".to_string(),
            McpServerHealth {
                status: "connected".to_string(),
                tools: 6,
            },
        );
        let response = HealthResponse {
            status: "healthy".to_string(),
            uptime_seconds: 3600,
            model: "claude-haiku-4".to_string(),
            mcp_servers: servers,
            memory: MemoryStats {
                total_memories: 87,
                recall_hit_rate: 0.84,
            },
            usage_today: UsageStats {
                queries: 10,
                tokens_input: 5000,
                tokens_output: 2000,
                cost_usd: 0.15,
            },
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["mcp_servers"]["recalld"]["tools"], 6);
        assert_eq!(json["memory"]["total_memories"], 87);
    }

    #[test]
    fn test_serialize_session_response() {
        let response = SessionResponse {
            session_id: Uuid::nil(),
            created_at: 1719331200,
            message_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model: "claude-haiku-4".to_string(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(
            json["session_id"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(json["message_count"], 0);
    }

    #[test]
    fn test_serialize_delete_session_response() {
        let response = DeleteSessionResponse {
            deleted: true,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["deleted"], true);
    }

    #[test]
    fn test_deserialize_empty_create_session() {
        let json = "{}";
        let req: CreateSessionRequest = serde_json::from_str(json).unwrap();
        assert!(req.system_prompt_override.is_none());
        assert!(req.temperature.is_none());
    }

    #[test]
    fn test_deserialize_create_session_with_overrides() {
        let json = r#"{
            "system_prompt_override": "You are a test bot.",
            "temperature": 0.3
        }"#;
        let req: CreateSessionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.system_prompt_override,
            Some("You are a test bot.".to_string())
        );
        assert_eq!(req.temperature, Some(0.3));
    }
}
