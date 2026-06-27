use clap::{Args, Subcommand};

use crate::error::CliError;

#[derive(Args, Debug)]
pub struct RouterArgs {
    #[command(subcommand)]
    pub action: RouterAction,
}

#[derive(Subcommand, Debug)]
pub enum RouterAction {
    Start,
    Stop,
    Status,
    Test {
        query: String,
    },
    Add {
        name: String,
        url: String,
    },
    Remove {
        name: String,
    },
    GenerateConfig,
}

pub async fn execute(_args: RouterArgs) -> Result<(), CliError> {
    println!("pupil router is not yet implemented. Coming in Phase 5.");
    Ok(())
}
