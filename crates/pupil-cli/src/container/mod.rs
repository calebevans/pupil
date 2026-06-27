mod docker;
mod podman;
pub mod compose;
pub mod image;

pub use docker::DockerRuntime;
pub use podman::PodmanRuntime;

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContainerError {
    #[error("container runtime not found: neither `docker` nor `podman` is in PATH")]
    RuntimeNotFound,

    #[error("container runtime `{runtime}` (from PUPIL_CONTAINER_RUNTIME) not found in PATH")]
    SpecifiedRuntimeNotFound { runtime: String },

    #[error(
        "invalid PUPIL_CONTAINER_RUNTIME value `{value}`: expected `docker` or `podman`"
    )]
    InvalidRuntimeValue { value: String },

    #[error("{operation} failed (exit code {code}): {stderr}")]
    CommandFailed {
        operation: String,
        code: i32,
        stderr: String,
    },

    #[error("{operation} was killed by a signal")]
    CommandKilled { operation: String },

    #[error("failed to spawn `{command}`: {source}")]
    SpawnFailed {
        command: String,
        source: std::io::Error,
    },

    #[error("{operation} timed out after {seconds}s")]
    Timeout { operation: String, seconds: u64 },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("OCI image error: {0}")]
    OciError(String),

    #[error("serialization error: {0}")]
    SerdeError(String),
}

pub type Result<T> = std::result::Result<T, ContainerError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContainerId(pub String);

impl ContainerId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContainerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let short = if self.0.len() > 12 {
            &self.0[..12]
        } else {
            &self.0
        };
        write!(f, "{}", short)
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub name: Option<String>,
    pub env: HashMap<String, String>,
    pub volumes: Vec<String>,
    pub ports: Vec<String>,
    pub network: Option<String>,
    pub detach: bool,
    pub remove_on_exit: bool,
    pub read_only: bool,
    pub tmpfs: Vec<String>,
    pub cap_drop_all: bool,
    pub security_opts: Vec<String>,
    pub entrypoint: Option<String>,
    pub command: Vec<String>,
    pub labels: HashMap<String, String>,
    pub platform: Option<String>,
    pub extra_flags: Vec<String>,
    pub add_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Docker,
    Podman,
}

impl std::fmt::Display for RuntimeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeKind::Docker => write!(f, "docker"),
            RuntimeKind::Podman => write!(f, "podman"),
        }
    }
}

#[async_trait::async_trait]
pub trait ContainerRuntime: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &str;
    fn binary_path(&self) -> &Path;
    async fn run(&self, image: &str, opts: &RunOptions) -> Result<ContainerId>;
    async fn exec(
        &self,
        id: &ContainerId,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output>;
    async fn exec_streaming(
        &self,
        id: &ContainerId,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output>;
    async fn commit(&self, id: &ContainerId, image_ref: &str) -> Result<()>;
    async fn build(
        &self,
        context: &Path,
        dockerfile: &Path,
        tag: &str,
        platform: &str,
    ) -> Result<()>;
    async fn push(&self, image_ref: &str) -> Result<()>;
    async fn pull(&self, image_ref: &str) -> Result<()>;
    async fn rm(&self, id: &ContainerId, force: bool) -> Result<()>;
    async fn cp(&self, id: &ContainerId, src: &str, dst: &Path) -> Result<()>;
    async fn cp_to(&self, id: &ContainerId, src: &Path, dst: &str) -> Result<()>;
    async fn logs(
        &self,
        id: &ContainerId,
        follow: bool,
        tail: usize,
    ) -> Result<String>;
}

pub(crate) async fn execute_runtime_command(
    binary: &Path,
    operation: &str,
    args: &[&str],
) -> Result<Output> {
    let binary = binary.to_path_buf();
    let operation = operation.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    let result = tokio::task::spawn_blocking(move || {
        let redacted_args: Vec<String> = {
            let mut out = Vec::with_capacity(args.len());
            let mut redact_next = false;
            for arg in &args {
                if redact_next {
                    if let Some(eq_pos) = arg.find('=') {
                        let key = &arg[..eq_pos];
                        let key_upper = key.to_uppercase();
                        if key_upper.contains("KEY")
                            || key_upper.contains("SECRET")
                            || key_upper.contains("TOKEN")
                            || key_upper.contains("PASSWORD")
                        {
                            out.push(format!("{}=***REDACTED***", key));
                        } else {
                            out.push(arg.clone());
                        }
                    } else {
                        out.push(arg.clone());
                    }
                    redact_next = false;
                } else if arg == "-e" || arg == "--env" {
                    out.push(arg.clone());
                    redact_next = true;
                } else {
                    out.push(arg.clone());
                }
            }
            out
        };
        tracing::debug!(
            cmd = %format!("{} {}", binary.display(), redacted_args.join(" ")),
            "executing container command"
        );

        let output = std::process::Command::new(&binary)
            .args(&args)
            .output()
            .map_err(|e| ContainerError::SpawnFailed {
                command: format!("{} {}", binary.display(), redacted_args.join(" ")),
                source: e,
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            if code == -1 {
                return Err(ContainerError::CommandKilled {
                    operation: operation.clone(),
                });
            }
            return Err(ContainerError::CommandFailed {
                operation,
                code,
                stderr: stderr.trim().to_string(),
            });
        }

        Ok(Output {
            stdout: stdout.trim().to_string(),
            stderr: stderr.trim().to_string(),
            exit_code: 0,
        })
    })
    .await
    .expect("blocking task panicked");

    result
}

pub(crate) fn build_run_args(image: &str, opts: &RunOptions) -> Vec<String> {
    let mut args = Vec::new();

    if opts.detach {
        args.push("-d".to_string());
    }
    if let Some(ref name) = opts.name {
        args.push("--name".to_string());
        args.push(name.clone());
    }
    if opts.remove_on_exit {
        args.push("--rm".to_string());
    }
    if opts.read_only {
        args.push("--read-only".to_string());
    }
    if opts.cap_drop_all {
        args.push("--cap-drop=ALL".to_string());
    }
    for opt in &opts.security_opts {
        args.push("--security-opt".to_string());
        args.push(opt.clone());
    }
    for path in &opts.tmpfs {
        args.push("--tmpfs".to_string());
        args.push(path.clone());
    }
    for (key, value) in &opts.env {
        args.push("-e".to_string());
        args.push(format!("{}={}", key, value));
    }
    for vol in &opts.volumes {
        args.push("-v".to_string());
        args.push(vol.clone());
    }
    for port in &opts.ports {
        args.push("-p".to_string());
        args.push(port.clone());
    }
    if let Some(ref net) = opts.network {
        args.push("--network".to_string());
        args.push(net.clone());
    }
    if let Some(ref ep) = opts.entrypoint {
        args.push("--entrypoint".to_string());
        args.push(ep.clone());
    }
    if let Some(ref plat) = opts.platform {
        args.push("--platform".to_string());
        args.push(plat.clone());
    }
    for (key, value) in &opts.labels {
        args.push("--label".to_string());
        args.push(format!("{}={}", key, value));
    }
    if let Some(ref host) = opts.add_host {
        args.push("--add-host".to_string());
        args.push(host.clone());
    }
    for flag in &opts.extra_flags {
        args.push(flag.clone());
    }
    args.push(image.to_string());
    for cmd_part in &opts.command {
        args.push(cmd_part.clone());
    }

    args
}

pub fn detect() -> Result<Box<dyn ContainerRuntime>> {
    if let Ok(value) = env::var("PUPIL_CONTAINER_RUNTIME") {
        let value_lower = value.to_lowercase();
        match value_lower.as_str() {
            "docker" => {
                let path = which::which("docker").map_err(|_| {
                    ContainerError::SpecifiedRuntimeNotFound {
                        runtime: "docker".to_string(),
                    }
                })?;
                tracing::info!(runtime = "docker", path = %path.display(),
                    "using container runtime from PUPIL_CONTAINER_RUNTIME");
                return Ok(Box::new(DockerRuntime::new(path)));
            }
            "podman" => {
                let path = which::which("podman").map_err(|_| {
                    ContainerError::SpecifiedRuntimeNotFound {
                        runtime: "podman".to_string(),
                    }
                })?;
                tracing::info!(runtime = "podman", path = %path.display(),
                    "using container runtime from PUPIL_CONTAINER_RUNTIME");
                return Ok(Box::new(PodmanRuntime::new(path)));
            }
            _ => {
                return Err(ContainerError::InvalidRuntimeValue {
                    value: value.clone(),
                });
            }
        }
    }

    if let Ok(path) = which::which("docker") {
        if docker_socket_accessible() {
            tracing::info!(runtime = "docker", path = %path.display(),
                "detected docker in PATH");
            return Ok(Box::new(DockerRuntime::new(path)));
        } else {
            tracing::warn!(
                "docker binary found at {} but Docker socket is not accessible; \
                 trying podman",
                path.display()
            );
        }
    }

    if let Ok(path) = which::which("podman") {
        tracing::info!(runtime = "podman", path = %path.display(),
            "detected podman in PATH");
        return Ok(Box::new(PodmanRuntime::new(path)));
    }

    if docker_socket_accessible() {
        let candidates = ["/usr/bin/docker", "/usr/local/bin/docker"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                tracing::info!(runtime = "docker", path = %p.display(),
                    "found docker via socket probe");
                return Ok(Box::new(DockerRuntime::new(p)));
            }
        }
    }

    if podman_socket_accessible() {
        let candidates = ["/usr/bin/podman", "/usr/local/bin/podman"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                tracing::info!(runtime = "podman", path = %p.display(),
                    "found podman via socket probe");
                return Ok(Box::new(PodmanRuntime::new(p)));
            }
        }
    }

    Err(ContainerError::RuntimeNotFound)
}

fn docker_socket_accessible() -> bool {
    if let Ok(host) = env::var("DOCKER_HOST") {
        if let Some(path) = host.strip_prefix("unix://") {
            return Path::new(path).exists();
        }
        return true;
    }

    let paths = [
        PathBuf::from("/var/run/docker.sock"),
        dirs::home_dir()
            .map(|h| h.join(".docker/run/docker.sock"))
            .unwrap_or_default(),
    ];

    paths.iter().any(|p| p.exists())
}

fn podman_socket_accessible() -> bool {
    if let Ok(xdg) = env::var("XDG_RUNTIME_DIR") {
        let sock = PathBuf::from(&xdg).join("podman/podman.sock");
        if sock.exists() {
            return true;
        }
    }

    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        let sock = PathBuf::from(format!("/run/user/{}/podman/podman.sock", uid));
        if sock.exists() {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_rejects_invalid_runtime_value() {
        unsafe { env::set_var("PUPIL_CONTAINER_RUNTIME", "kubernetes") };
        let result = detect();
        assert!(matches!(
            result,
            Err(ContainerError::InvalidRuntimeValue { .. })
        ));
        unsafe { env::remove_var("PUPIL_CONTAINER_RUNTIME") };
    }

    #[test]
    fn detect_rejects_missing_specified_runtime() {
        unsafe { env::set_var("PUPIL_CONTAINER_RUNTIME", "podman") };
        if which::which("podman").is_err() {
            let result = detect();
            assert!(matches!(
                result,
                Err(ContainerError::SpecifiedRuntimeNotFound { .. })
            ));
        }
        unsafe { env::remove_var("PUPIL_CONTAINER_RUNTIME") };
    }

    #[test]
    fn minimal_run_args() {
        let opts = RunOptions {
            detach: true,
            ..Default::default()
        };
        let args = build_run_args("myimage:latest", &opts);
        assert_eq!(args, vec!["-d", "myimage:latest"]);
    }

    #[test]
    fn full_run_args() {
        let opts = RunOptions {
            detach: true,
            name: Some("mycontainer".to_string()),
            read_only: true,
            cap_drop_all: true,
            security_opts: vec!["no-new-privileges:true".to_string()],
            tmpfs: vec!["/tmp".to_string()],
            env: HashMap::from([("KEY".to_string(), "value".to_string())]),
            volumes: vec!["data:/data".to_string()],
            ports: vec!["8080:8080".to_string()],
            network: Some("mynet".to_string()),
            ..Default::default()
        };
        let args = build_run_args("myimage:latest", &opts);
        assert!(args.contains(&"-d".to_string()));
        assert!(args.contains(&"--name".to_string()));
        assert!(args.contains(&"mycontainer".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(args.contains(&"--cap-drop=ALL".to_string()));
        assert!(args.contains(&"--security-opt".to_string()));
        assert!(args.contains(&"--tmpfs".to_string()));
        assert!(args.contains(&"-e".to_string()));
        assert!(args.contains(&"KEY=value".to_string()));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.contains(&"data:/data".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"8080:8080".to_string()));
        assert!(args.contains(&"--network".to_string()));
        assert!(args.contains(&"mynet".to_string()));
        let image_pos = args.iter().position(|a| a == "myimage:latest").unwrap();
        assert_eq!(image_pos, args.len() - 1);
    }

    #[test]
    fn container_id_display_truncates() {
        let id = ContainerId("abcdef1234567890abcdef".to_string());
        assert_eq!(format!("{}", id), "abcdef123456");
    }

    #[test]
    fn container_id_display_short() {
        let id = ContainerId("abc".to_string());
        assert_eq!(format!("{}", id), "abc");
    }
}
