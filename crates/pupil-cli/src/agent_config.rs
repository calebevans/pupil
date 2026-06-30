use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::CliError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub model: String,
    pub learning_model: Option<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub learning_prompt: Option<String>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerEntry>,
    #[serde(default)]
    pub curriculum: CurriculumConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub routing: Option<RoutingConfig>,
    #[serde(default)]
    pub pricing: HashMap<String, PricingOverride>,
    #[serde(default)]
    pub response_schema: Option<ResponseSchemaConfig>,
    #[serde(default)]
    pub collaboration: Option<CollaborationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSchemaConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub schema: serde_json::Value,
    #[serde(default = "default_true")]
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CurriculumConfig {
    #[serde(default)]
    pub sources: Vec<SourceEntry>,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default)]
    pub decay: f64,
    pub learning_profile: Option<String>,
    #[serde(default)]
    pub sync: Option<SyncConfig>,
}

fn default_namespace() -> String {
    "knowledge".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceEntry {
    Short(String),
    Long(SourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub path: Option<String>,
    pub url: Option<String>,
    pub glob: Option<String>,
    pub learning_profile: Option<String>,
    pub learning_prompt: Option<String>,
    pub namespace: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub decay: Option<f64>,
    #[serde(default)]
    pub sync: Option<SourceSyncConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServerEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub tools: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildConfig {
    pub max_cost_usd: Option<f64>,
    #[serde(default = "default_on_budget_exceeded")]
    pub on_budget_exceeded: String,
    pub self_test: Option<SelfTestConfig>,
}

fn default_on_budget_exceeded() -> String {
    "confirm".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfTestConfig {
    pub file: String,
    pub min_score: f64,
    pub max_retries: u32,

    /// Model identifier for the LLM judge. If omitted, the agent's own
    /// `model` field is used.
    #[serde(default)]
    pub judge_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConfig {
    pub max_tokens_per_query: Option<u64>,
    pub max_cost_per_day_usd: Option<f64>,
    pub on_budget_exceeded: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub sample_questions: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub exclusive_topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingOverride {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollaborationConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_allowed_agents")]
    pub allowed_agents: AllowedAgents,

    #[serde(default = "default_max_depth")]
    pub max_depth: u32,

    #[serde(default = "default_collab_timeout")]
    pub timeout_secs: u64,

    #[serde(default = "default_max_calls_per_turn")]
    pub max_calls_per_turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedAgents {
    List(Vec<String>),
    All(String),
}

fn default_allowed_agents() -> AllowedAgents {
    AllowedAgents::All("all".to_string())
}
fn default_max_depth() -> u32 {
    3
}
fn default_collab_timeout() -> u64 {
    120
}
fn default_max_calls_per_turn() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncConfig {
    #[serde(default = "default_sync_interval")]
    pub interval: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_on_change")]
    pub on_change: String,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub request_delay_ms: Option<u64>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default = "default_true")]
    pub respect_robots_txt: bool,
}

fn default_sync_interval() -> String {
    "6h".to_string()
}
fn default_true() -> bool {
    true
}
fn default_on_change() -> String {
    "in_place".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceSyncConfig {
    pub enabled: Option<bool>,
    pub interval: Option<String>,
    pub strategy: Option<String>,
    pub auth: Option<AuthConfig>,
    pub webhook_secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

impl AgentConfig {
    pub fn load(dir: &Path) -> Result<Self, CliError> {
        let path = dir.join("pupil.yaml");
        if !path.exists() {
            return Err(CliError::ConfigNotFound { path });
        }
        let contents =
            std::fs::read_to_string(&path).map_err(|e| CliError::Io(e))?;
        serde_yml::from_str(&contents).map_err(|e| CliError::Yaml(e))
    }
}

pub const BASE_IMAGE: &str = "pupil-base:dev";

pub fn image_ref(name: &str, tag: Option<&str>) -> String {
    format!("pupil-agent-{}:{}", name, tag.unwrap_or("latest"))
}

pub fn resolve_api_key(model: &str) -> (String, bool) {
    let key_name = if model.starts_with("claude-") || model.starts_with("anthropic/") {
        "ANTHROPIC_API_KEY"
    } else if model.starts_with("gpt-") || model.starts_with("openai/") {
        "OPENAI_API_KEY"
    } else if model.starts_with("gemini-") || model.starts_with("google/") {
        "GOOGLE_API_KEY"
    } else if model.starts_with("ollama/") || model.starts_with("ollama:") {
        return ("OLLAMA_API_KEY".to_string(), true);
    } else if model.starts_with("bedrock/") {
        "AWS_ACCESS_KEY_ID"
    } else if model.starts_with("vertex/") {
        return ("VERTEX_API_KEY".to_string(), resolve_vertex_key().is_some());
    } else if model.starts_with("azure/") {
        "AZURE_OPENAI_API_KEY"
    } else if model.starts_with("openai-compat:") {
        return ("OPENAI_API_KEY".to_string(), true);
    } else {
        "OPENAI_API_KEY"
    };
    let is_set = std::env::var(key_name).is_ok();
    (key_name.to_string(), is_set)
}

/// Get a Vertex API key. Checks the env var first, then falls back to
/// `gcloud auth print-access-token`. Returns None if neither is available.
pub fn resolve_vertex_key() -> Option<String> {
    if let Ok(key) = std::env::var("VERTEX_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    gcloud_access_token()
}

/// Get a fresh access token from the gcloud CLI. Called each time a
/// container needs credentials so tokens don't expire mid-build.
pub fn gcloud_access_token() -> Option<String> {
    let output = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return None;
    }
    tracing::debug!("Obtained fresh Vertex API token from gcloud CLI");
    Some(token)
}

/// Resolve Vertex project ID and location from env vars or gcloud config.
/// Injects them into the provided env map.
pub fn resolve_vertex_env(env: &mut std::collections::HashMap<String, String>) {
    if let Some(token) = resolve_vertex_key() {
        env.insert("VERTEX_API_KEY".to_string(), token);
    }

    if std::env::var("VERTEX_PROJECT_ID").is_err() {
        if let Ok(output) = std::process::Command::new("gcloud")
            .args(["config", "get-value", "project"])
            .output()
        {
            if output.status.success() {
                let project = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !project.is_empty() {
                    env.insert("VERTEX_PROJECT_ID".to_string(), project);
                }
            }
        }
    } else {
        env.insert(
            "VERTEX_PROJECT_ID".to_string(),
            std::env::var("VERTEX_PROJECT_ID").unwrap(),
        );
    }

    let location = std::env::var("VERTEX_LOCATION")
        .unwrap_or_else(|_| "us-central1".to_string());
    env.insert("VERTEX_LOCATION".to_string(), location);
}

pub fn resolve_agent(name: Option<&str>) -> Result<(PathBuf, AgentConfig), CliError> {
    match name {
        Some(n) => {
            let as_path = PathBuf::from(n);
            let dir = if as_path.join("pupil.yaml").exists() {
                as_path
            } else {
                let cwd = std::env::current_dir()?;
                let sub = cwd.join(n);
                if sub.join("pupil.yaml").exists() {
                    sub
                } else if cwd.join("pupil.yaml").exists() {
                    cwd
                } else {
                    return Err(CliError::ConfigNotFound {
                        path: sub.join("pupil.yaml"),
                    });
                }
            };
            let config = AgentConfig::load(&dir)?;
            Ok((dir, config))
        }
        None => {
            let cwd = std::env::current_dir()?;
            if cwd.join("pupil.yaml").exists() {
                let config = AgentConfig::load(&cwd)?;
                Ok((cwd, config))
            } else {
                Err(CliError::ConfigNotFound {
                    path: cwd.join("pupil.yaml"),
                })
            }
        }
    }
}

pub fn lookup_pricing(model: &str) -> (f64, f64) {
    match model {
        m if m.starts_with("claude-sonnet-4") => (3.0, 15.0),
        m if m.starts_with("claude-opus-4") => (15.0, 75.0),
        m if m.starts_with("claude-haiku") => (0.80, 4.0),
        m if m.starts_with("gpt-4o-mini") => (0.15, 0.60),
        m if m.starts_with("gpt-4o") => (2.50, 10.0),
        m if m.starts_with("gpt-4") => (30.0, 60.0),
        m if m.starts_with("gemini-1.5-flash") => (0.075, 0.30),
        m if m.starts_with("gemini-1.5-pro") => (3.50, 10.50),
        m if m.starts_with("gemini-2") => (1.25, 10.0),
        m if m.starts_with("ollama") => (0.0, 0.0),
        _ => (3.0, 15.0),
    }
}
