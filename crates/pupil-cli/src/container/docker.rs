use std::path::{Path, PathBuf};

use super::{
    build_run_args, execute_runtime_command, ContainerId, ContainerError, ContainerRuntime, Output,
    Result, RunOptions,
};

#[derive(Debug, Clone)]
pub struct DockerRuntime {
    binary: PathBuf,
}

impl DockerRuntime {
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
}

#[async_trait::async_trait]
impl ContainerRuntime for DockerRuntime {
    fn name(&self) -> &str {
        "docker"
    }

    fn binary_path(&self) -> &Path {
        &self.binary
    }

    async fn run(&self, image: &str, opts: &RunOptions) -> Result<ContainerId> {
        let run_args = build_run_args(image, opts);
        let mut full_args = vec!["run".to_string()];
        full_args.extend(run_args);
        let refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();

        let output = execute_runtime_command(&self.binary, "run", &refs).await?;
        let id = output.stdout.trim().to_string();
        if id.is_empty() {
            return Err(ContainerError::CommandFailed {
                operation: "run".to_string(),
                code: 0,
                stderr: "docker run produced no container ID on stdout".to_string(),
            });
        }
        tracing::info!(container_id = %&id[..12.min(id.len())], image = %image,
            "container started");
        Ok(ContainerId(id))
    }

    async fn exec(
        &self,
        id: &ContainerId,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output> {
        let mut args: Vec<String> = vec!["exec".to_string()];
        for (key, value) in env {
            args.push("-e".to_string());
            args.push(format!("{}={}", key, value));
        }
        args.push(id.0.clone());
        for part in command {
            args.push(part.to_string());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_runtime_command(&self.binary, "exec", &refs).await
    }

    async fn exec_streaming(
        &self,
        id: &ContainerId,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output> {
        let mut args: Vec<String> = vec!["exec".to_string()];
        for (key, value) in env {
            args.push("-e".to_string());
            args.push(format!("{}={}", key, value));
        }
        args.push(id.0.clone());
        for part in command {
            args.push(part.to_string());
        }

        tracing::debug!(
            cmd = %format!("{} {}", self.binary.display(), args.join(" ")),
            "executing container command (streaming)"
        );

        let output = tokio::task::spawn_blocking({
            let binary = self.binary.clone();
            let args = args.clone();
            move || {
                let output = std::process::Command::new(&binary)
                    .args(&args)
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .output()
                    .map_err(|e| ContainerError::SpawnFailed {
                        command: format!("{} exec", binary.display()),
                        source: e,
                    })?;

                let code = output.status.code().unwrap_or(-1);
                if code != 0 && code != -1 {
                    return Err(ContainerError::CommandFailed {
                        operation: "exec".to_string(),
                        code,
                        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                    });
                }

                Ok(Output {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: code,
                })
            }
        })
        .await
        .expect("blocking task panicked");

        output
    }

    async fn commit(&self, id: &ContainerId, image_ref: &str) -> Result<()> {
        let args = ["commit", id.0.as_str(), image_ref];
        execute_runtime_command(&self.binary, "commit", &args).await?;
        tracing::info!(container = %id, image_ref = %image_ref,
            "container committed as image");
        Ok(())
    }

    async fn build(
        &self,
        context: &Path,
        dockerfile: &Path,
        tag: &str,
        platform: &str,
    ) -> Result<()> {
        let context_str = context.to_string_lossy().to_string();
        let dockerfile_str = dockerfile.to_string_lossy().to_string();
        let args = [
            "build",
            "-f",
            &dockerfile_str,
            "-t",
            tag,
            "--platform",
            platform,
            &context_str,
        ];
        execute_runtime_command(&self.binary, "build", &args).await?;
        tracing::info!(tag = %tag, platform = %platform, "image built");
        Ok(())
    }

    async fn push(&self, image_ref: &str) -> Result<()> {
        let args = ["push", image_ref];
        execute_runtime_command(&self.binary, "push", &args).await?;
        tracing::info!(image_ref = %image_ref, "image pushed");
        Ok(())
    }

    async fn pull(&self, image_ref: &str) -> Result<()> {
        let args = ["pull", image_ref];
        execute_runtime_command(&self.binary, "pull", &args).await?;
        tracing::info!(image_ref = %image_ref, "image pulled");
        Ok(())
    }

    async fn rm(&self, id: &ContainerId, force: bool) -> Result<()> {
        let mut args_vec = vec!["rm"];
        if force {
            args_vec.push("-f");
        }
        args_vec.push(id.0.as_str());
        execute_runtime_command(&self.binary, "rm", &args_vec).await?;
        tracing::info!(container = %id, force = force, "container removed");
        Ok(())
    }

    async fn cp(&self, id: &ContainerId, src: &str, dst: &Path) -> Result<()> {
        let container_path = format!("{}:{}", id.0, src);
        let dst_str = dst.to_string_lossy().to_string();
        let args = ["cp", &container_path, &dst_str];
        execute_runtime_command(&self.binary, "cp", &args).await?;
        tracing::debug!(container = %id, src = %src, dst = %dst.display(),
            "files copied from container");
        Ok(())
    }

    async fn cp_to(&self, id: &ContainerId, src: &Path, dst: &str) -> Result<()> {
        let src_str = src.to_string_lossy().to_string();
        let container_path = format!("{}:{}", id.0, dst);
        let args = ["cp", &src_str, &container_path];
        execute_runtime_command(&self.binary, "cp", &args).await?;
        tracing::debug!(container = %id, src = %src.display(), dst = %dst,
            "files copied to container");
        Ok(())
    }

    async fn logs(
        &self,
        id: &ContainerId,
        follow: bool,
        tail: usize,
    ) -> Result<String> {
        let tail_str = tail.to_string();
        let mut args = vec!["logs"];
        if follow {
            args.push("--follow");
        }
        args.push("--tail");
        args.push(&tail_str);
        args.push(id.0.as_str());

        let output = execute_runtime_command(&self.binary, "logs", &args).await?;
        let combined = if output.stderr.is_empty() {
            output.stdout
        } else if output.stdout.is_empty() {
            output.stderr
        } else {
            format!("{}\n{}", output.stdout, output.stderr)
        };
        Ok(combined)
    }
}
