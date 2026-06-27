#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod logging;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process;

use pupil_cli::commands;
use pupil_cli::config;
use pupil_cli::error::{self, CliError};

#[derive(Parser, Debug)]
#[command(
    name = "pupil",
    version,
    about = "Build and run teachable AI agents",
    long_about = "Pupil packages AI agents with their knowledge into container images.\n\
                   Define an agent with pupil.yaml, teach it with curriculum files,\n\
                   build it into a container, and distribute it through any OCI registry.",
    after_help = "Run `pupil <command> --help` for more information on a specific command."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[arg(short, long, global = true)]
    quiet: bool,

    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true, default_value = "auto")]
    color: ColorChoice,

    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create a new agent directory with pupil.yaml and curriculum/.
    Create(commands::create::CreateArgs),

    /// Add content to an agent's curriculum.
    Teach(commands::teach::TeachArgs),

    /// Build an agent: learn the curriculum and snapshot the image.
    Build(commands::build::BuildArgs),

    /// Run an agent: start the container and begin chatting.
    Run(commands::run::RunArgs),

    /// Push an agent image to an OCI registry.
    Push(commands::push::PushArgs),

    /// Pull an agent image from an OCI registry.
    Pull(commands::pull::PullArgs),

    /// List locally registered agents.
    List(commands::list::ListArgs),

    /// Export an agent as an OCI archive tar file.
    Export(commands::export::ExportArgs),

    /// Import an agent from an OCI archive tar file.
    Import(commands::import::ImportArgs),

    /// Show agent status, build info, and runtime usage.
    Status(commands::status::StatusArgs),

    /// Show logs from a running agent container.
    Logs(commands::logs::LogsArgs),

    /// Validate the environment: container runtime, API keys, Ollama.
    Doctor(commands::doctor::DoctorArgs),

    /// Get, set, or list global configuration values.
    Config(commands::config::ConfigArgs),

    /// Generate shell completion scripts.
    Completions(commands::completions::CompletionsArgs),

    /// Run tests against a built agent.
    Test(commands::test::TestArgs),

    /// Inspect learned memories: list, search, stats, quality, graph, diff.
    Inspect(commands::inspect::InspectArgs),

    /// Watch curriculum for changes and re-learn automatically.
    Watch(commands::watch::WatchArgs),

    /// Snapshot runtime volume state into a new image.
    Commit(commands::commit::CommitArgs),

    /// Check URL sources for changes and re-learn.
    Sync(commands::sync::SyncArgs),

    /// Manage the multi-agent router.
    Router(commands::router::RouterArgs),
}

fn main() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .unicode(true)
                .context_lines(3)
                .build(),
        )
    }))
    .expect("failed to install miette error hook");

    let cli = Cli::parse();

    let format = if cli.json {
        logging::LogFormat::Json
    } else {
        logging::LogFormat::from_env()
    };

    let verbosity = if cli.quiet { 0 } else { cli.verbose };
    logging::init_logging(format, verbosity);

    match cli.color {
        ColorChoice::Always => console::set_colors_enabled(true),
        ColorChoice::Never => console::set_colors_enabled(false),
        ColorChoice::Auto => {}
    }

    tracing::debug!(command = ?cli.command, "pupil starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let exit_code = runtime.block_on(async_main(cli));
    process::exit(exit_code);
}

async fn async_main(cli: Cli) -> i32 {
    if !matches!(
        cli.command,
        Command::Config(_) | Command::Completions(_) | Command::Doctor(_)
    ) {
        if std::env::var("PUPIL_SKIP_SETUP").unwrap_or_default() != "1"
            && !config::GlobalConfig::path().exists()
        {
            // Wizard not yet implemented; skip silently.
        }
    }

    let result: Result<(), CliError> = match cli.command {
        Command::Create(args) => commands::create::execute(args).await,
        Command::Teach(args) => commands::teach::execute(args).await,
        Command::Build(args) => commands::build::execute(args).await,
        Command::Run(args) => commands::run::execute(args).await,
        Command::Push(args) => commands::push::execute(args).await,
        Command::Pull(args) => commands::pull::execute(args).await,
        Command::List(args) => commands::list::execute(args).await,
        Command::Export(args) => commands::export::execute(args).await,
        Command::Import(args) => commands::import::execute(args).await,
        Command::Commit(args) => commands::commit::execute(args).await,
        Command::Status(args) => commands::status::execute(args).await,
        Command::Logs(args) => commands::logs::execute(args).await,
        Command::Doctor(args) => commands::doctor::execute(args).await,
        Command::Config(args) => commands::config::execute(args).await,
        Command::Completions(args) => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            commands::completions::execute(args, &mut cmd)
        }
        Command::Test(args) => commands::test::execute(args).await,
        Command::Inspect(args) => commands::inspect::execute(args).await,
        Command::Watch(args) => commands::watch::execute(args).await,
        Command::Sync(args) => commands::sync::execute(args).await,
        Command::Router(args) => commands::router::execute(args).await,
    };

    match result {
        Ok(()) => error::exit_code::OK,
        Err(e) => {
            eprintln!("{:?}", miette::Report::from(e));
            error::exit_code::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn create_command() {
        let cli = Cli::try_parse_from(["pupil", "create", "my-agent"]).unwrap();
        assert!(matches!(cli.command, Command::Create(_)));
    }

    #[test]
    fn create_with_options() {
        let cli = Cli::try_parse_from([
            "pupil",
            "create",
            "my-agent",
            "--template",
            "full",
            "--model",
            "claude-haiku-4",
        ])
        .unwrap();
        match cli.command {
            Command::Create(args) => {
                assert_eq!(args.name, "my-agent");
                assert_eq!(args.template, "full");
                assert_eq!(args.model.as_deref(), Some("claude-haiku-4"));
            }
            _ => panic!("expected Create command"),
        }
    }

    #[test]
    fn build_defaults() {
        let cli = Cli::try_parse_from(["pupil", "build"]).unwrap();
        match cli.command {
            Command::Build(args) => {
                assert!(args.name.is_none());
                assert!(!args.no_cache);
                assert!(!args.no_confirm);
                assert!(!args.dry_run);
            }
            _ => panic!("expected Build command"),
        }
    }

    #[test]
    fn global_verbose_flag() {
        let cli = Cli::try_parse_from(["pupil", "-vv", "doctor"]).unwrap();
        assert_eq!(cli.verbose, 2);
    }

    #[test]
    fn global_json_flag() {
        let cli = Cli::try_parse_from(["pupil", "--json", "list"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn completions_shells() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let cli = Cli::try_parse_from(["pupil", "completions", shell]);
            assert!(cli.is_ok(), "failed to parse completions for {shell}");
        }
    }

    #[test]
    fn inspect_subcommands() {
        let cli = Cli::try_parse_from([
            "pupil",
            "inspect",
            "my-agent",
            "search",
            "deploy process",
        ])
        .unwrap();
        match cli.command {
            Command::Inspect(args) => {
                assert_eq!(args.name.as_deref(), Some("my-agent"));
                assert!(matches!(
                    args.action,
                    Some(commands::inspect::InspectAction::Search { .. })
                ));
            }
            _ => panic!("expected Inspect command"),
        }
    }

    #[test]
    fn test_command_generate() {
        let cli = Cli::try_parse_from([
            "pupil",
            "test",
            "--generate",
            "--count",
            "20",
            "--output",
            "qa.yaml",
        ])
        .unwrap();
        match cli.command {
            Command::Test(args) => {
                assert!(args.generate);
                assert_eq!(args.count, 20);
                assert_eq!(args.output, Some(PathBuf::from("qa.yaml")));
            }
            _ => panic!("expected Test command"),
        }
    }

    #[test]
    fn watch_defaults() {
        let cli = Cli::try_parse_from(["pupil", "watch", "my-agent"]).unwrap();
        match cli.command {
            Command::Watch(args) => {
                assert_eq!(args.name.as_deref(), Some("my-agent"));
                assert!(!args.test);
                assert_eq!(args.debounce, 500);
                assert!(!args.no_initial_learn);
            }
            _ => panic!("expected Watch command"),
        }
    }
}
