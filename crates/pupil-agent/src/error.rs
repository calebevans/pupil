use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("configuration file not found: {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("invalid configuration: {message}")]
    ConfigInvalid { message: String },

    #[error("missing environment variable: {name}")]
    EnvVarMissing { name: String },

    #[error("failed to start MCP server '{server_name}': {source}")]
    McpServerStart {
        server_name: String,
        source: std::io::Error,
    },

    #[error("required MCP server '{server_name}' is unavailable: {reason}")]
    McpServerUnavailable {
        server_name: String,
        reason: String,
    },

    #[error("MCP tool call '{tool_name}' on server '{server_name}' failed: {message}")]
    McpToolCallFailed {
        tool_name: String,
        server_name: String,
        message: String,
    },

    #[error("unknown tool: '{tool_name}' (not found in any MCP server)")]
    UnknownTool { tool_name: String },

    #[error("LLM API error (HTTP {status}): {body}")]
    LlmApiError { status: u16, body: String },

    #[error("LLM API request timed out after {timeout_secs}s")]
    LlmTimeout { timeout_secs: u64 },

    #[error("cannot resolve LLM provider for model '{model}': no matching pattern")]
    LlmProviderResolution { model: String },

    #[error("LLM authentication failed for provider '{provider}': {message}")]
    LlmAuthError { provider: String, message: String },

    #[error("LLM rate limit exceeded for provider '{provider}' after {retries} retries")]
    LlmRateLimited { provider: String, retries: u32 },

    #[cfg(feature = "learn")]
    #[error("failed to read curriculum source '{path}': {source}")]
    CurriculumReadError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[cfg(feature = "learn")]
    #[error("failed to extract content from '{path}': {message}")]
    CurriculumExtractError { path: PathBuf, message: String },

    #[cfg(feature = "learn")]
    #[error("failed to fetch curriculum URL '{url}': {message}")]
    CurriculumFetchError { url: String, message: String },

    #[cfg(feature = "learn")]
    #[error("build budget exceeded: spent ${actual:.2} of ${budget:.2} limit")]
    BudgetExceeded { budget: f64, actual: f64 },

    #[error("ReAct loop exceeded {max_iterations} iterations for this query")]
    MaxIterationsExceeded { max_iterations: u32 },

    #[error("tool '{tool_name}' timed out after {timeout_secs}s")]
    ToolTimeout {
        tool_name: String,
        timeout_secs: u64,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yml::Error),

    #[error("MCP error: {0}")]
    Mcp(#[from] crate::mcp::McpError),

    #[error("LLM error: {0}")]
    Llm(#[from] crate::llm::LlmError),

    #[error("MCP config error: {0}")]
    McpConfig(#[from] crate::mcp::config::ConfigError),
}

pub mod exit_code {
    pub const OK: i32 = 0;
    pub const CONFIG_ERROR: i32 = 1;
    pub const MCP_ERROR: i32 = 2;
    pub const LLM_ERROR: i32 = 3;
    pub const LEARN_ERROR: i32 = 4;
    pub const BUDGET_ERROR: i32 = 5;
    pub const INTERNAL_ERROR: i32 = 99;
}

impl AgentError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ConfigNotFound { .. }
            | Self::ConfigInvalid { .. }
            | Self::EnvVarMissing { .. } => exit_code::CONFIG_ERROR,

            Self::McpServerStart { .. }
            | Self::McpServerUnavailable { .. }
            | Self::McpToolCallFailed { .. }
            | Self::UnknownTool { .. } => exit_code::MCP_ERROR,

            Self::LlmApiError { .. }
            | Self::LlmTimeout { .. }
            | Self::LlmProviderResolution { .. }
            | Self::LlmAuthError { .. }
            | Self::LlmRateLimited { .. } => exit_code::LLM_ERROR,

            #[cfg(feature = "learn")]
            Self::CurriculumReadError { .. }
            | Self::CurriculumExtractError { .. }
            | Self::CurriculumFetchError { .. } => exit_code::LEARN_ERROR,

            #[cfg(feature = "learn")]
            Self::BudgetExceeded { .. } => exit_code::BUDGET_ERROR,

            Self::MaxIterationsExceeded { .. }
            | Self::ToolTimeout { .. } => exit_code::INTERNAL_ERROR,

            Self::Mcp(_) | Self::McpConfig(_) => exit_code::MCP_ERROR,

            Self::Llm(_) => exit_code::LLM_ERROR,

            Self::Io(_) | Self::Json(_) | Self::Yaml(_) => exit_code::INTERNAL_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_errors_return_code_1() {
        let err = AgentError::ConfigNotFound {
            path: PathBuf::from("/config/pupil.yaml"),
        };
        assert_eq!(err.exit_code(), exit_code::CONFIG_ERROR);
    }

    #[test]
    fn mcp_errors_return_code_2() {
        let err = AgentError::McpServerUnavailable {
            server_name: "recalld".into(),
            reason: "exited".into(),
        };
        assert_eq!(err.exit_code(), exit_code::MCP_ERROR);
    }

    #[test]
    fn llm_errors_return_code_3() {
        let err = AgentError::LlmApiError {
            status: 429,
            body: "rate limited".into(),
        };
        assert_eq!(err.exit_code(), exit_code::LLM_ERROR);
    }

    #[test]
    fn io_errors_return_internal_code() {
        let err = AgentError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert_eq!(err.exit_code(), exit_code::INTERNAL_ERROR);
    }
}
