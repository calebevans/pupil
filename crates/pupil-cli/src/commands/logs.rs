use clap::Args;

use crate::agent_config::resolve_agent;
use crate::container::{self, ContainerId};
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct LogsArgs {
    pub name: Option<String>,

    #[arg(short, long)]
    pub follow: bool,

    #[arg(long, default_value = "100")]
    pub tail: usize,
}

pub async fn execute(args: LogsArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let container_name = format!("pupil-{}", config.name);

    if args.follow {
        let status = tokio::process::Command::new(runtime.name())
            .args([
                "logs",
                "--follow",
                "--tail",
                &args.tail.to_string(),
                &container_name,
            ])
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .await?;

        if !status.success() {
            return Err(CliError::ContainerRuntimeError {
                message: format!(
                    "Failed to follow logs for container '{}'",
                    container_name
                ),
            });
        }
    } else {
        let container_id = ContainerId(container_name.clone());
        let output = runtime
            .logs(&container_id, false, args.tail)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to get logs: {}", e),
            })?;

        print!("{}", output);
    }

    Ok(())
}
