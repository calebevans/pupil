use clap::Args;
use clap_complete::Shell;

use crate::error::CliError;

#[derive(Args, Debug)]
pub struct CompletionsArgs {
    pub shell: Shell,
}

pub fn execute(args: CompletionsArgs, cmd: &mut clap::Command) -> Result<(), CliError> {
    let mut stdout = std::io::stdout();
    clap_complete::generate(args.shell, cmd, "pupil", &mut stdout);
    Ok(())
}
