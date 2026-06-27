use tracing_subscriber::{
    EnvFilter,
    fmt::{self, time::UtcTime},
    prelude::*,
};
use tracing_indicatif::IndicatifLayer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Human,
    Json,
}

impl LogFormat {
    pub fn from_env() -> Self {
        match std::env::var("PUPIL_LOG_FORMAT").as_deref() {
            Ok("human") => LogFormat::Human,
            Ok("json") => LogFormat::Json,
            _ => {
                if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
                    LogFormat::Human
                } else {
                    LogFormat::Json
                }
            }
        }
    }
}

pub fn init_logging(format: LogFormat, verbosity: u8) {
    let default_level = match verbosity {
        0 => "pupil_cli=info,warn",
        1 => "pupil_cli=debug,info",
        _ => "pupil_cli=trace,debug",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    match format {
        LogFormat::Human => {
            let indicatif_layer = IndicatifLayer::new();

            let fmt_layer = fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .with_ansi(true)
                .with_target(false)
                .with_level(true)
                .without_time()
                .compact();

            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(indicatif_layer)
                .init();
        }
        LogFormat::Json => {
            let layer = fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true)
                .with_level(true)
                .with_timer(UtcTime::rfc_3339())
                .json();

            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_human() {
        assert_eq!(parse_format_str("human"), LogFormat::Human);
    }

    #[test]
    fn env_var_json() {
        assert_eq!(parse_format_str("json"), LogFormat::Json);
    }

    fn parse_format_str(s: &str) -> LogFormat {
        match s {
            "human" => LogFormat::Human,
            "json" => LogFormat::Json,
            _ => LogFormat::Human,
        }
    }
}
