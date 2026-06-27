use clap::Args;
use std::path::PathBuf;

use crate::agent_config::{image_ref, resolve_agent};
use crate::container;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct ExportArgs {
    pub name: Option<String>,

    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub tag: Option<String>,
}

pub async fn execute(args: ExportArgs) -> Result<(), CliError> {
    let (_, config) = resolve_agent(args.name.as_deref())?;
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let tag = args.tag.as_deref().unwrap_or("latest");
    let image = image_ref(&config.name, Some(tag));

    let image_check = tokio::process::Command::new(runtime.name())
        .args(["image", "inspect", &image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;

    if !image_check.success() {
        return Err(CliError::ImageNotFound {
            name: config.name.clone(),
        });
    }

    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(format!("{}.tar", config.name)));

    if output.exists() {
        eprintln!(
            "Warning: Overwriting existing file: {}",
            output.display()
        );
    }

    println!("Exporting {} to {} ...", image, output.display());

    let save_output = tokio::process::Command::new(runtime.name())
        .args(["save", "-o", &output.to_string_lossy(), &image])
        .output()
        .await?;

    if !save_output.status.success() {
        return Err(CliError::ContainerRuntimeError {
            message: format!(
                "Export failed: {}",
                String::from_utf8_lossy(&save_output.stderr)
            ),
        });
    }

    let file_size = std::fs::metadata(&output)?.len();
    println!(
        "Exported: {} ({:.1}MB)",
        output.display(),
        file_size as f64 / 1_048_576.0
    );

    Ok(())
}
