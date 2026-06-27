use clap::Args;

use crate::config::GlobalConfig;
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct CreateArgs {
    pub name: String,

    #[arg(short, long, default_value = "minimal")]
    pub template: String,

    #[arg(short, long)]
    pub model: Option<String>,
}

pub async fn execute(args: CreateArgs) -> Result<(), CliError> {
    validate_agent_name(&args.name)?;

    let target = std::env::current_dir()?.join(&args.name);
    if target.exists() {
        return Err(CliError::ConfigInvalid {
            message: format!("Directory already exists: {}", target.display()),
        });
    }

    let template_yaml = match args.template.as_str() {
        "minimal" => include_str!("../templates/minimal.yaml"),
        "full" => include_str!("../templates/full.yaml"),
        "knowledge-base" => include_str!("../templates/knowledge-base.yaml"),
        "chatbot" => include_str!("../templates/chatbot.yaml"),
        other => {
            return Err(CliError::ConfigInvalid {
                message: format!(
                    "Unknown template: '{}'. Available: minimal, full, knowledge-base, chatbot",
                    other
                ),
            });
        }
    };

    let global_config = GlobalConfig::load().unwrap_or_default();
    let model = args
        .model
        .or(global_config.default_model)
        .unwrap_or_else(|| "claude-sonnet-4-6".to_string());

    let yaml_content = template_yaml
        .replace("{name}", &args.name)
        .replace("{model}", &model);

    std::fs::create_dir_all(target.join("curriculum"))?;
    std::fs::write(target.join("pupil.yaml"), &yaml_content)?;

    println!("Created agent '{}' at {}", args.name, target.display());
    println!();
    println!("Next steps:");
    println!("  cd {}", args.name);
    println!("  pupil teach <files or URLs>    # add content to curriculum");
    println!("  pupil build                    # learn the curriculum");
    println!("  pupil run                      # chat with the agent");

    Ok(())
}

fn validate_agent_name(name: &str) -> Result<(), CliError> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::ConfigInvalid {
            message: "Agent name must be 1-64 characters".to_string(),
        });
    }
    let valid = regex::Regex::new(r"^[a-z0-9][a-z0-9-]*[a-z0-9]$").unwrap();
    if name.len() == 1 {
        if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err(CliError::ConfigInvalid {
                message: format!(
                    "Invalid agent name: '{}'. Must contain only lowercase letters, digits, and hyphens.",
                    name
                ),
            });
        }
    } else if !valid.is_match(name) {
        return Err(CliError::ConfigInvalid {
            message: format!(
                "Invalid agent name: '{}'. Must start and end with a letter or digit, and contain only lowercase letters, digits, and hyphens.",
                name
            ),
        });
    }
    let reserved = ["base", "runtime", "build", "dev"];
    if reserved.contains(&name) {
        return Err(CliError::ConfigInvalid {
            message: format!("'{}' is a reserved name. Choose a different name.", name),
        });
    }
    Ok(())
}
