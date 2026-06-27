use clap::Args;

use crate::agent_config::{image_ref, resolve_agent};
use crate::container::{self, ContainerRuntime};
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct PushArgs {
    pub name: Option<String>,

    pub registry_ref: String,

    #[arg(long)]
    pub latest: bool,

    #[arg(long)]
    pub force: bool,
}

pub async fn execute(args: PushArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let local_image = image_ref(&config.name, None);
    if !image_exists(runtime.as_ref(), &local_image).await? {
        return Err(CliError::ImageNotFound {
            name: config.name.clone(),
        });
    }

    let inspect_output = tokio::process::Command::new(runtime.name())
        .args(["inspect", "--format", "{{.Config.Env}}", &local_image])
        .output()
        .await?;
    let env_str = String::from_utf8_lossy(&inspect_output.stdout);

    let secret_patterns = [
        "API_KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PRIVATE_KEY",
        "ACCESS_KEY",
        "CREDENTIALS",
    ];
    let suspicious: Vec<String> = env_str
        .split_whitespace()
        .filter(|e| {
            let upper = e.to_uppercase();
            secret_patterns.iter().any(|p| upper.contains(p))
        })
        .map(|s| s.to_string())
        .collect();

    if !suspicious.is_empty() && !args.force {
        eprintln!("Warning: The image may contain secrets in environment variables:");
        for var in &suspicious {
            let key = var.split('=').next().unwrap_or(var);
            eprintln!("  - {}", key);
        }
        eprintln!();
        eprintln!("API keys should be injected at runtime via -e, not baked into images.");
        eprintln!("Use --force to push anyway.");
        return Err(CliError::PushFailed {
            reference: args.registry_ref,
            message: "Image may contain secrets. Use --force to override.".to_string(),
        });
    }

    let tag_output = tokio::process::Command::new(runtime.name())
        .args(["tag", &local_image, &args.registry_ref])
        .output()
        .await?;

    if !tag_output.status.success() {
        return Err(CliError::PushFailed {
            reference: args.registry_ref.clone(),
            message: format!(
                "Failed to tag image: {}",
                String::from_utf8_lossy(&tag_output.stderr)
            ),
        });
    }

    println!("Pushing {} ...", args.registry_ref);
    runtime
        .push(&args.registry_ref)
        .await
        .map_err(|e| CliError::PushFailed {
            reference: args.registry_ref.clone(),
            message: e.to_string(),
        })?;
    println!("Pushed: {}", args.registry_ref);

    if args.latest {
        let latest_ref = if let Some(idx) = args.registry_ref.rfind(':') {
            format!("{}:latest", &args.registry_ref[..idx])
        } else {
            format!("{}:latest", args.registry_ref)
        };

        let tag_output = tokio::process::Command::new(runtime.name())
            .args(["tag", &local_image, &latest_ref])
            .output()
            .await?;

        if tag_output.status.success() {
            runtime
                .push(&latest_ref)
                .await
                .map_err(|e| CliError::PushFailed {
                    reference: latest_ref.clone(),
                    message: e.to_string(),
                })?;
            println!("Pushed: {}", latest_ref);
        }
    }

    Ok(())
}

async fn image_exists(
    runtime: &dyn ContainerRuntime,
    image: &str,
) -> Result<bool, CliError> {
    let output = tokio::process::Command::new(runtime.name())
        .args(["image", "inspect", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    Ok(output.success())
}
