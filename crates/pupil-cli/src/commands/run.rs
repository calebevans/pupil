use clap::Args;
use std::collections::HashMap;

use crate::agent_config::{image_ref, resolve_agent, resolve_api_key};
use crate::container::{self, ContainerRuntime, RunOptions};
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct RunArgs {
    pub name: Option<String>,

    #[arg(short, long)]
    pub port: Option<u16>,

    #[arg(long)]
    pub with_ollama: bool,

    #[arg(short, long)]
    pub detach: bool,

    #[arg(short, long, value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    #[arg(long)]
    pub tag: Option<String>,
}

pub async fn execute(args: RunArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;

    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let tag = args.tag.as_deref().unwrap_or("latest");
    let image = image_ref(&config.name, Some(tag));
    if !image_exists(runtime.as_ref(), &image).await? {
        return Err(CliError::ImageNotFound {
            name: config.name.clone(),
        });
    }

    let (key_name, key_set) = resolve_api_key(&config.model);
    if !key_set {
        return Err(CliError::EnvVarMissing { name: key_name });
    }

    let mut env_vars: HashMap<String, String> = HashMap::new();

    if key_name == "VERTEX_API_KEY" {
        crate::agent_config::resolve_vertex_env(&mut env_vars);
    } else {
        let api_key_value = std::env::var(&key_name).unwrap_or_default();
        env_vars.insert(key_name.clone(), api_key_value);
    }

    env_vars.insert("RECALLD_STORAGE_DATA_DIR".to_string(), "/data/recalld".to_string());
    env_vars.insert("RECALLD_DAEMON_SOCKET".to_string(), "/data/recalld/recalld.sock".to_string());

    for env_str in &args.env {
        let (k, v) = env_str.split_once('=').ok_or_else(|| CliError::ConfigInvalid {
            message: format!(
                "Invalid env format: '{}'. Expected KEY=VALUE.",
                env_str
            ),
        })?;
        env_vars.insert(k.to_string(), v.to_string());
    }

    let volume_name = format!("pupil-{}-data", config.name);

    let mut command = vec!["pupil-agent".to_string()];
    if let Some(port) = args.port {
        command.push("--port".to_string());
        command.push(port.to_string());
        env_vars.insert("PUPIL_LOG_FORMAT".to_string(), "json".to_string());
    }

    if args.with_ollama {
        env_vars.insert(
            "OLLAMA_BASE_URL".to_string(),
            "http://ollama:11434".to_string(),
        );

        let compose_config = crate::container::compose::ComposeConfig {
            agent_image: image.clone(),
            agent_name: format!("pupil-{}", config.name),
            agent_env: env_vars.clone(),
            agent_volumes: vec![format!("{}:/data", volume_name)],
            agent_ports: if let Some(port) = args.port {
                vec![format!("{}:8080", port)]
            } else {
                Vec::new()
            },
            ..Default::default()
        };

        let compose_dir = tempfile::tempdir().map_err(|e| CliError::Io(e))?;
        let compose_file =
            crate::container::compose::write_compose_file(&compose_config, compose_dir.path())
                .map_err(|e| CliError::ContainerRuntimeError {
                    message: format!("Failed to write compose file: {}", e),
                })?;

        crate::container::compose::compose_up(&compose_file, None)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to start compose services: {}", e),
            })?;

        if args.detach {
            println!("Agent '{}' started in background.", config.name);
            if let Some(port) = args.port {
                println!("  HTTP API: http://localhost:{}", port);
            }
            println!("  Container: pupil-{}", config.name);
            println!("  View logs: pupil logs {}", config.name);
            println!(
                "  Stop: docker compose -f {} down",
                compose_file.display()
            );
            return Ok(());
        }

        let container_name = format!("pupil-{}", config.name);
        attach_to_container(runtime.as_ref(), &container_name).await?;
        return Ok(());
    }

    let container_name = format!("pupil-{}", config.name);

    if is_container_running(runtime.as_ref(), &container_name).await? {
        if args.detach {
            println!("Agent '{}' is already running.", config.name);
            return Ok(());
        }
        println!("Attaching to running agent '{}'...", config.name);
        attach_to_container(runtime.as_ref(), &container_name).await?;
        return Ok(());
    }

    let mut ports = Vec::new();
    if let Some(port) = args.port {
        ports.push(format!("{}:8080", port));
    }

    let _container_id = runtime
        .run(
            &image,
            &RunOptions {
                name: Some(container_name.clone()),
                env: env_vars,
                volumes: vec![format!("{}:/data", volume_name)],
                ports,
                read_only: true,
                tmpfs: vec!["/tmp".to_string()],
                cap_drop_all: true,
                security_opts: vec!["no-new-privileges:true".to_string()],
                detach: args.detach,
                remove_on_exit: !args.detach,
                command,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| CliError::RunContainerFailed {
            message: e.to_string(),
        })?;

    if args.detach {
        println!("Agent '{}' started in background.", config.name);
        if let Some(port) = args.port {
            println!("  HTTP API: http://localhost:{}", port);
        }
        println!("  Container: {}", container_name);
        println!("  View logs: pupil logs {}", config.name);
        println!("  Stop: {} stop {}", runtime.name(), container_name);
        return Ok(());
    }

    println!(
        "Agent '{}' ready. Type your message, or Ctrl+C to stop.",
        config.name
    );
    println!();
    attach_to_container(runtime.as_ref(), &container_name).await?;

    Ok(())
}

async fn image_exists(
    runtime: &dyn ContainerRuntime,
    image: &str,
) -> Result<bool, CliError> {
    let output = tokio::process::Command::new(runtime.binary_path())
        .args(["image", "inspect", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    Ok(output.success())
}

async fn attach_to_container(
    runtime: &dyn ContainerRuntime,
    container_name: &str,
) -> Result<(), CliError> {
    let status = tokio::process::Command::new(runtime.binary_path())
        .args(["attach", container_name])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        return Err(CliError::AgentCrashed {
            exit_code: status.code().unwrap_or(-1),
        });
    }

    Ok(())
}

pub async fn is_container_running(
    runtime: &dyn ContainerRuntime,
    name: &str,
) -> Result<bool, CliError> {
    let output = tokio::process::Command::new(runtime.binary_path())
        .args([
            "ps",
            "--filter",
            &format!("name={}", name),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .await?;
    let names = String::from_utf8_lossy(&output.stdout);
    Ok(names.lines().any(|line| line.trim() == name))
}
