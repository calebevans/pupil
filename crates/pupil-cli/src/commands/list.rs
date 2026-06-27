use clap::Args;

use crate::container;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(serde::Serialize)]
struct AgentRow {
    name: String,
    tag: String,
    size: String,
    last_built: String,
}

pub async fn execute(args: ListArgs) -> Result<(), CliError> {
    let runtime = container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let output = tokio::process::Command::new(runtime.name())
        .args([
            "images",
            "--filter",
            "reference=pupil-agent-*",
            "--format",
            "{{.Repository}}\t{{.Tag}}\t{{.Size}}\t{{.CreatedAt}}",
        ])
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    if lines.is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!("No agents found. Create one with `pupil create <name>`.");
        }
        return Ok(());
    }

    let mut rows: Vec<AgentRow> = Vec::new();
    for line in &lines {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let agent_name = parts[0]
            .strip_prefix("pupil-agent-")
            .unwrap_or(parts[0])
            .to_string();

        rows.push(AgentRow {
            name: agent_name,
            tag: parts[1].to_string(),
            size: parts[2].to_string(),
            last_built: parts[3].to_string(),
        });
    }

    if args.json {
        let json = serde_json::to_string_pretty(&rows)?;
        println!("{}", json);
    } else {
        println!(
            "{:<20} {:<10} {:<10} {}",
            "NAME", "TAG", "SIZE", "LAST BUILT"
        );
        println!("{}", "-".repeat(70));
        for row in &rows {
            println!(
                "{:<20} {:<10} {:<10} {}",
                row.name, row.tag, row.size, row.last_built
            );
        }
    }

    Ok(())
}
