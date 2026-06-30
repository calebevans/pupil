pub mod config;
pub mod normalize;
pub mod schema;

use std::collections::HashMap;
use std::time::Duration;

use process_wrap::tokio::{CommandWrap, ProcessGroup};
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{DynService, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use thiserror::Error;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::mcp::config::{ConfigError, McpServerConfig};

#[derive(Debug, Error)]
pub enum McpError {
    #[error("required MCP server `{server_name}` failed to start: {source}")]
    RequiredServerStartFailed {
        server_name: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("required MCP server `{server_name}` failed tool discovery: {source}")]
    RequiredServerDiscoveryFailed {
        server_name: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("optional MCP server `{server_name}` failed to start: {source}")]
    OptionalServerStartFailed {
        server_name: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("MCP server `{server_name}` not found in active servers")]
    ServerNotFound { server_name: String },

    #[error("unknown tool `{tool_name}`; not registered by any MCP server{}", suggestion.as_ref().map(|s| format!(". Did you mean `{s}`?")).unwrap_or_default())]
    UnknownTool {
        tool_name: String,
        suggestion: Option<String>,
    },

    #[error("tool `{tool_name}` on server `{server_name}` returned error: {detail}")]
    ToolCallFailed {
        tool_name: String,
        server_name: String,
        detail: String,
    },

    #[error("MCP server configuration error: {0}")]
    ConfigError(#[from] ConfigError),

    #[error("MCP server `{server_name}` transport closed")]
    TransportClosed { server_name: String },

    #[error("tool `{tool_name}` timed out after {timeout_secs}s")]
    ToolCallTimeout {
        tool_name: String,
        timeout_secs: u64,
    },
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub original_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Clone)]
pub struct McpManager {
    inner: std::sync::Arc<McpManagerInner>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("tool_count", &self.inner.tools.len())
            .finish()
    }
}

struct McpManagerInner {
    servers: tokio::sync::Mutex<HashMap<String, RunningService<RoleClient, Box<dyn DynService<RoleClient>>>>>,
    tool_index: HashMap<String, String>,
    tools: Vec<ToolDefinition>,
    tool_filters: HashMap<String, config::ToolFilter>,
    cancel_token: CancellationToken,
}

impl McpManager {
    pub async fn start_all(
        configs: &HashMap<String, McpServerConfig>,
        cancel_token: CancellationToken,
    ) -> Result<Self, McpError> {
        // --- Phase 1: Validate configs ---
        let config_errors = crate::mcp::config::validate_configs(configs);
        for err in &config_errors {
            let is_required = match err {
                ConfigError::EmptyCommand { server_name }
                | ConfigError::MissingEnvVar { server_name, .. }
                | ConfigError::InvalidServerName { name: server_name } => configs
                    .get(server_name.as_str())
                    .map(|c| c.required)
                    .unwrap_or(false),
            };
            if is_required {
                return Err(McpError::ConfigError(
                    config_errors.into_iter().next().unwrap(),
                ));
            } else {
                tracing::warn!(error = %err, "optional MCP server config error");
            }
        }

        // --- Phase 2: Start servers and discover tools ---
        let mut servers = HashMap::new();
        let mut raw_tools: HashMap<String, Vec<Tool>> = HashMap::new();

        for (name, config) in configs {
            let resolved_env = match crate::mcp::config::resolve_env(config, name) {
                Ok(env) => env,
                Err(e) => {
                    if config.required {
                        return Err(McpError::ConfigError(e));
                    }
                    tracing::warn!(
                        server = %name,
                        error = %e,
                        "skipping optional MCP server due to config error"
                    );
                    continue;
                }
            };

            let mut cmd = Command::new(&config.command);
            cmd.args(&config.args);
            for (k, v) in &resolved_env {
                cmd.env(k, v);
            }
            cmd.stderr(std::process::Stdio::piped());

            let mut wrap = CommandWrap::from(cmd);
            wrap.wrap(ProcessGroup::leader());

            let transport = match TokioChildProcess::new(wrap) {
                Ok(t) => t,
                Err(e) => {
                    let err = anyhow::Error::new(e)
                        .context(format!("failed to spawn `{}`", config.command));
                    if config.required {
                        return Err(McpError::RequiredServerStartFailed {
                            server_name: name.clone(),
                            source: err,
                        });
                    }
                    tracing::warn!(
                        server = %name,
                        error = %err,
                        "optional MCP server failed to start; skipping"
                    );
                    continue;
                }
            };

            let service = match ().into_dyn().serve(transport).await {
                Ok(s) => s,
                Err(e) => {
                    let err =
                        anyhow::Error::msg(format!("{e}")).context("MCP handshake failed");
                    if config.required {
                        return Err(McpError::RequiredServerStartFailed {
                            server_name: name.clone(),
                            source: err,
                        });
                    }
                    tracing::warn!(
                        server = %name,
                        error = %err,
                        "optional MCP server handshake failed; skipping"
                    );
                    continue;
                }
            };

            let tools = match service.list_all_tools().await {
                Ok(t) => t,
                Err(e) => {
                    let err =
                        anyhow::Error::msg(format!("{e}")).context("tool discovery failed");
                    if config.required {
                        return Err(McpError::RequiredServerDiscoveryFailed {
                            server_name: name.clone(),
                            source: err,
                        });
                    }
                    tracing::warn!(
                        server = %name,
                        error = %err,
                        "optional MCP server tool discovery failed; skipping"
                    );
                    let _ = service.cancel().await;
                    continue;
                }
            };

            tracing::info!(
                server = %name,
                tool_count = tools.len(),
                tools = ?tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(),
                "MCP server started"
            );

            raw_tools.insert(name.clone(), tools);
            servers.insert(name.clone(), service);
        }

        // --- Phase 3: Detect tool name collisions ---
        let mut name_to_servers: HashMap<String, Vec<String>> = HashMap::new();
        for (server_name, tool_list) in &raw_tools {
            for tool in tool_list {
                name_to_servers
                    .entry(tool.name.to_string())
                    .or_default()
                    .push(server_name.clone());
            }
        }

        let collisions: std::collections::HashSet<String> = name_to_servers
            .iter()
            .filter(|(_, srvs)| srvs.len() > 1)
            .map(|(tool_name, srvs)| {
                tracing::warn!(
                    tool = %tool_name,
                    servers = ?srvs,
                    "tool name collision; both will be prefixed with server name"
                );
                tool_name.clone()
            })
            .collect();

        // --- Phase 4: Build tool_index and tools vec ---
        let mut tool_index: HashMap<String, String> = HashMap::new();
        let mut tools: Vec<ToolDefinition> = Vec::new();

        for (server_name, tool_list) in &raw_tools {
            for tool in tool_list {
                let original_name = tool.name.to_string();
                let display_name = if collisions.contains(&original_name) {
                    format!("{}.{}", server_name, original_name)
                } else {
                    original_name.clone()
                };

                tool_index.insert(display_name.clone(), server_name.clone());

                let mut schema_value = serde_json::to_value(&*tool.input_schema)
                    .unwrap_or_else(|_| serde_json::json!({"type": "object"}));

                if display_name == "store_memory" {
                    promote_to_required(&mut schema_value, &["entities", "topics"]);
                }

                tools.push(ToolDefinition {
                    name: display_name,
                    original_name,
                    description: tool.description.as_ref().map(|d| d.to_string()),
                    input_schema: schema_value,
                });
            }
        }

        // --- Phase 5: Log summary ---
        tracing::info!(
            server_count = servers.len(),
            tool_count = tools.len(),
            collision_count = collisions.len(),
            "MCP manager initialized"
        );

        let mut tool_filters = HashMap::new();
        for (name, cfg) in configs {
            if let Some(ref filters) = cfg.tools {
                for (stage, filter) in filters {
                    tool_filters.insert(
                        format!("{}:{}", name, stage),
                        filter.clone(),
                    );
                }
            }
        }

        Ok(Self {
            inner: std::sync::Arc::new(McpManagerInner {
                servers: tokio::sync::Mutex::new(servers),
                tool_index,
                tools,
                tool_filters,
                cancel_token,
            }),
        })
    }

    pub fn all_tools(&self) -> &[ToolDefinition] {
        &self.inner.tools
    }

    pub fn tools_for_stage(&self, stage: &str) -> Vec<&ToolDefinition> {
        let allowed: Option<std::collections::HashSet<&str>> = {
            let mut names = None;
            for (key, filter) in &self.inner.tool_filters {
                if key.ends_with(&format!(":{}", stage)) {
                    match filter {
                        config::ToolFilter::All(s) if s == "all" => return self.inner.tools.iter().collect(),
                        config::ToolFilter::List(list) => {
                            let set = names.get_or_insert_with(std::collections::HashSet::new);
                            set.extend(list.iter().map(|s| s.as_str()));
                        }
                        _ => {}
                    }
                }
            }
            names
        };

        match allowed {
            Some(set) => self.inner.tools.iter().filter(|t| set.contains(t.name.as_str())).collect(),
            None => self.inner.tools.iter().collect(),
        }
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        let resolved_name = match self.inner.tool_index.get(tool_name) {
            Some(_) => tool_name.to_string(),
            None => {
                let registered_names: Vec<&str> = self
                    .inner
                    .tool_index
                    .keys()
                    .map(|s| s.as_str())
                    .collect();
                match normalize::resolve_tool_name(tool_name, &registered_names) {
                    Some(normalized) => {
                        tracing::warn!(
                            original_name = %tool_name,
                            resolved_name = %normalized.name,
                            strategy = %normalized.strategy,
                            "tool_name_normalized: LLM produced a hallucinated \
                             tool name that was resolved via normalization"
                        );
                        normalized.name
                    }
                    None => {
                        let suggestion =
                            normalize::closest_tool_name(tool_name, &registered_names);
                        return Err(McpError::UnknownTool {
                            tool_name: tool_name.to_string(),
                            suggestion,
                        });
                    }
                }
            }
        };

        let server_name = self
            .inner
            .tool_index
            .get(&resolved_name)
            .expect("resolved_name was validated against tool_index")
            .clone();

        let servers = self.inner.servers.lock().await;
        let service = servers
            .get(&server_name)
            .ok_or_else(|| McpError::ServerNotFound {
                server_name: server_name.clone(),
            })?;

        let original_name = resolved_name
            .strip_prefix(&format!("{}.", server_name))
            .unwrap_or(&resolved_name);

        let mut params = CallToolRequestParams::new(original_name.to_string());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }

        let result = service.call_tool(params).await.map_err(|e| match e {
            rmcp::service::ServiceError::TransportClosed => McpError::TransportClosed {
                server_name: server_name.clone(),
            },
            other => McpError::ToolCallFailed {
                tool_name: resolved_name.to_string(),
                server_name: server_name.clone(),
                detail: other.to_string(),
            },
        })?;

        Ok(result)
    }

    pub async fn shutdown_all(&self) {
        self.inner.cancel_token.cancel();

        let servers: HashMap<String, RunningService<RoleClient, Box<dyn DynService<RoleClient>>>> = {
            let mut locked = self.inner.servers.lock().await;
            std::mem::take(&mut *locked)
        };

        for (name, service) in servers {
            tracing::info!(server = %name, "shutting down MCP server");
            match tokio::time::timeout(Duration::from_secs(5), service.cancel()).await {
                Ok(Ok(_quit_reason)) => {
                    tracing::info!(server = %name, "MCP server shut down cleanly");
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        server = %name,
                        error = %e,
                        "MCP server shutdown returned error"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        server = %name,
                        "MCP server shutdown timed out after 5s; \
                         process will be killed on drop"
                    );
                }
            }
        }
    }

    pub async fn health_check(&self) -> HashMap<String, Result<usize, String>> {
        let mut results = HashMap::new();
        let timeout = Duration::from_secs(10);

        let servers = self.inner.servers.lock().await;
        for (name, service) in servers.iter() {
            let check = tokio::time::timeout(timeout, service.list_all_tools()).await;

            let status = match check {
                Ok(Ok(tools)) => Ok(tools.len()),
                Ok(Err(e)) => Err(format!("list_tools failed: {e}")),
                Err(_) => Err("health check timed out after 10s".to_string()),
            };

            results.insert(name.clone(), status);
        }

        results
    }
}

fn promote_to_required(schema: &mut serde_json::Value, fields: &[&str]) {
    if let Some(obj) = schema.as_object_mut() {
        let required = obj
            .entry("required")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut();
        if let Some(arr) = required {
            for field in fields {
                let val = serde_json::Value::String(field.to_string());
                if !arr.contains(&val) {
                    arr.push(val);
                }
            }
        }
    }
}

pub fn spawn_health_check_task(
    manager: McpManager,
    cancel_token: CancellationToken,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::debug!("health check task cancelled");
                    break;
                }
                _ = ticker.tick() => {
                    let status = manager.health_check().await;
                    for (name, result) in &status {
                        match result {
                            Ok(n) => {
                                tracing::debug!(
                                    server = %name,
                                    tools = n,
                                    "health check passed"
                                );
                            }
                            Err(msg) => {
                                tracing::warn!(
                                    server = %name,
                                    error = %msg,
                                    "health check failed"
                                );
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_start_all_empty_config() {
        let configs = HashMap::new();
        let token = CancellationToken::new();
        let manager = McpManager::start_all(&configs, token).await.unwrap();
        assert!(manager.all_tools().is_empty());
    }

    #[tokio::test]
    async fn test_call_unknown_tool() {
        let configs = HashMap::new();
        let token = CancellationToken::new();
        let manager = McpManager::start_all(&configs, token).await.unwrap();

        let result = manager.call_tool("nonexistent", None).await;
        match &result {
            Err(McpError::UnknownTool { tool_name, suggestion }) => {
                assert_eq!(tool_name, "nonexistent");
                assert!(suggestion.is_none());
            }
            other => panic!("expected UnknownTool, got: {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "requires mock MCP servers"]
    async fn test_tool_collision_prefixing() {
        let mut configs = HashMap::new();
        configs.insert(
            "server_a".to_string(),
            McpServerConfig {
                command: "echo-mcp-server".to_string(),
                args: vec!["--name".to_string(), "echo".to_string()],
                env: HashMap::new(),
                required: false,
                tools: None,
            },
        );
        configs.insert(
            "server_b".to_string(),
            McpServerConfig {
                command: "echo-mcp-server".to_string(),
                args: vec!["--name".to_string(), "echo".to_string()],
                env: HashMap::new(),
                required: false,
                tools: None,
            },
        );

        let token = CancellationToken::new();
        let manager = McpManager::start_all(&configs, token.clone()).await.unwrap();

        let tools = manager.all_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"server_a.echo"));
        assert!(names.contains(&"server_b.echo"));
        assert!(!names.contains(&"echo"));

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn test_required_server_failure() {
        let mut configs = HashMap::new();
        configs.insert(
            "bad_server".to_string(),
            McpServerConfig {
                command: "/nonexistent/binary".to_string(),
                args: vec![],
                env: HashMap::new(),
                required: true,
                tools: None,
            },
        );

        let token = CancellationToken::new();
        let result = McpManager::start_all(&configs, token).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            McpError::RequiredServerStartFailed { .. }
        ));
    }

    #[tokio::test]
    async fn test_optional_server_failure() {
        let mut configs = HashMap::new();
        configs.insert(
            "bad_server".to_string(),
            McpServerConfig {
                command: "/nonexistent/binary".to_string(),
                args: vec![],
                env: HashMap::new(),
                required: false,
                tools: None,
            },
        );

        let token = CancellationToken::new();
        let manager = McpManager::start_all(&configs, token).await.unwrap();
        assert!(manager.all_tools().is_empty());
    }

    #[tokio::test]
    async fn test_shutdown_empty() {
        let configs = HashMap::new();
        let token = CancellationToken::new();
        let manager = McpManager::start_all(&configs, token).await.unwrap();
        manager.shutdown_all().await;
    }
}
