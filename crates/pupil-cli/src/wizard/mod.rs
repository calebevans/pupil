use crate::config::{ConfigError, GlobalConfig};
use console::style;
use dialoguer::{theme::ColorfulTheme, Confirm, Password, Select};

#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error("wizard was cancelled by the user")]
    Cancelled,

    #[error("I/O error during wizard: {0}")]
    Io(#[from] std::io::Error),

    #[error("dialog error during wizard: {0}")]
    Dialog(#[from] dialoguer::Error),

    #[error("failed to save config: {0}")]
    ConfigSave(#[from] ConfigError),

    #[error("API key validation failed: {0}")]
    ValidationFailed(String),
}

/// Check whether the wizard should run.
///
/// Returns true if:
/// 1. ~/.config/pupil/config.yaml does not exist
/// 2. PUPIL_SKIP_SETUP is not set to "1"
/// 3. stdout is a TTY (no wizard in non-interactive contexts)
pub fn should_run_wizard() -> bool {
    if std::env::var("PUPIL_SKIP_SETUP").as_deref() == Ok("1") {
        return false;
    }
    if !console::Term::stdout().is_term() {
        return false;
    }
    !GlobalConfig::exists()
}

/// Run the first-run setup wizard.
///
/// Steps:
/// 1. Detect and confirm container runtime (docker/podman)
/// 2. Select LLM provider (Anthropic/OpenAI/Google/Ollama)
/// 3. Enter API key (masked input)
/// 4. Select default model based on provider
/// 5. Validate API key with a test call
/// 6. Write ~/.config/pupil/config.yaml
pub async fn run_wizard() -> Result<GlobalConfig, WizardError> {
    let theme = ColorfulTheme::default();

    println!();
    println!(
        "{}",
        style("Welcome to Pupil! Let's set up your environment.").bold()
    );
    println!();

    // --- Step 1: Detect container runtime ---
    let runtime = detect_container_runtime();
    let runtime = match runtime {
        Some(detected) => {
            println!(
                "  {} Found container runtime: {}",
                style("[OK]").green().bold(),
                style(&detected).cyan()
            );
            detected
        }
        None => {
            println!(
                "  {} No container runtime found.",
                style("[!!]").red().bold()
            );
            println!("  Pupil requires Docker or Podman. Install one and try again.");
            println!("  Docker: https://docs.docker.com/get-docker/");
            println!("  Podman: https://podman.io/getting-started/installation");
            println!();
            if !Confirm::with_theme(&theme)
                .with_prompt("Continue without a container runtime?")
                .default(false)
                .interact()?
            {
                return Err(WizardError::Cancelled);
            }
            "docker".to_string()
        }
    };

    // --- Step 2: Select LLM provider ---
    let providers = &[
        "Anthropic (Claude)",
        "OpenAI (GPT)",
        "Google (Gemini)",
        "Ollama (local)",
    ];
    let provider_idx = Select::with_theme(&theme)
        .with_prompt("Select your LLM provider")
        .items(providers)
        .default(0)
        .interact()?;

    let (provider_name, env_var_name) = match provider_idx {
        0 => ("anthropic", "ANTHROPIC_API_KEY"),
        1 => ("openai", "OPENAI_API_KEY"),
        2 => ("google", "GOOGLE_API_KEY"),
        3 => ("ollama", ""),
        _ => unreachable!(),
    };

    // --- Step 3: Enter API key ---
    let api_key = if !env_var_name.is_empty() {
        let existing = std::env::var(env_var_name).ok();
        if let Some(ref key) = existing {
            let _ = key;
            println!(
                "  {} {} is already set in your environment.",
                style("[OK]").green().bold(),
                env_var_name
            );
            if !Confirm::with_theme(&theme)
                .with_prompt("Use the existing key?")
                .default(true)
                .interact()?
            {
                Some(
                    Password::with_theme(&theme)
                        .with_prompt(format!("Enter your {} API key", provider_name))
                        .interact()?,
                )
            } else {
                existing
            }
        } else {
            println!(
                "  {} is not set. You'll need an API key from your provider.",
                env_var_name
            );
            Some(
                Password::with_theme(&theme)
                    .with_prompt(format!("Enter your {} API key", provider_name))
                    .interact()?,
            )
        }
    } else {
        None
    };

    // --- Step 4: Select default model ---
    let models = match provider_idx {
        0 => vec!["claude-sonnet-4-6", "claude-haiku-4", "claude-opus-4"],
        1 => vec!["gpt-4o", "gpt-4o-mini", "gpt-4.1", "gpt-4.1-mini"],
        2 => vec!["gemini-2.5-pro", "gemini-2.5-flash"],
        3 => vec!["ollama/llama3", "ollama/mistral", "ollama/gemma2"],
        _ => unreachable!(),
    };

    let model_idx = Select::with_theme(&theme)
        .with_prompt("Select your default model")
        .items(&models)
        .default(0)
        .interact()?;

    let model = models[model_idx].to_string();

    // --- Step 5: Validate API key ---
    if let Some(ref key) = api_key {
        print!("  Validating API key... ");
        match validate_api_key(provider_name, key).await {
            Ok(()) => {
                println!("{}", style("OK").green().bold());
            }
            Err(e) => {
                println!("{}", style("FAILED").red().bold());
                println!("  Error: {}", e);
                println!();
                if !Confirm::with_theme(&theme)
                    .with_prompt("Save config anyway?")
                    .default(false)
                    .interact()?
                {
                    return Err(WizardError::Cancelled);
                }
            }
        }
    }

    // --- Step 6: Write config ---
    let config = GlobalConfig {
        default_provider: Some(provider_name.to_string()),
        default_model: Some(model),
        container_runtime: Some(runtime),
        default_registry: None,
    };

    config.save()?;

    println!();
    println!(
        "  {} Config saved to {}",
        style("[OK]").green().bold(),
        style(GlobalConfig::path().display()).dim()
    );

    if let Some(ref _key) = api_key {
        if std::env::var(env_var_name).is_err() {
            println!();
            println!(
                "  {} Add this to your shell profile:",
                style("Note:").yellow().bold()
            );
            println!("  export {}=\"your-key-here\"", env_var_name);
        }
    }

    println!();
    println!(
        "  {}",
        style("Setup complete! Run `pupil create my-agent` to get started.").bold()
    );
    println!();

    Ok(config)
}

/// Detect which container runtime is available.
///
/// Resolution order:
/// 1. PUPIL_CONTAINER_RUNTIME env var (explicit override)
/// 2. `which docker` (check PATH)
/// 3. `which podman` (check PATH)
fn detect_container_runtime() -> Option<String> {
    if let Ok(runtime) = std::env::var("PUPIL_CONTAINER_RUNTIME") {
        if runtime == "docker" || runtime == "podman" {
            if which::which(&runtime).is_ok() {
                return Some(runtime);
            }
        }
        return None;
    }

    if which::which("docker").is_ok() {
        return Some("docker".to_string());
    }
    if which::which("podman").is_ok() {
        return Some("podman".to_string());
    }

    None
}

/// Validate an API key by making a lightweight test call.
async fn validate_api_key(provider: &str, key: &str) -> Result<(), String> {
    let client = reqwest::Client::new();

    match provider {
        "anthropic" => {
            let resp = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(
                    r#"{"model":"claude-haiku-4","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}"#,
                )
                .send()
                .await
                .map_err(|e| format!("request failed: {}", e))?;

            if resp.status().is_success() || resp.status().as_u16() == 200 {
                Ok(())
            } else if resp.status().as_u16() == 401 {
                Err("invalid API key (401 Unauthorized)".into())
            } else {
                Err(format!("unexpected status: {}", resp.status()))
            }
        }
        "openai" => {
            let resp = client
                .get("https://api.openai.com/v1/models")
                .header("Authorization", format!("Bearer {}", key))
                .send()
                .await
                .map_err(|e| format!("request failed: {}", e))?;

            if resp.status().is_success() {
                Ok(())
            } else if resp.status().as_u16() == 401 {
                Err("invalid API key (401 Unauthorized)".into())
            } else {
                Err(format!("unexpected status: {}", resp.status()))
            }
        }
        "google" => {
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models?key={}",
                key
            );
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("request failed: {}", e))?;

            if resp.status().is_success() {
                Ok(())
            } else if resp.status().as_u16() == 400 || resp.status().as_u16() == 403 {
                Err("invalid API key".into())
            } else {
                Err(format!("unexpected status: {}", resp.status()))
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_run_wizard_respects_skip_env() {
        // SAFETY: This test is run in isolation and does not share state
        // with other tests that read PUPIL_SKIP_SETUP.
        unsafe { std::env::set_var("PUPIL_SKIP_SETUP", "1") };
        assert!(!should_run_wizard());
        unsafe { std::env::remove_var("PUPIL_SKIP_SETUP") };
    }

    #[test]
    fn test_detect_container_runtime_env_override() {
        if which::which("docker").is_err() && which::which("podman").is_err() {
            return;
        }
        let runtime = detect_container_runtime();
        assert!(runtime.is_some());
    }
}
