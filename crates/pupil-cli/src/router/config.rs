use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    pub router: RouterSettings,
    pub agents: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterSettings {
    #[serde(default = "default_listen")]
    pub listen: String,

    #[serde(default = "default_strategy")]
    pub strategy: RoutingStrategyConfig,

    #[serde(default = "default_fallback")]
    pub fallback: String,

    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,

    #[serde(default)]
    pub hybrid: HybridConfig,

    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,

    #[serde(default)]
    pub classifier: Option<ClassifierConfig>,

    #[serde(default)]
    pub health_check: HealthCheckConfig,

    #[serde(default = "default_true")]
    pub session_affinity: bool,

    #[serde(default = "default_session_ttl")]
    pub session_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoutingStrategyConfig {
    Keyword,
    Embedding,
    Llm,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridConfig {
    #[serde(default = "default_keyword_threshold")]
    pub keyword_threshold: f64,

    #[serde(default = "default_embedding_threshold")]
    pub embedding_threshold: f64,

    #[serde(default = "default_llm_threshold")]
    pub llm_threshold: f64,

    #[serde(default = "default_classifier_model")]
    pub classifier_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    #[serde(default = "default_embedding_provider")]
    pub provider: String,

    #[serde(default = "default_embedding_model")]
    pub model: String,

    #[serde(default = "default_ollama_url")]
    pub base_url: String,

    #[serde(default = "default_embedding_dims")]
    pub dimensions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierConfig {
    #[serde(default = "default_classifier_model")]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    #[serde(default = "default_health_interval")]
    pub interval_secs: u64,

    #[serde(default = "default_health_timeout")]
    pub timeout_secs: u64,

    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub url: String,

    #[serde(default)]
    pub description: String,

    #[serde(default)]
    pub topics: Vec<String>,

    #[serde(default)]
    pub exclusive_topics: Vec<String>,

    #[serde(default)]
    pub sample_questions: Vec<String>,

    #[serde(default)]
    pub priority: i32,
}

pub(crate) fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}
pub(crate) fn default_strategy() -> RoutingStrategyConfig {
    RoutingStrategyConfig::Hybrid
}
fn default_fallback() -> String {
    "error".to_string()
}
pub(crate) fn default_confidence_threshold() -> f64 {
    0.6
}
fn default_keyword_threshold() -> f64 {
    0.8
}
fn default_embedding_threshold() -> f64 {
    0.7
}
fn default_llm_threshold() -> f64 {
    0.5
}
pub(crate) fn default_classifier_model() -> String {
    "claude-haiku-4".to_string()
}
pub(crate) fn default_embedding_provider() -> String {
    "ollama".to_string()
}
pub(crate) fn default_embedding_model() -> String {
    "embeddinggemma".to_string()
}
pub(crate) fn default_ollama_url() -> String {
    "http://ollama:11434".to_string()
}
pub(crate) fn default_embedding_dims() -> usize {
    768
}
fn default_health_interval() -> u64 {
    30
}
fn default_health_timeout() -> u64 {
    5
}
fn default_unhealthy_threshold() -> u32 {
    3
}
fn default_healthy_threshold() -> u32 {
    1
}
fn default_true() -> bool {
    true
}
pub(crate) fn default_session_ttl() -> u64 {
    3600
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            keyword_threshold: default_keyword_threshold(),
            embedding_threshold: default_embedding_threshold(),
            llm_threshold: default_llm_threshold(),
            classifier_model: default_classifier_model(),
        }
    }
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_health_interval(),
            timeout_secs: default_health_timeout(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterConfigError {
    #[error("Cannot read config file at {path}: {source}")]
    IoError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Invalid YAML in {path}: {source}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_yml::Error,
    },

    #[error("Config validation error: {0}")]
    ValidationError(String),
}

pub fn load_router_config(
    path: &std::path::Path,
) -> Result<RouterConfig, RouterConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| RouterConfigError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let config: RouterConfig =
        serde_yml::from_str(&content).map_err(|e| RouterConfigError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;

    validate_config(&config)?;

    Ok(config)
}

pub fn validate_config(config: &RouterConfig) -> Result<(), RouterConfigError> {
    if config.agents.is_empty() {
        return Err(RouterConfigError::ValidationError(
            "At least one agent must be configured".to_string(),
        ));
    }

    let mut names = std::collections::HashSet::new();
    for agent in &config.agents {
        if !names.insert(&agent.name) {
            return Err(RouterConfigError::ValidationError(format!(
                "Duplicate agent name: {}",
                agent.name
            )));
        }
    }

    let t = config.router.confidence_threshold;
    if !(0.0..=1.0).contains(&t) {
        return Err(RouterConfigError::ValidationError(format!(
            "confidence_threshold must be between 0.0 and 1.0, got {t}"
        )));
    }

    let h = &config.router.hybrid;
    for (name, val) in [
        ("keyword_threshold", h.keyword_threshold),
        ("embedding_threshold", h.embedding_threshold),
        ("llm_threshold", h.llm_threshold),
    ] {
        if !(0.0..=1.0).contains(&val) {
            return Err(RouterConfigError::ValidationError(format!(
                "{name} must be between 0.0 and 1.0, got {val}"
            )));
        }
    }

    let needs_embedding = matches!(
        config.router.strategy,
        RoutingStrategyConfig::Embedding | RoutingStrategyConfig::Hybrid
    );
    if needs_embedding && config.router.embedding.is_none() {
        return Err(RouterConfigError::ValidationError(
            "Embedding or hybrid strategy requires an `embedding` config section".to_string(),
        ));
    }

    if config.router.strategy == RoutingStrategyConfig::Llm && config.router.classifier.is_none() {
        return Err(RouterConfigError::ValidationError(
            "LLM strategy requires a `classifier` config section".to_string(),
        ));
    }

    let fallback = &config.router.fallback;
    if fallback != "error" && fallback != "ask" {
        let exists = config.agents.iter().any(|a| &a.name == fallback);
        if !exists {
            return Err(RouterConfigError::ValidationError(format!(
                "Fallback agent '{}' not found in agents list. \
                 Use an agent name, \"error\", or \"ask\".",
                fallback
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config() -> RouterConfig {
        RouterConfig {
            router: RouterSettings {
                listen: "0.0.0.0:8080".to_string(),
                strategy: RoutingStrategyConfig::Keyword,
                fallback: "error".to_string(),
                confidence_threshold: 0.6,
                hybrid: HybridConfig::default(),
                embedding: None,
                classifier: None,
                health_check: HealthCheckConfig::default(),
                session_affinity: true,
                session_ttl_secs: 3600,
            },
            agents: vec![AgentConfig {
                name: "agent-1".to_string(),
                url: "http://localhost:8081".to_string(),
                description: "Test agent".to_string(),
                topics: vec!["test".to_string()],
                exclusive_topics: vec![],
                sample_questions: vec![],
                priority: 0,
            }],
        }
    }

    #[test]
    fn valid_minimal_config() {
        let config = minimal_config();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn no_agents_is_error() {
        let mut config = minimal_config();
        config.agents.clear();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn duplicate_agent_names_is_error() {
        let mut config = minimal_config();
        config.agents.push(config.agents[0].clone());
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn confidence_threshold_out_of_range() {
        let mut config = minimal_config();
        config.router.confidence_threshold = 1.5;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn embedding_strategy_requires_embedding_config() {
        let mut config = minimal_config();
        config.router.strategy = RoutingStrategyConfig::Embedding;
        config.router.embedding = None;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn fallback_agent_must_exist() {
        let mut config = minimal_config();
        config.router.fallback = "nonexistent".to_string();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn error_and_ask_are_valid_fallbacks() {
        let mut config = minimal_config();
        config.router.fallback = "error".to_string();
        assert!(validate_config(&config).is_ok());

        config.router.fallback = "ask".to_string();
        assert!(validate_config(&config).is_ok());
    }
}
