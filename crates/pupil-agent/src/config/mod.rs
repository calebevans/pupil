use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

pub use crate::mcp::config::McpServerConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub name: String,

    #[serde(default)]
    pub description: String,

    #[serde(default)]
    pub model: String,

    #[serde(default)]
    pub learning_model: Option<String>,

    #[serde(default)]
    pub fallback_model: Option<String>,

    #[serde(default)]
    pub system_prompt: String,

    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,

    #[serde(default)]
    pub curriculum: Option<CurriculumConfig>,

    #[serde(default)]
    pub build: Option<BuildConfig>,

    #[serde(default)]
    pub runtime: Option<RuntimeConfig>,

    #[serde(default)]
    pub pricing: HashMap<String, PricingOverride>,

    #[serde(default)]
    pub routing: Option<RoutingConfig>,

    #[serde(default)]
    pub audit: Option<AuditConfig>,

    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,

    #[serde(
        default = "default_tool_timeout_secs",
        deserialize_with = "deserialize_duration_secs"
    )]
    pub tool_timeout: Duration,

    #[serde(default = "default_temperature")]
    pub temperature: f64,

    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

fn default_max_iterations() -> u32 {
    50
}

fn default_tool_timeout_secs() -> Duration {
    Duration::from_secs(30)
}

fn default_temperature() -> f64 {
    0.7
}

fn deserialize_duration_secs<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let secs = u64::deserialize(deserializer)?;
    Ok(Duration::from_secs(secs))
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurriculumConfig {
    #[serde(default)]
    pub sources: Vec<SourceEntry>,

    #[serde(default = "default_namespace")]
    pub namespace: String,

    #[serde(default)]
    pub decay: f64,

    #[serde(default)]
    pub learning_profile: Option<String>,

    #[serde(default)]
    pub sync: Option<SyncConfig>,
}

fn default_namespace() -> String {
    "knowledge".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SourceEntry {
    Short(String),
    Long(SourceConfig),
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    #[serde(default)]
    pub path: Option<String>,

    #[serde(default)]
    pub url: Option<String>,

    #[serde(default)]
    pub glob: Option<String>,

    #[serde(default)]
    pub learning_profile: Option<String>,

    #[serde(default)]
    pub learning_prompt: Option<String>,

    #[serde(default)]
    pub namespace: Option<String>,

    #[serde(default)]
    pub tags: Vec<String>,

    #[serde(default)]
    pub decay: Option<f64>,

    #[serde(default)]
    pub sync: Option<SourceSyncConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_sync_interval")]
    pub interval: String,

    #[serde(default = "default_on_change")]
    pub on_change: String,

    #[serde(default)]
    pub post_test: bool,

    #[serde(default = "default_test_file")]
    pub test_file: String,

    #[serde(default = "default_concurrency")]
    pub concurrency: u32,

    #[serde(default = "default_request_delay")]
    pub request_delay_ms: u64,

    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(default = "default_user_agent")]
    pub user_agent: String,

    #[serde(default = "default_true")]
    pub respect_robots_txt: bool,
}

fn default_true() -> bool {
    true
}
fn default_sync_interval() -> String {
    "6h".to_string()
}
fn default_on_change() -> String {
    "in_place".to_string()
}
fn default_test_file() -> String {
    "tests.yaml".to_string()
}
fn default_concurrency() -> u32 {
    4
}
fn default_request_delay() -> u64 {
    500
}
fn default_timeout_secs() -> u64 {
    30
}
fn default_user_agent() -> String {
    "PupilBot/1.0 (+https://github.com/calebevans/pupil)".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceSyncConfig {
    #[serde(default)]
    pub enabled: Option<bool>,

    #[serde(default)]
    pub interval: Option<String>,

    #[serde(default)]
    pub on_change: Option<String>,

    #[serde(default)]
    pub auth: Option<AuthConfig>,

    #[serde(default)]
    pub strategy: Option<String>,

    #[serde(default)]
    pub webhook_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,

    #[serde(default)]
    pub token: Option<String>,

    #[serde(default)]
    pub username: Option<String>,

    #[serde(default)]
    pub password: Option<String>,

    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildConfig {
    #[serde(default)]
    pub max_cost_usd: Option<f64>,

    #[serde(default = "default_on_budget_exceeded")]
    pub on_budget_exceeded: String,

    #[serde(default)]
    pub self_test: Option<SelfTestConfig>,
}

fn default_on_budget_exceeded() -> String {
    "abort".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelfTestConfig {
    #[serde(default = "default_test_file")]
    pub file: String,

    #[serde(default = "default_min_score")]
    pub min_score: f64,

    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Model identifier for the LLM judge. If omitted, the agent's own
    /// `model` field is used. Accepts the same format as the top-level
    /// `model` field (e.g. "claude-sonnet-4-6", "vertex/gemini-2.5-flash").
    #[serde(default)]
    pub judge_model: Option<String>,
}

fn default_min_score() -> f64 {
    0.8
}
fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub max_tokens_per_query: Option<u64>,

    #[serde(default)]
    pub max_cost_per_day_usd: Option<f64>,

    #[serde(default = "default_runtime_budget_action")]
    pub on_budget_exceeded: String,
}

fn default_runtime_budget_action() -> String {
    "warn".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PricingOverride {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub log_queries: bool,

    #[serde(default = "default_true")]
    pub log_responses: bool,

    #[serde(default = "default_true")]
    pub log_memories: bool,

    #[serde(default)]
    pub redact_patterns: Vec<String>,

    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

fn default_retention_days() -> u32 {
    90
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Config file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("Failed to read config file '{path}': {source}")]
    IoError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse pupil.yaml: {source}")]
    ParseError {
        #[source]
        source: serde_yml::Error,
    },

    #[error("Agent name is required in pupil.yaml")]
    MissingName,

    #[error(
        "Agent name '{name}' is invalid. Must match [a-z0-9][a-z0-9-]*[a-z0-9] \
         and be 2-64 characters."
    )]
    InvalidName { name: String },

    #[error("Model is required in pupil.yaml")]
    MissingModel,

    #[error(
        "Source '{source_id}' has both learning_profile and learning_prompt. \
         These are mutually exclusive."
    )]
    MutuallyExclusivePrompt { source_id: String },

    #[error("Source entry has both 'path' and 'url'. Use one or the other.")]
    AmbiguousSource,

    #[error("Source entry must have at least one of 'path', 'url', or 'glob'.")]
    EmptySource,

    #[error(
        "Unknown learning profile '{profile}'. \
         Available: general, reference, procedural, conceptual, faq, policy, code"
    )]
    UnknownProfile { profile: String },

    #[error("Sync interval '{interval}' is below the 5-minute minimum.")]
    SyncIntervalTooShort { interval: String },

    #[error(
        "Invalid sync interval '{interval}'. \
         Use format like '30m', '1h', '6h', '1d'."
    )]
    InvalidSyncInterval { interval: String },

    #[error("Temperature {value} is out of range. Must be 0.0 to 2.0.")]
    InvalidTemperature { value: f64 },

    #[error("max_iterations must be at least 1.")]
    InvalidMaxIterations,

    #[error("Environment variable '{var}' referenced in config but not set.")]
    MissingEnvVar { var: String },
}

const KNOWN_PROFILES: &[&str] = &[
    "general",
    "reference",
    "procedural",
    "conceptual",
    "faq",
    "policy",
    "code",
];

const NAME_PATTERN: &str = r"^[a-z0-9][a-z0-9-]*[a-z0-9]$";

static NAME_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(NAME_PATTERN).expect("valid regex"));

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::NotFound {
                path: path.to_path_buf(),
            });
        }

        let raw = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConfigError::NotFound {
                    path: path.to_path_buf(),
                }
            } else {
                ConfigError::IoError {
                    path: path.to_path_buf(),
                    source: e,
                }
            }
        })?;

        let substituted = substitute_env_vars(&raw)?;

        let config: AgentConfig =
            serde_yml::from_str(&substituted).map_err(|e| ConfigError::ParseError { source: e })?;

        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.name.is_empty() {
            return Err(ConfigError::MissingName);
        }

        if !NAME_RE.is_match(&self.name) || self.name.len() < 2 || self.name.len() > 64 {
            return Err(ConfigError::InvalidName {
                name: self.name.clone(),
            });
        }

        if self.model.is_empty() {
            return Err(ConfigError::MissingModel);
        }

        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(ConfigError::InvalidTemperature {
                value: self.temperature,
            });
        }

        if self.max_iterations < 1 {
            return Err(ConfigError::InvalidMaxIterations);
        }

        if let Some(ref curriculum) = self.curriculum {
            if let Some(ref profile) = curriculum.learning_profile {
                if !KNOWN_PROFILES.contains(&profile.as_str()) {
                    return Err(ConfigError::UnknownProfile {
                        profile: profile.clone(),
                    });
                }
            }

            for source in &curriculum.sources {
                if let SourceEntry::Long(sc) = source {
                    let has_path = sc.path.is_some();
                    let has_url = sc.url.is_some();
                    let has_glob = sc.glob.is_some();

                    if has_path && has_url {
                        return Err(ConfigError::AmbiguousSource);
                    }

                    if !has_path && !has_url && !has_glob {
                        return Err(ConfigError::EmptySource);
                    }

                    if sc.learning_profile.is_some() && sc.learning_prompt.is_some() {
                        let id = sc
                            .path
                            .as_deref()
                            .or(sc.url.as_deref())
                            .or(sc.glob.as_deref())
                            .unwrap_or("unknown");
                        return Err(ConfigError::MutuallyExclusivePrompt {
                            source_id: id.to_string(),
                        });
                    }

                    if let Some(ref profile) = sc.learning_profile {
                        if !KNOWN_PROFILES.contains(&profile.as_str()) {
                            return Err(ConfigError::UnknownProfile {
                                profile: profile.clone(),
                            });
                        }
                    }

                    if let Some(ref sync) = sc.sync {
                        if let Some(ref interval) = sync.interval {
                            validate_sync_interval(interval)?;
                        }
                    }
                }
            }

            if let Some(ref sync) = curriculum.sync {
                validate_sync_interval(&sync.interval)?;
            }
        }

        Ok(())
    }

    pub fn effective_learning_model(&self) -> &str {
        self.learning_model.as_deref().unwrap_or(&self.model)
    }

    pub fn max_tokens_per_query(&self) -> Option<u64> {
        self.runtime.as_ref().and_then(|r| r.max_tokens_per_query)
    }

    pub fn build_max_cost_usd(&self) -> Option<f64> {
        self.build.as_ref().and_then(|b| b.max_cost_usd)
    }
}

fn substitute_env_vars(input: &str) -> Result<String, ConfigError> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            if chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                let mut found_close = false;
                for inner in chars.by_ref() {
                    if inner == '}' {
                        found_close = true;
                        break;
                    }
                    var_name.push(inner);
                }
                if found_close {
                    let value = std::env::var(&var_name).map_err(|_| {
                        ConfigError::MissingEnvVar {
                            var: var_name.clone(),
                        }
                    })?;
                    result.push_str(&value);
                } else {
                    result.push('$');
                    result.push('{');
                    result.push_str(&var_name);
                }
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

fn validate_sync_interval(interval: &str) -> Result<Duration, ConfigError> {
    let interval = interval.trim();
    if interval.is_empty() {
        return Err(ConfigError::InvalidSyncInterval {
            interval: interval.to_string(),
        });
    }

    let (num_str, suffix) = interval.split_at(interval.len() - 1);
    let num: u64 = num_str.parse().map_err(|_| ConfigError::InvalidSyncInterval {
        interval: interval.to_string(),
    })?;

    let duration = match suffix {
        "m" => Duration::from_secs(num * 60),
        "h" => Duration::from_secs(num * 3600),
        "d" => Duration::from_secs(num * 86400),
        _ => {
            return Err(ConfigError::InvalidSyncInterval {
                interval: interval.to_string(),
            });
        }
    };

    if duration < Duration::from_secs(300) {
        return Err(ConfigError::SyncIntervalTooShort {
            interval: interval.to_string(),
        });
    }

    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn load_yaml(yaml: &str) -> Result<AgentConfig, ConfigError> {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();
        AgentConfig::load(file.path())
    }

    #[test]
    fn test_minimal_valid_config() {
        let config = load_yaml(
            r#"
name: my-agent
model: claude-haiku-4
"#,
        )
        .unwrap();
        assert_eq!(config.name, "my-agent");
        assert_eq!(config.model, "claude-haiku-4");
        assert_eq!(config.max_iterations, 50);
        assert_eq!(config.tool_timeout, Duration::from_secs(30));
        assert!((config.temperature - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_missing_name() {
        let err = load_yaml("model: claude-haiku-4\n").unwrap_err();
        assert!(matches!(err, ConfigError::MissingName));
    }

    #[test]
    fn test_invalid_name_uppercase() {
        let err = load_yaml("name: MyAgent\nmodel: x\n").unwrap_err();
        assert!(matches!(err, ConfigError::InvalidName { .. }));
    }

    #[test]
    fn test_invalid_name_too_short() {
        let err = load_yaml("name: a\nmodel: x\n").unwrap_err();
        assert!(matches!(err, ConfigError::InvalidName { .. }));
    }

    #[test]
    fn test_missing_model() {
        let err = load_yaml("name: my-agent\n").unwrap_err();
        assert!(matches!(err, ConfigError::MissingModel));
    }

    #[test]
    fn test_mutually_exclusive_prompt_and_profile() {
        let err = load_yaml(
            r#"
name: my-agent
model: claude-haiku-4
curriculum:
  namespace: knowledge
  sources:
    - path: ./docs/
      learning_profile: reference
      learning_prompt: "custom instructions"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::MutuallyExclusivePrompt { .. }));
    }

    #[test]
    fn test_unknown_profile() {
        let err = load_yaml(
            r#"
name: my-agent
model: claude-haiku-4
curriculum:
  namespace: knowledge
  sources:
    - path: ./docs/
      learning_profile: nonexistent
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProfile { .. }));
    }

    #[test]
    fn test_sync_interval_too_short() {
        let err = validate_sync_interval("2m").unwrap_err();
        assert!(matches!(err, ConfigError::SyncIntervalTooShort { .. }));
    }

    #[test]
    fn test_sync_interval_valid() {
        assert_eq!(
            validate_sync_interval("30m").unwrap(),
            Duration::from_secs(1800)
        );
        assert_eq!(
            validate_sync_interval("6h").unwrap(),
            Duration::from_secs(21600)
        );
        assert_eq!(
            validate_sync_interval("1d").unwrap(),
            Duration::from_secs(86400)
        );
    }

    #[test]
    fn test_env_var_substitution() {
        unsafe { std::env::set_var("TEST_PUPIL_VAR", "hello") };
        let result = substitute_env_vars("key: ${TEST_PUPIL_VAR}").unwrap();
        assert_eq!(result, "key: hello");
        unsafe { std::env::remove_var("TEST_PUPIL_VAR") };
    }

    #[test]
    fn test_env_var_missing() {
        let err = substitute_env_vars("key: ${DEFINITELY_NOT_SET_XYZ123}").unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar { .. }));
    }

    #[test]
    fn test_temperature_out_of_range() {
        let err = load_yaml("name: my-agent\nmodel: x\ntemperature: 3.0\n").unwrap_err();
        assert!(matches!(err, ConfigError::InvalidTemperature { .. }));
    }

    #[test]
    fn test_full_config_round_trip() {
        let config = load_yaml(
            r#"
name: onboarding-bot
description: "Onboarding assistant"
model: claude-haiku-4
learning_model: claude-sonnet-4-6
system_prompt: |
  You are an onboarding assistant.
max_iterations: 100
temperature: 0.3
mcp_servers:
  recalld:
    command: recalld
    args: ["mcp"]
    required: true
    env:
      RECALLD_DATA_DIR: /data/recalld
curriculum:
  namespace: knowledge
  decay: 0.0
  sources:
    - ./curriculum/
build:
  max_cost_usd: 10.0
  on_budget_exceeded: warn
  self_test:
    file: tests.yaml
    min_score: 0.85
    max_retries: 2
runtime:
  max_tokens_per_query: 16000
  max_cost_per_day_usd: 5.0
  on_budget_exceeded: degrade
"#,
        )
        .unwrap();

        assert_eq!(config.name, "onboarding-bot");
        assert_eq!(config.max_iterations, 100);
        assert_eq!(config.effective_learning_model(), "claude-sonnet-4-6");
        assert!(config.mcp_servers.get("recalld").unwrap().required);
        assert_eq!(
            config
                .build
                .as_ref()
                .unwrap()
                .self_test
                .as_ref()
                .unwrap()
                .max_retries,
            2
        );
    }
}
