pub mod tool;

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::config::{AllowedAgents, CollaborationConfig};

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRegistryEntry {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Error)]
pub enum CollaborationError {
    #[error("Unknown agent '{name}'. Available agents: {available}")]
    AgentNotFound { name: String, available: String },

    #[error("Agent '{name}' is not in this agent's allowed_agents list")]
    AgentNotAllowed { name: String },

    #[error("Depth limit exceeded (current={current}, max={max})")]
    DepthExceeded { current: u32, max: u32 },

    #[error("Loop detected: '{target}' is already in call chain [{chain}]")]
    LoopDetected { target: String, chain: String },

    #[error("Call limit exceeded: {count}/{max} ask_agent calls this turn")]
    CallLimitExceeded { count: u32, max: u32 },

    #[error("Agent '{name}' timed out after {timeout_secs}s")]
    Timeout { name: String, timeout_secs: u64 },

    #[error("Request to agent '{name}' failed: {detail}")]
    RequestFailed { name: String, detail: String },
}

pub fn parse_agent_registry(env_value: &str) -> Result<Vec<AgentRegistryEntry>, String> {
    serde_json::from_str(env_value)
        .map_err(|e| format!("Failed to parse PUPIL_AGENT_REGISTRY: {e}"))
}

#[derive(Clone)]
pub struct AgentCaller {
    client: reqwest::Client,
    registry: Vec<AgentRegistryEntry>,
    allowed_agents: AllowedAgents,
    max_depth: u32,
    timeout: Duration,
    max_calls_per_turn: u32,
    self_name: String,
    router_url: Option<String>,
}

impl AgentCaller {
    pub fn new(
        config: &CollaborationConfig,
        registry: Vec<AgentRegistryEntry>,
        self_name: String,
        router_url: Option<String>,
    ) -> Self {
        let filtered = match &config.allowed_agents {
            AllowedAgents::All(s) if s == "all" => registry,
            AllowedAgents::All(_) => Vec::new(),
            AllowedAgents::List(list) => registry
                .into_iter()
                .filter(|e| list.contains(&e.name))
                .collect(),
        };

        Self {
            client: reqwest::Client::new(),
            registry: filtered,
            allowed_agents: config.allowed_agents.clone(),
            max_depth: config.max_depth,
            timeout: Duration::from_secs(config.timeout_secs),
            max_calls_per_turn: config.max_calls_per_turn,
            self_name,
            router_url,
        }
    }

    pub fn registry(&self) -> &[AgentRegistryEntry] {
        &self.registry
    }

    pub fn max_calls_per_turn(&self) -> u32 {
        self.max_calls_per_turn
    }

    pub async fn call(
        &self,
        agent: &str,
        question: &str,
        current_depth: u32,
        current_chain: &[String],
    ) -> Result<String, CollaborationError> {
        let entry = self
            .registry
            .iter()
            .find(|e| e.name == agent)
            .ok_or_else(|| {
                let available = self
                    .registry
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                CollaborationError::AgentNotFound {
                    name: agent.to_string(),
                    available,
                }
            })?;

        if !self.allowed_agents.is_allowed(agent) {
            return Err(CollaborationError::AgentNotAllowed {
                name: agent.to_string(),
            });
        }

        if current_chain.iter().any(|n| n == agent) {
            return Err(CollaborationError::LoopDetected {
                target: agent.to_string(),
                chain: current_chain.join(","),
            });
        }

        let next_depth = current_depth + 1;
        if next_depth > self.max_depth {
            return Err(CollaborationError::DepthExceeded {
                current: next_depth,
                max: self.max_depth,
            });
        }

        let mut new_chain = current_chain.to_vec();
        new_chain.push(self.self_name.clone());
        let chain_header = new_chain.join(",");

        let target_url = if let Some(ref router) = self.router_url {
            format!("{}/v1/chat/completions", router)
        } else {
            format!("{}/v1/chat/completions", entry.url)
        };

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": question}],
            "stream": false,
        });

        let mut request = self
            .client
            .post(&target_url)
            .json(&body)
            .header("X-Pupil-Depth", next_depth.to_string())
            .header("X-Pupil-Chain", &chain_header)
            .header("X-Pupil-Source", &self.self_name);

        if self.router_url.is_some() {
            request = request.header("X-Pupil-Target-Agent", agent);
        }

        let start = std::time::Instant::now();

        let response = match tokio::time::timeout(self.timeout, request.send()).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                tracing::warn!(
                    source = %self.self_name,
                    target = %agent,
                    error = %e,
                    "Inter-agent call failed"
                );
                return Err(CollaborationError::RequestFailed {
                    name: agent.to_string(),
                    detail: e.to_string(),
                });
            }
            Err(_) => {
                tracing::warn!(
                    source = %self.self_name,
                    target = %agent,
                    timeout_secs = self.timeout.as_secs(),
                    "Inter-agent call timed out"
                );
                return Err(CollaborationError::Timeout {
                    name: agent.to_string(),
                    timeout_secs: self.timeout.as_secs(),
                });
            }
        };

        let elapsed = start.elapsed();
        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            tracing::warn!(
                source = %self.self_name,
                target = %agent,
                status = %status,
                latency_ms = elapsed.as_millis() as u64,
                "Inter-agent call returned error"
            );
            return Err(CollaborationError::RequestFailed {
                name: agent.to_string(),
                detail: format!("HTTP {status}: {error_body}"),
            });
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            CollaborationError::RequestFailed {
                name: agent.to_string(),
                detail: format!("Failed to parse response: {e}"),
            }
        })?;

        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        tracing::info!(
            source = %self.self_name,
            target = %agent,
            depth = next_depth,
            chain = %chain_header,
            latency_ms = elapsed.as_millis() as u64,
            status = "success",
            "Inter-agent call completed"
        );

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_agent_registry_valid() {
        let json = r#"[
            {"name": "db-expert", "url": "http://db:8080", "description": "DB expert"},
            {"name": "fe-expert", "url": "http://fe:8080"}
        ]"#;
        let entries = parse_agent_registry(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "db-expert");
        assert_eq!(entries[1].description, "");
    }

    #[test]
    fn test_parse_agent_registry_empty() {
        let entries = parse_agent_registry("[]").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_agent_registry_malformed() {
        let result = parse_agent_registry("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_allowed_agents_all() {
        let allowed = AllowedAgents::All("all".to_string());
        assert!(allowed.is_allowed("anything"));
    }

    #[test]
    fn test_allowed_agents_list() {
        let allowed = AllowedAgents::List(vec!["db-expert".to_string()]);
        assert!(allowed.is_allowed("db-expert"));
        assert!(!allowed.is_allowed("fe-expert"));
    }

    #[test]
    fn test_agent_caller_filters_registry() {
        let config = CollaborationConfig {
            enabled: true,
            allowed_agents: AllowedAgents::List(vec!["db-expert".to_string()]),
            max_depth: 3,
            timeout_secs: 120,
            max_calls_per_turn: 10,
        };
        let registry = vec![
            AgentRegistryEntry {
                name: "db-expert".to_string(),
                url: "http://db:8080".to_string(),
                description: "DB".to_string(),
            },
            AgentRegistryEntry {
                name: "fe-expert".to_string(),
                url: "http://fe:8080".to_string(),
                description: "FE".to_string(),
            },
        ];
        let caller = AgentCaller::new(&config, registry, "pm".to_string(), None);
        assert_eq!(caller.registry().len(), 1);
        assert_eq!(caller.registry()[0].name, "db-expert");
    }
}
