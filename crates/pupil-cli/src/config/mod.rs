use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub container_runtime: Option<String>,
    #[serde(default)]
    pub default_registry: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config at {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse config at {path}: {source}")]
    ParseFailed {
        path: PathBuf,
        source: serde_yml::Error,
    },

    #[error("failed to write config to {path}: {source}")]
    WriteFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to serialize config: {source}")]
    SerializeFailed { source: serde_yml::Error },

    #[error("unknown config key: {key}")]
    UnknownKey { key: String },

    #[error("invalid value '{value}' for {key}: expected one of {valid}")]
    InvalidValue {
        key: String,
        value: String,
        valid: String,
    },
}

impl GlobalConfig {
    /// Returns the config file path: ~/.config/pupil/config.yaml
    /// Uses the `dirs` crate for platform-appropriate paths:
    /// - Linux: ~/.config/pupil/config.yaml
    /// - macOS: ~/Library/Application Support/pupil/config.yaml
    ///   (but we use XDG on macOS too for consistency)
    /// - Windows: %APPDATA%\pupil\config.yaml
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .expect("could not determine config directory")
            .join("pupil")
            .join("config.yaml")
    }

    /// Load from disk. Returns Default (all None) if file does not exist.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents =
            std::fs::read_to_string(&path).map_err(|e| ConfigError::ReadFailed {
                path: path.clone(),
                source: e,
            })?;
        serde_yml::from_str(&contents).map_err(|e| ConfigError::ParseFailed { path, source: e })
    }

    /// Save to disk. Creates parent directories if needed.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::WriteFailed {
                path: path.clone(),
                source: e,
            })?;
        }
        let contents =
            serde_yml::to_string(self).map_err(|e| ConfigError::SerializeFailed { source: e })?;
        std::fs::write(&path, contents)
            .map_err(|e| ConfigError::WriteFailed { path, source: e })
    }

    /// Check whether the config file exists on disk.
    pub fn exists() -> bool {
        Self::path().exists()
    }

    /// Get a single config value by key name. Returns None if the key
    /// is not set.
    pub fn get(&self, key: &str) -> Option<String> {
        match key {
            "default_provider" => self.default_provider.clone(),
            "default_model" => self.default_model.clone(),
            "container_runtime" => self.container_runtime.clone(),
            "default_registry" => self.default_registry.clone(),
            _ => None,
        }
    }

    /// Set a single config value by key name. Returns Err if the key
    /// is not recognized.
    pub fn set(&mut self, key: &str, value: String) -> Result<(), ConfigError> {
        match key {
            "default_provider" => {
                self.default_provider = Some(value);
                Ok(())
            }
            "default_model" => {
                self.default_model = Some(value);
                Ok(())
            }
            "container_runtime" => {
                if value != "docker" && value != "podman" {
                    return Err(ConfigError::InvalidValue {
                        key: key.into(),
                        value,
                        valid: "docker, podman".into(),
                    });
                }
                self.container_runtime = Some(value);
                Ok(())
            }
            "default_registry" => {
                self.default_registry = Some(value);
                Ok(())
            }
            _ => Err(ConfigError::UnknownKey { key: key.into() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GlobalConfig::default();
        assert!(config.default_provider.is_none());
        assert!(config.default_model.is_none());
        assert!(config.container_runtime.is_none());
        assert!(config.default_registry.is_none());
    }

    #[test]
    fn test_get_known_key() {
        let config = GlobalConfig {
            default_provider: Some("anthropic".into()),
            default_model: Some("claude-haiku-4".into()),
            container_runtime: Some("docker".into()),
            default_registry: Some("ghcr.io/myorg".into()),
        };
        assert_eq!(config.get("default_provider"), Some("anthropic".into()));
        assert_eq!(config.get("default_model"), Some("claude-haiku-4".into()));
        assert_eq!(config.get("container_runtime"), Some("docker".into()));
        assert_eq!(
            config.get("default_registry"),
            Some("ghcr.io/myorg".into())
        );
    }

    #[test]
    fn test_get_unknown_key() {
        let config = GlobalConfig::default();
        assert!(config.get("unknown_key").is_none());
    }

    #[test]
    fn test_set_valid_runtime() {
        let mut config = GlobalConfig::default();
        assert!(config.set("container_runtime", "docker".into()).is_ok());
        assert_eq!(
            config.container_runtime.as_deref(),
            Some("docker")
        );
    }

    #[test]
    fn test_set_invalid_runtime() {
        let mut config = GlobalConfig::default();
        let result = config.set("container_runtime", "rkt".into());
        assert!(result.is_err());
    }

    #[test]
    fn test_set_unknown_key() {
        let mut config = GlobalConfig::default();
        let result = config.set("nonexistent", "value".into());
        assert!(result.is_err());
    }

    #[test]
    fn test_roundtrip_yaml() {
        let config = GlobalConfig {
            default_provider: Some("anthropic".into()),
            default_model: Some("claude-sonnet-4-6".into()),
            container_runtime: Some("docker".into()),
            default_registry: None,
        };
        let yaml = serde_yml::to_string(&config).unwrap();
        let loaded: GlobalConfig = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(loaded.default_provider, config.default_provider);
        assert_eq!(loaded.default_model, config.default_model);
        assert_eq!(loaded.container_runtime, config.container_runtime);
        assert_eq!(loaded.default_registry, config.default_registry);
    }
}
