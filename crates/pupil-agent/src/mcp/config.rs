use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("mcp_servers.{server_name}: `command` must not be empty")]
    EmptyCommand { server_name: String },

    #[error(
        "mcp_servers.{server_name}: environment variable `{var_name}` \
         is not set (used in env.{field})"
    )]
    MissingEnvVar {
        server_name: String,
        var_name: String,
        field: String,
    },

    #[error("mcp_servers: server name `{name}` is invalid; must match [a-zA-Z0-9_-]+")]
    InvalidServerName { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub env: HashMap<String, String>,

    #[serde(default)]
    pub required: bool,
}

static ENV_VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex")
});

pub fn substitute_env_vars(
    value: &str,
    server_name: &str,
    field: &str,
    required: bool,
) -> Result<String, ConfigError> {
    let mut result = value.to_string();
    let matches: Vec<(String, String)> = ENV_VAR_RE
        .captures_iter(value)
        .map(|cap| {
            let full_match = cap.get(0).unwrap().as_str().to_string();
            let var_name = cap.get(1).unwrap().as_str().to_string();
            (full_match, var_name)
        })
        .collect();

    for (placeholder, var_name) in matches {
        match std::env::var(&var_name) {
            Ok(val) => {
                result = result.replace(&placeholder, &val);
            }
            Err(_) => {
                if required {
                    return Err(ConfigError::MissingEnvVar {
                        server_name: server_name.to_string(),
                        var_name,
                        field: field.to_string(),
                    });
                } else {
                    tracing::warn!(
                        server = server_name,
                        var = var_name,
                        field = field,
                        "environment variable not set; substituting empty string"
                    );
                    result = result.replace(&placeholder, "");
                }
            }
        }
    }
    Ok(result)
}

pub fn resolve_env(
    config: &McpServerConfig,
    server_name: &str,
) -> Result<HashMap<String, String>, ConfigError> {
    let mut resolved = HashMap::with_capacity(config.env.len());
    for (key, raw_value) in &config.env {
        let value = substitute_env_vars(raw_value, server_name, key, config.required)?;
        resolved.insert(key.clone(), value);
    }
    Ok(resolved)
}

static SERVER_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]+$").expect("valid regex"));

pub fn validate_configs(configs: &HashMap<String, McpServerConfig>) -> Vec<ConfigError> {
    let name_re = &*SERVER_NAME_RE;
    let mut errors = Vec::new();

    for (name, config) in configs {
        if !name_re.is_match(name) {
            errors.push(ConfigError::InvalidServerName {
                name: name.clone(),
            });
        }

        if config.command.trim().is_empty() {
            errors.push(ConfigError::EmptyCommand {
                server_name: name.clone(),
            });
        }

        if let Err(e) = resolve_env(config, name) {
            errors.push(e);
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_no_vars() {
        let result =
            substitute_env_vars("/data/recalld", "recalld", "RECALLD_DATA_DIR", true).unwrap();
        assert_eq!(result, "/data/recalld");
    }

    #[test]
    fn test_substitute_single_var() {
        unsafe { std::env::set_var("TEST_KEY_1", "secret123") };
        let result =
            substitute_env_vars("${TEST_KEY_1}", "web_search", "API_KEY", true).unwrap();
        assert_eq!(result, "secret123");
        unsafe { std::env::remove_var("TEST_KEY_1") };
    }

    #[test]
    fn test_substitute_multiple_vars() {
        unsafe { std::env::set_var("TEST_HOST", "localhost") };
        unsafe { std::env::set_var("TEST_PORT", "8080") };
        let result = substitute_env_vars(
            "http://${TEST_HOST}:${TEST_PORT}/api",
            "my_server",
            "BASE_URL",
            true,
        )
        .unwrap();
        assert_eq!(result, "http://localhost:8080/api");
        unsafe { std::env::remove_var("TEST_HOST") };
        unsafe { std::env::remove_var("TEST_PORT") };
    }

    #[test]
    fn test_substitute_missing_var_required() {
        unsafe { std::env::remove_var("NONEXISTENT_VAR_XYZ") };
        let result =
            substitute_env_vars("${NONEXISTENT_VAR_XYZ}", "my_server", "SECRET", true);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::MissingEnvVar {
                server_name,
                var_name,
                field,
            } => {
                assert_eq!(server_name, "my_server");
                assert_eq!(var_name, "NONEXISTENT_VAR_XYZ");
                assert_eq!(field, "SECRET");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn test_substitute_missing_var_optional() {
        unsafe { std::env::remove_var("NONEXISTENT_VAR_ABC") };
        let result = substitute_env_vars(
            "prefix_${NONEXISTENT_VAR_ABC}_suffix",
            "my_server",
            "KEY",
            false,
        )
        .unwrap();
        assert_eq!(result, "prefix__suffix");
    }

    #[test]
    fn test_validate_empty_command() {
        let mut configs = HashMap::new();
        configs.insert(
            "bad_server".to_string(),
            McpServerConfig {
                command: "".to_string(),
                args: vec![],
                env: HashMap::new(),
                required: true,
            },
        );
        let errors = validate_configs(&configs);
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::EmptyCommand { .. })));
    }

    #[test]
    fn test_validate_invalid_server_name() {
        let mut configs = HashMap::new();
        configs.insert(
            "bad server name!".to_string(),
            McpServerConfig {
                command: "some-cmd".to_string(),
                args: vec![],
                env: HashMap::new(),
                required: false,
            },
        );
        let errors = validate_configs(&configs);
        assert!(errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidServerName { .. })));
    }

    #[test]
    fn test_validate_valid_config() {
        let mut configs = HashMap::new();
        configs.insert(
            "recalld".to_string(),
            McpServerConfig {
                command: "recalld".to_string(),
                args: vec!["mcp".to_string()],
                env: {
                    let mut m = HashMap::new();
                    m.insert(
                        "RECALLD_DATA_DIR".to_string(),
                        "/data/recalld".to_string(),
                    );
                    m
                },
                required: true,
            },
        );
        let errors = validate_configs(&configs);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_resolve_env_with_substitution() {
        unsafe { std::env::set_var("TEST_RESOLVE_KEY", "resolved_value") };
        let config = McpServerConfig {
            command: "test".to_string(),
            args: vec![],
            env: {
                let mut m = HashMap::new();
                m.insert("API_KEY".to_string(), "${TEST_RESOLVE_KEY}".to_string());
                m.insert("PLAIN".to_string(), "no_vars_here".to_string());
                m
            },
            required: true,
        };
        let resolved = resolve_env(&config, "test_server").unwrap();
        assert_eq!(resolved.get("API_KEY").unwrap(), "resolved_value");
        assert_eq!(resolved.get("PLAIN").unwrap(), "no_vars_here");
        unsafe { std::env::remove_var("TEST_RESOLVE_KEY") };
    }

    #[test]
    fn test_deserialization_from_yaml() {
        let yaml = r#"
recalld:
  command: recalld
  args: ["mcp"]
  required: true
  env:
    RECALLD_DATA_DIR: /data/recalld
web_search:
  command: /usr/local/bin/web-search-mcp
  args: []
  required: false
  env:
    API_KEY: "${WEB_SEARCH_KEY}"
"#;
        let configs: HashMap<String, McpServerConfig> = serde_yml::from_str(yaml).unwrap();
        assert_eq!(configs.len(), 2);
        assert!(configs["recalld"].required);
        assert!(!configs["web_search"].required);
        assert_eq!(configs["recalld"].command, "recalld");
        assert_eq!(configs["recalld"].args, vec!["mcp"]);
        assert_eq!(
            configs["web_search"].env.get("API_KEY").unwrap(),
            "${WEB_SEARCH_KEY}"
        );
    }
}
