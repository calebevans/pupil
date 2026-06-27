use clap::Args;

use crate::agent_config::image_ref;
use crate::container;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct PullArgs {
    pub registry_ref: String,

    #[arg(long)]
    pub name: Option<String>,
}

pub async fn execute(args: PullArgs) -> Result<(), CliError> {
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    println!("Pulling {} ...", args.registry_ref);
    runtime
        .pull(&args.registry_ref)
        .await
        .map_err(|e| CliError::PullFailed {
            reference: args.registry_ref.clone(),
            message: e.to_string(),
        })?;

    let local_name = if let Some(ref name) = args.name {
        name.clone()
    } else {
        let path = args
            .registry_ref
            .split('/')
            .last()
            .unwrap_or(&args.registry_ref);
        let name_part = path.split(':').next().unwrap_or(path);
        name_part
            .strip_prefix("pupil-agent-")
            .unwrap_or(name_part)
            .to_string()
    };

    let local_image = image_ref(&local_name, None);
    let tag_output = tokio::process::Command::new(runtime.name())
        .args(["tag", &args.registry_ref, &local_image])
        .output()
        .await?;

    if !tag_output.status.success() {
        return Err(CliError::PullFailed {
            reference: args.registry_ref,
            message: format!(
                "Failed to tag image as '{}': {}",
                local_image,
                String::from_utf8_lossy(&tag_output.stderr)
            ),
        });
    }

    println!("Pulled and registered as '{}'", local_name);
    println!("  Image: {}", local_image);
    println!("  Run with: pupil run {}", local_name);

    Ok(())
}
