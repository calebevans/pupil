use clap::{Args, Subcommand};

use crate::config::GlobalConfig;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    Get { key: String },
    Set { key: String, value: String },
    List,
}

pub async fn execute(args: ConfigArgs) -> Result<(), CliError> {
    match args.action {
        ConfigAction::Get { key } => {
            let config = GlobalConfig::load().unwrap_or_default();
            let value = get_config_value(&config, &key)?;
            match value {
                Some(v) => println!("{}", v),
                None => println!("(not set)"),
            }
        }
        ConfigAction::Set { key, value } => {
            let mut config = GlobalConfig::load().unwrap_or_default();
            set_config_value(&mut config, &key, &value)?;
            config.save().map_err(|e| CliError::GlobalConfigInvalid {
                message: format!("Failed to save config: {}", e),
            })?;
            println!("Set {} = {}", key, value);
        }
        ConfigAction::List => {
            let config = GlobalConfig::load().unwrap_or_default();
            let path = GlobalConfig::path();
            println!("Config file: {}", path.display());
            println!();
            println!(
                "  default_provider:   {}",
                config.default_provider.as_deref().unwrap_or("(not set)")
            );
            println!(
                "  default_model:      {}",
                config.default_model.as_deref().unwrap_or("(not set)")
            );
            println!(
                "  container_runtime:  {}",
                config
                    .container_runtime
                    .as_deref()
                    .unwrap_or("(auto-detect)")
            );
            println!(
                "  default_registry:   {}",
                config.default_registry.as_deref().unwrap_or("(not set)")
            );
        }
    }
    Ok(())
}

fn get_config_value(config: &GlobalConfig, key: &str) -> Result<Option<String>, CliError> {
    match key {
        "default_provider" => Ok(config.default_provider.clone()),
        "default_model" => Ok(config.default_model.clone()),
        "container_runtime" => Ok(config.container_runtime.clone()),
        "default_registry" => Ok(config.default_registry.clone()),
        _ => Err(CliError::GlobalConfigInvalid {
            message: format!(
                "Unknown config key: '{}'. Valid keys: default_provider, default_model, container_runtime, default_registry",
                key
            ),
        }),
    }
}

fn set_config_value(
    config: &mut GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), CliError> {
    match key {
        "default_provider" => {
            let valid = ["anthropic", "openai", "google", "ollama"];
            if !valid.contains(&value) {
                return Err(CliError::GlobalConfigInvalid {
                    message: format!(
                        "Invalid provider: '{}'. Valid: {}",
                        value,
                        valid.join(", ")
                    ),
                });
            }
            config.default_provider = Some(value.to_string());
        }
        "default_model" => {
            config.default_model = Some(value.to_string());
        }
        "container_runtime" => {
            let valid = ["docker", "podman"];
            if !valid.contains(&value) {
                return Err(CliError::GlobalConfigInvalid {
                    message: format!(
                        "Invalid runtime: '{}'. Valid: {}",
                        value,
                        valid.join(", ")
                    ),
                });
            }
            config.container_runtime = Some(value.to_string());
        }
        "default_registry" => {
            config.default_registry = Some(value.to_string());
        }
        _ => {
            return Err(CliError::GlobalConfigInvalid {
                message: format!(
                    "Unknown config key: '{}'. Valid keys: default_provider, default_model, container_runtime, default_registry",
                    key
                ),
            });
        }
    }
    Ok(())
}
