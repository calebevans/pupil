use clap::Args;
use std::path::PathBuf;

use crate::agent_config::image_ref;
use crate::container;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct ImportArgs {
    pub path: PathBuf,

    #[arg(long)]
    pub name: Option<String>,
}

pub async fn execute(args: ImportArgs) -> Result<(), CliError> {
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    if !args.path.exists() {
        return Err(CliError::ConfigInvalid {
            message: format!("File not found: {}", args.path.display()),
        });
    }

    println!("Importing from {} ...", args.path.display());

    let load_output = tokio::process::Command::new(runtime.name())
        .args(["load", "-i", &args.path.to_string_lossy()])
        .output()
        .await?;

    if !load_output.status.success() {
        return Err(CliError::ContainerRuntimeError {
            message: format!(
                "Import failed: {}",
                String::from_utf8_lossy(&load_output.stderr)
            ),
        });
    }

    let stdout = String::from_utf8_lossy(&load_output.stdout);

    let loaded_image = stdout
        .lines()
        .find(|line| line.contains("Loaded image"))
        .and_then(|line| line.split(": ").nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    if let Some(ref name) = args.name {
        let new_image = image_ref(name, None);
        let tag_output = tokio::process::Command::new(runtime.name())
            .args(["tag", &loaded_image, &new_image])
            .output()
            .await?;

        if !tag_output.status.success() {
            eprintln!(
                "Warning: failed to re-tag as '{}'. Image is available as '{}'.",
                new_image, loaded_image
            );
        } else {
            println!("Imported and registered as '{}'", name);
            println!("  Image: {}", new_image);
            println!("  Run with: pupil run {}", name);
            return Ok(());
        }
    }

    let inferred_name = loaded_image
        .split(':')
        .next()
        .unwrap_or(&loaded_image)
        .strip_prefix("pupil-agent-")
        .unwrap_or(&loaded_image);

    println!("Imported: {}", loaded_image);
    println!("  Run with: pupil run {}", inferred_name);

    Ok(())
}
