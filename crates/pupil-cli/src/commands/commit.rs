use clap::Args;

use crate::agent_config::{image_ref, resolve_agent};
use crate::container;
use crate::error::CliError;

use super::run::is_container_running;

#[derive(Args, Debug)]
pub struct CommitArgs {
    pub name: Option<String>,

    #[arg(long)]
    pub tag: Option<String>,

    #[arg(short, long)]
    pub message: Option<String>,
}

pub async fn execute(args: CommitArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let container_name = format!("pupil-{}", config.name);
    if !is_container_running(runtime.as_ref(), &container_name).await? {
        return Err(CliError::RunContainerFailed {
            message: format!(
                "Agent '{}' is not running. Start it with `pupil run {}`.",
                config.name, config.name
            ),
        });
    }

    let tag = args.tag.as_deref().unwrap_or("latest");
    let new_image = image_ref(&config.name, Some(tag));

    println!("Committing runtime state to {} ...", new_image);

    let mut commit_args_vec = vec!["commit".to_string()];
    if let Some(ref msg) = args.message {
        commit_args_vec.push("-m".to_string());
        commit_args_vec.push(msg.clone());
    }
    commit_args_vec.push(container_name.clone());
    commit_args_vec.push(new_image.clone());

    let output = tokio::process::Command::new(runtime.name())
        .args(&commit_args_vec)
        .output()
        .await?;

    if !output.status.success() {
        return Err(CliError::CommitFailed {
            message: format!(
                "Commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    println!("Committed: {}", new_image);
    println!();
    println!("The image now includes all runtime-learned knowledge.");
    println!("Push it with: pupil push {} <registry>", config.name);

    Ok(())
}
