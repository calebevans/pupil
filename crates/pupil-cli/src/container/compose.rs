use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{execute_runtime_command, ContainerError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuProfile {
    None,
    Nvidia,
}

#[derive(Debug, Clone)]
pub struct ComposeConfig {
    pub agent_image: String,
    pub agent_name: String,
    pub agent_env: HashMap<String, String>,
    pub agent_volumes: Vec<String>,
    pub agent_ports: Vec<String>,
    pub agent_read_only: bool,
    pub embedding_model: String,
    pub ollama_image: String,
    pub gpu: GpuProfile,
    pub extra_services: HashMap<String, serde_yml::Value>,
    pub agent_entrypoint: Option<Vec<String>>,
    pub agent_command: Option<Vec<String>>,
    pub agent_tmpfs: Vec<String>,
}

impl Default for ComposeConfig {
    fn default() -> Self {
        Self {
            agent_image: String::new(),
            agent_name: "agent".to_string(),
            agent_env: HashMap::new(),
            agent_volumes: Vec::new(),
            agent_ports: Vec::new(),
            agent_read_only: true,
            embedding_model: "embeddinggemma".to_string(),
            ollama_image: "ollama/ollama:latest".to_string(),
            gpu: GpuProfile::None,
            extra_services: HashMap::new(),
            agent_entrypoint: None,
            agent_command: None,
            agent_tmpfs: vec!["/tmp".to_string()],
        }
    }
}

pub fn generate(config: &ComposeConfig) -> Result<String> {
    let mut compose = serde_yml::Value::Mapping(serde_yml::Mapping::new());

    // ---- Agent service ----
    let mut agent_svc = serde_yml::Mapping::new();
    agent_svc.insert(
        serde_yml::Value::String("image".to_string()),
        serde_yml::Value::String(config.agent_image.clone()),
    );

    if config.agent_read_only {
        agent_svc.insert(
            serde_yml::Value::String("read_only".to_string()),
            serde_yml::Value::Bool(true),
        );
    }

    if !config.agent_tmpfs.is_empty() {
        let tmpfs_seq: Vec<serde_yml::Value> = config
            .agent_tmpfs
            .iter()
            .map(|t| serde_yml::Value::String(t.clone()))
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("tmpfs".to_string()),
            serde_yml::Value::Sequence(tmpfs_seq),
        );
    }

    if !config.agent_volumes.is_empty() {
        let vols: Vec<serde_yml::Value> = config
            .agent_volumes
            .iter()
            .map(|v| serde_yml::Value::String(v.clone()))
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("volumes".to_string()),
            serde_yml::Value::Sequence(vols),
        );
    }

    agent_svc.insert(
        serde_yml::Value::String("cap_drop".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String("ALL".to_string())]),
    );
    agent_svc.insert(
        serde_yml::Value::String("security_opt".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String(
            "no-new-privileges:true".to_string(),
        )]),
    );

    if !config.agent_env.is_empty() {
        let env_seq: Vec<serde_yml::Value> = config
            .agent_env
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    serde_yml::Value::String(k.clone())
                } else {
                    serde_yml::Value::String(format!("{}={}", k, v))
                }
            })
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("environment".to_string()),
            serde_yml::Value::Sequence(env_seq),
        );
    }

    if !config.agent_ports.is_empty() {
        let ports: Vec<serde_yml::Value> = config
            .agent_ports
            .iter()
            .map(|p| serde_yml::Value::String(p.clone()))
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("ports".to_string()),
            serde_yml::Value::Sequence(ports),
        );
    }

    agent_svc.insert(
        serde_yml::Value::String("networks".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String(
            "agent-net".to_string(),
        )]),
    );

    let mut depends = serde_yml::Mapping::new();
    let mut ollama_dep = serde_yml::Mapping::new();
    ollama_dep.insert(
        serde_yml::Value::String("condition".to_string()),
        serde_yml::Value::String("service_healthy".to_string()),
    );
    depends.insert(
        serde_yml::Value::String("ollama".to_string()),
        serde_yml::Value::Mapping(ollama_dep),
    );
    agent_svc.insert(
        serde_yml::Value::String("depends_on".to_string()),
        serde_yml::Value::Mapping(depends),
    );

    if let Some(ref ep) = config.agent_entrypoint {
        let ep_seq: Vec<serde_yml::Value> = ep
            .iter()
            .map(|s| serde_yml::Value::String(s.clone()))
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("entrypoint".to_string()),
            serde_yml::Value::Sequence(ep_seq),
        );
    }

    if let Some(ref cmd) = config.agent_command {
        let cmd_seq: Vec<serde_yml::Value> = cmd
            .iter()
            .map(|s| serde_yml::Value::String(s.clone()))
            .collect();
        agent_svc.insert(
            serde_yml::Value::String("command".to_string()),
            serde_yml::Value::Sequence(cmd_seq),
        );
    }

    // ---- Ollama service ----
    let mut ollama_svc = serde_yml::Mapping::new();
    ollama_svc.insert(
        serde_yml::Value::String("image".to_string()),
        serde_yml::Value::String(config.ollama_image.clone()),
    );

    ollama_svc.insert(
        serde_yml::Value::String("volumes".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String(
            "ollama-models:/root/.ollama".to_string(),
        )]),
    );

    ollama_svc.insert(
        serde_yml::Value::String("environment".to_string()),
        serde_yml::Value::Mapping({
            let mut env = serde_yml::Mapping::new();
            env.insert(
                serde_yml::Value::String("OLLAMA_NUM_PARALLEL".to_string()),
                serde_yml::Value::String("4".to_string()),
            );
            env.insert(
                serde_yml::Value::String("OLLAMA_MAX_LOADED_MODELS".to_string()),
                serde_yml::Value::String("1".to_string()),
            );
            env
        }),
    );

    let model_pull_script = format!(
        "ollama serve &\n\
         PID=$!\n\
         timeout 60 sh -c 'until curl -sf http://localhost:11434/api/tags \
         >/dev/null 2>&1; do sleep 1; done'\n\
         ollama pull {model}\n\
         wait $PID",
        model = config.embedding_model
    );

    ollama_svc.insert(
        serde_yml::Value::String("entrypoint".to_string()),
        serde_yml::Value::Sequence(vec![
            serde_yml::Value::String("/bin/sh".to_string()),
            serde_yml::Value::String("-c".to_string()),
        ]),
    );
    ollama_svc.insert(
        serde_yml::Value::String("command".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String(model_pull_script)]),
    );

    let mut healthcheck = serde_yml::Mapping::new();
    healthcheck.insert(
        serde_yml::Value::String("test".to_string()),
        serde_yml::Value::Sequence(vec![
            serde_yml::Value::String("CMD-SHELL".to_string()),
            serde_yml::Value::String(
                "curl -f http://localhost:11434/api/tags || exit 1".to_string(),
            ),
        ]),
    );
    healthcheck.insert(
        serde_yml::Value::String("interval".to_string()),
        serde_yml::Value::String("10s".to_string()),
    );
    healthcheck.insert(
        serde_yml::Value::String("timeout".to_string()),
        serde_yml::Value::String("5s".to_string()),
    );
    healthcheck.insert(
        serde_yml::Value::String("retries".to_string()),
        serde_yml::Value::Number(serde_yml::Number::from(5)),
    );
    healthcheck.insert(
        serde_yml::Value::String("start_period".to_string()),
        serde_yml::Value::String("60s".to_string()),
    );
    ollama_svc.insert(
        serde_yml::Value::String("healthcheck".to_string()),
        serde_yml::Value::Mapping(healthcheck),
    );

    ollama_svc.insert(
        serde_yml::Value::String("networks".to_string()),
        serde_yml::Value::Sequence(vec![serde_yml::Value::String(
            "agent-net".to_string(),
        )]),
    );

    if config.gpu == GpuProfile::Nvidia {
        let mut deploy = serde_yml::Mapping::new();
        let mut resources = serde_yml::Mapping::new();
        let mut reservations = serde_yml::Mapping::new();
        let mut device = serde_yml::Mapping::new();

        device.insert(
            serde_yml::Value::String("driver".to_string()),
            serde_yml::Value::String("nvidia".to_string()),
        );
        device.insert(
            serde_yml::Value::String("count".to_string()),
            serde_yml::Value::Number(serde_yml::Number::from(1)),
        );
        device.insert(
            serde_yml::Value::String("capabilities".to_string()),
            serde_yml::Value::Sequence(vec![serde_yml::Value::String(
                "gpu".to_string(),
            )]),
        );

        reservations.insert(
            serde_yml::Value::String("devices".to_string()),
            serde_yml::Value::Sequence(vec![serde_yml::Value::Mapping(device)]),
        );
        resources.insert(
            serde_yml::Value::String("reservations".to_string()),
            serde_yml::Value::Mapping(reservations),
        );
        deploy.insert(
            serde_yml::Value::String("resources".to_string()),
            serde_yml::Value::Mapping(resources),
        );
        ollama_svc.insert(
            serde_yml::Value::String("deploy".to_string()),
            serde_yml::Value::Mapping(deploy),
        );
    }

    // ---- Assemble top-level ----
    let mut services = serde_yml::Mapping::new();
    services.insert(
        serde_yml::Value::String("agent".to_string()),
        serde_yml::Value::Mapping(agent_svc),
    );
    services.insert(
        serde_yml::Value::String("ollama".to_string()),
        serde_yml::Value::Mapping(ollama_svc),
    );

    for (name, value) in &config.extra_services {
        services.insert(
            serde_yml::Value::String(name.clone()),
            value.clone(),
        );
    }

    // Volumes
    let mut volumes = serde_yml::Mapping::new();
    for vol_str in &config.agent_volumes {
        if let Some(vol_name) = vol_str.split(':').next() {
            if !vol_name.starts_with('/') && !vol_name.starts_with('.') {
                volumes.insert(
                    serde_yml::Value::String(vol_name.to_string()),
                    serde_yml::Value::Null,
                );
            }
        }
    }
    volumes.insert(
        serde_yml::Value::String("ollama-models".to_string()),
        serde_yml::Value::Null,
    );

    // Networks
    let mut networks = serde_yml::Mapping::new();
    let mut agent_net = serde_yml::Mapping::new();
    agent_net.insert(
        serde_yml::Value::String("internal".to_string()),
        serde_yml::Value::Bool(true),
    );
    networks.insert(
        serde_yml::Value::String("agent-net".to_string()),
        serde_yml::Value::Mapping(agent_net),
    );

    let mapping = compose.as_mapping_mut().unwrap();
    mapping.insert(
        serde_yml::Value::String("services".to_string()),
        serde_yml::Value::Mapping(services),
    );
    mapping.insert(
        serde_yml::Value::String("volumes".to_string()),
        serde_yml::Value::Mapping(volumes),
    );
    mapping.insert(
        serde_yml::Value::String("networks".to_string()),
        serde_yml::Value::Mapping(networks),
    );

    serde_yml::to_string(&compose).map_err(|e| ContainerError::SerdeError(e.to_string()))
}

pub fn write_compose_file(config: &ComposeConfig, dir: &Path) -> Result<PathBuf> {
    let yaml = generate(config)?;
    let file_path = dir.join("docker-compose.yml");
    std::fs::write(&file_path, yaml)?;
    tracing::debug!(path = %file_path.display(), "compose file written");
    Ok(file_path)
}

fn detect_compose() -> Result<(PathBuf, Vec<String>)> {
    if let Ok(docker_path) = which::which("docker") {
        let output = std::process::Command::new(&docker_path)
            .args(["compose", "version"])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                return Ok((docker_path, vec!["compose".to_string()]));
            }
        }
    }

    if let Ok(path) = which::which("docker-compose") {
        return Ok((path, vec![]));
    }

    if let Ok(podman_path) = which::which("podman") {
        let output = std::process::Command::new(&podman_path)
            .args(["compose", "version"])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                return Ok((podman_path, vec!["compose".to_string()]));
            }
        }
    }

    if let Ok(path) = which::which("podman-compose") {
        return Ok((path, vec![]));
    }

    Err(ContainerError::RuntimeNotFound)
}

pub async fn compose_up(compose_file: &Path, profile: Option<&str>) -> Result<()> {
    let (binary, prefix) = tokio::task::spawn_blocking(detect_compose)
        .await
        .map_err(|e| ContainerError::OciError(format!("compose detection task panicked: {e}")))??;
    let compose_path = compose_file.to_string_lossy().to_string();

    let mut args: Vec<String> = prefix;
    args.push("-f".to_string());
    args.push(compose_path);
    if let Some(p) = profile {
        args.push("--profile".to_string());
        args.push(p.to_string());
    }
    args.push("up".to_string());
    args.push("-d".to_string());

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    execute_runtime_command(&binary, "compose up", &refs).await?;
    tracing::info!(compose_file = %compose_file.display(), "compose services started");
    Ok(())
}

pub async fn compose_down(compose_file: &Path, remove_volumes: bool) -> Result<()> {
    let (binary, prefix) = tokio::task::spawn_blocking(detect_compose)
        .await
        .map_err(|e| ContainerError::OciError(format!("compose detection task panicked: {e}")))??;
    let compose_path = compose_file.to_string_lossy().to_string();

    let mut args: Vec<String> = prefix;
    args.push("-f".to_string());
    args.push(compose_path);
    args.push("down".to_string());
    if remove_volumes {
        args.push("--volumes".to_string());
    }

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    execute_runtime_command(&binary, "compose down", &refs).await?;
    tracing::info!(compose_file = %compose_file.display(), "compose services stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_default_compose() {
        let config = ComposeConfig {
            agent_image: "pupil-agent-test:latest".to_string(),
            agent_env: HashMap::from([
                ("ANTHROPIC_API_KEY".to_string(), String::new()),
                (
                    "OLLAMA_BASE_URL".to_string(),
                    "http://ollama:11434".to_string(),
                ),
            ]),
            agent_volumes: vec!["agent-data:/data".to_string()],
            ..Default::default()
        };

        let yaml = generate(&config).unwrap();

        assert!(yaml.contains("pupil-agent-test:latest"));
        assert!(yaml.contains("ollama/ollama:latest"));
        assert!(yaml.contains("agent-net"));
        assert!(yaml.contains("service_healthy"));
        assert!(yaml.contains("embeddinggemma"));
        assert!(yaml.contains("ollama-models"));
        assert!(yaml.contains("agent-data"));
        assert!(yaml.contains("read_only: true"));
        assert!(yaml.contains("cap_drop"));
        assert!(yaml.contains("ALL"));
        assert!(yaml.contains("no-new-privileges"));
        assert!(yaml.contains("internal: true"));
        assert!(!yaml.contains("nvidia"));
        assert!(!yaml.contains("capabilities"));
    }

    #[test]
    fn generate_with_gpu() {
        let config = ComposeConfig {
            agent_image: "test:latest".to_string(),
            gpu: GpuProfile::Nvidia,
            ..Default::default()
        };

        let yaml = generate(&config).unwrap();
        assert!(yaml.contains("nvidia"));
        assert!(yaml.contains("gpu"));
        assert!(yaml.contains("deploy"));
        assert!(yaml.contains("reservations"));
    }

    #[test]
    fn generate_with_custom_model() {
        let config = ComposeConfig {
            agent_image: "test:latest".to_string(),
            embedding_model: "nomic-embed-text".to_string(),
            ..Default::default()
        };

        let yaml = generate(&config).unwrap();
        assert!(yaml.contains("nomic-embed-text"));
        assert!(!yaml.contains("embeddinggemma"));
    }
}
