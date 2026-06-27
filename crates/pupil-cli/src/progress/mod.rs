use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::time::Duration;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
    Quiet,
    Plain,
}

impl OutputMode {
    pub fn detect(json: bool, quiet: bool, progress: Option<&str>) -> Self {
        if quiet {
            return Self::Quiet;
        }
        if json {
            return Self::Json;
        }
        if let Some("plain") = progress {
            return Self::Plain;
        }
        if console::Term::stderr().is_term() {
            Self::Human
        } else {
            Self::Plain
        }
    }
}

/// Initialize the global tracing subscriber with indicatif support.
///
/// Call this once at startup in main.rs. The returned `MultiProgress` handle
/// can be used to create additional progress bars outside of tracing.
///
/// In JSON mode, uses `tracing_subscriber::fmt::json()` instead of indicatif.
/// In quiet mode, sets the log level to WARN.
pub fn init_tracing(mode: OutputMode) -> MultiProgress {
    let multi = MultiProgress::new();

    match mode {
        OutputMode::Human => {
            let indicatif_layer = IndicatifLayer::new()
                .with_progress_style(spinner_style())
                .with_max_progress_bars(10, None);

            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(indicatif_layer.get_stderr_writer())
                        .with_ansi(true),
                )
                .with(indicatif_layer)
                .init();
        }
        OutputMode::Json => {
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_writer(std::io::stderr),
                )
                .init();
        }
        OutputMode::Quiet => {
            let env_filter = EnvFilter::new("warn");

            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_ansi(false),
                )
                .init();
        }
        OutputMode::Plain => {
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_ansi(false),
                )
                .init();
        }
    }

    multi
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .tick_strings(&["   ", ".  ", ".. ", "...", " ..", "  .", "   "])
        .template("{spinner} {msg}")
        .expect("invalid spinner template")
}

/// Create a spinner progress bar for a build phase.
///
/// Use for unbounded operations (scanning, learning, committing) where
/// the total is not known in advance.
pub fn create_spinner(multi: &MultiProgress, message: &str, mode: OutputMode) -> ProgressBar {
    if mode == OutputMode::Quiet {
        return ProgressBar::hidden();
    }

    let pb = multi.add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["   ", ".  ", ".. ", "...", " ..", "  .", "   "])
            .template("{spinner:.cyan} {msg}")
            .expect("invalid template"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

/// Create a progress bar for the learning phase.
///
/// Shows: current source being learned, memories created so far,
/// and elapsed time.
pub fn create_learning_progress(
    multi: &MultiProgress,
    total_sources: u64,
    mode: OutputMode,
) -> ProgressBar {
    if mode == OutputMode::Quiet {
        return ProgressBar::hidden();
    }

    let pb = multi.add(ProgressBar::new(total_sources));
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.cyan} Learning [{bar:30.cyan/dim}] \
                 {pos}/{len} sources | {msg} | {elapsed}",
            )
            .expect("invalid template")
            .progress_chars("=> "),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

/// Finish a progress bar with a completion message.
pub fn finish_progress(pb: &ProgressBar, message: &str, mode: OutputMode) {
    match mode {
        OutputMode::Human => {
            pb.finish_with_message(console::style(message).green().to_string());
        }
        OutputMode::Json => {
            pb.finish_and_clear();
        }
        _ => {
            pb.finish_with_message(message.to_string());
        }
    }
}
