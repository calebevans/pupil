use clap::Args;

use crate::agent_config::BASE_IMAGE;
use crate::config::GlobalConfig;
use crate::container;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    #[arg(long)]
    pub details: bool,
}

pub async fn execute(args: DoctorArgs) -> Result<(), CliError> {
    let mut all_ok = true;

    println!("Pupil Doctor");
    println!("============");
    println!();

    print!("Container runtime: ");
    match container::detect() {
        Ok(rt) => {
            let version_output = tokio::process::Command::new(rt.name())
                .args(["--version"])
                .output()
                .await;
            match version_output {
                Ok(out) => {
                    let version = String::from_utf8_lossy(&out.stdout);
                    println!("OK ({})", version.trim());
                }
                Err(_) => println!("OK ({})", rt.name()),
            }
        }
        Err(_) => {
            println!("NOT FOUND");
            println!("  Install Docker: https://docs.docker.com/get-docker/");
            println!("  or Podman: https://podman.io/getting-started/installation");
            all_ok = false;
        }
    }

    print!("Global config: ");
    let config_path = GlobalConfig::path();
    if config_path.exists() {
        match GlobalConfig::load() {
            Ok(config) => {
                println!("OK ({})", config_path.display());
                if args.details {
                    if let Some(ref provider) = config.default_provider {
                        println!("  Default provider: {}", provider);
                    }
                    if let Some(ref model) = config.default_model {
                        println!("  Default model: {}", model);
                    }
                    if let Some(ref rt) = config.container_runtime {
                        println!("  Runtime: {}", rt);
                    }
                }
            }
            Err(_) => {
                println!("INVALID ({})", config_path.display());
                println!("  Run `pupil config list` to check the config file.");
                all_ok = false;
            }
        }
    } else {
        println!("NOT FOUND (will be created on first run)");
        if args.details {
            println!("  Expected at: {}", config_path.display());
        }
    }

    let api_keys = [
        ("ANTHROPIC_API_KEY", "Anthropic (Claude models)"),
        ("OPENAI_API_KEY", "OpenAI (GPT models)"),
        ("GOOGLE_API_KEY", "Google (Gemini models)"),
    ];

    println!();
    println!("API Keys:");
    let mut any_key_set = false;
    for (key, desc) in &api_keys {
        let is_set = std::env::var(key).is_ok();
        if is_set {
            println!("  {}: SET ({})", key, desc);
            any_key_set = true;
        } else if args.details {
            println!("  {}: not set ({})", key, desc);
        }
    }
    if !any_key_set {
        println!("  No API keys found. Set at least one to use Pupil.");
        println!("  Example: export ANTHROPIC_API_KEY=sk-...");
        all_ok = false;
    }

    println!();
    print!("Ollama: ");
    let ollama_url = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());

    match reqwest::get(&format!("{}/api/tags", ollama_url)).await {
        Ok(resp) if resp.status().is_success() => {
            println!("OK ({})", ollama_url);
            if args.details {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let models: Vec<String> = body
                        .get("models")
                        .and_then(|m| m.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    println!(
                        "  Models: {}",
                        if models.is_empty() {
                            "none".to_string()
                        } else {
                            models.join(", ")
                        }
                    );
                    if !models.iter().any(|m| m.contains("embeddinggemma")) {
                        println!(
                            "  Note: embeddinggemma not pulled. Run `ollama pull embeddinggemma`."
                        );
                    }
                }
            }
        }
        _ => {
            println!("NOT AVAILABLE ({})", ollama_url);
            println!("  Ollama is optional but needed for local embeddings.");
            println!("  Install: https://ollama.com/download");
            println!("  Or use --with-ollama when running agents.");
        }
    }

    println!();
    print!("Base image: ");
    if let Ok(rt) = container::detect() {
        let check = tokio::process::Command::new(rt.name())
            .args(["image", "inspect", BASE_IMAGE])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        match check {
            Ok(status) if status.success() => println!("OK ({})", BASE_IMAGE),
            Ok(_) => {
                println!("NOT PULLED");
                println!("  Will be pulled automatically on first build.");
            }
            Err(_) => println!("UNKNOWN (could not check)"),
        }
    } else {
        println!("SKIPPED (no container runtime)");
    }

    println!();
    if all_ok {
        println!("All checks passed. Pupil is ready to use.");
    } else {
        println!("Some checks failed. Fix the issues above before using Pupil.");
    }

    Ok(())
}
