use clap::Args;

use crate::agent_config::{image_ref, resolve_agent};
use crate::container;
use crate::error::CliError;

use super::build::load_manifest_from_image;
use super::run::is_container_running;

#[derive(Args, Debug)]
pub struct StatusArgs {
    pub name: Option<String>,

    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: StatusArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let image = image_ref(&config.name, None);

    let image_check = tokio::process::Command::new(runtime.name())
        .args(["image", "inspect", &image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    let image_exists = image_check.success();

    let container_name = format!("pupil-{}", config.name);
    let is_running = is_container_running(runtime.as_ref(), &container_name)
        .await
        .unwrap_or(false);

    let manifest = if image_exists {
        load_manifest_from_image(runtime.as_ref(), &image)
            .await
            .ok()
    } else {
        None
    };

    let image_size = if image_exists {
        let size_output = tokio::process::Command::new(runtime.name())
            .args(["image", "inspect", "--format", "{{.Size}}", &image])
            .output()
            .await;
        match size_output {
            Ok(out) => {
                let size_str = String::from_utf8_lossy(&out.stdout);
                if let Ok(bytes) = size_str.trim().parse::<u64>() {
                    format!("{:.1}MB", bytes as f64 / 1_048_576.0)
                } else {
                    "unknown".to_string()
                }
            }
            Err(_) => "unknown".to_string(),
        }
    } else {
        "not built".to_string()
    };

    let memory_count = manifest.as_ref().and_then(|m| {
        m.get("sources")
            .and_then(|s| s.as_object())
            .map(|sources| {
                sources
                    .values()
                    .filter_map(|v| v.get("memory_ids"))
                    .filter_map(|ids| ids.as_array())
                    .map(|arr| arr.len() as u64)
                    .sum::<u64>()
            })
    });

    let last_build = manifest
        .as_ref()
        .and_then(|m| m.get("builds"))
        .and_then(|b| b.as_array())
        .and_then(|arr| arr.last())
        .cloned();

    if args.json {
        let status = serde_json::json!({
            "name": config.name,
            "model": config.model,
            "status": if is_running { "running" } else { "stopped" },
            "image": image,
            "image_size": image_size,
            "memory_count": memory_count,
            "last_build": last_build,
        });
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    println!("Agent: {}", config.name);
    println!("Model: {}", config.model);
    if is_running {
        println!("Status: running (container: {})", container_name);
    } else {
        println!("Status: stopped");
    }
    println!("Image: {} ({})", image, image_size);
    println!();

    if let Some(count) = memory_count {
        println!("Memories: {}", count);
    }

    if let Some(build) = last_build {
        println!();
        println!("Last build:");
        if let Some(ts) = build.get("timestamp").and_then(|t| t.as_str()) {
            println!("  Date: {}", ts);
        }
        if let Some(model) = build.get("model").and_then(|m| m.as_str()) {
            println!("  Model: {}", model);
        }
        if let Some(cost) = build.get("estimated_cost_usd").and_then(|c| c.as_f64()) {
            let input = build
                .get("input_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let output = build
                .get("output_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            println!(
                "  Cost: ${:.2} ({}K input + {}K output tokens)",
                cost,
                input / 1000,
                output / 1000
            );
        }
        if let Some(memories) = build
            .get("memories_created")
            .and_then(|m| m.as_u64())
        {
            println!("  Memories created: {}", memories);
        }
    }

    Ok(())
}
