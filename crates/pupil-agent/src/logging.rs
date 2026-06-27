use tracing_subscriber::{
    EnvFilter,
    fmt::{self, time::UtcTime},
    prelude::*,
};

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
                if atty_stderr() {
                    LogFormat::Human
                } else {
                    LogFormat::Json
                }
            }
        }
    }
}

fn atty_stderr() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

pub fn init_logging(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("pupil_agent=info,warn")
    });

    match format {
        LogFormat::Human => {
            let layer = fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(true)
                .with_target(true)
                .with_level(true)
                .without_time()
                .compact();

            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .init();
        }
        LogFormat::Json => {
            let layer = fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true)
                .with_level(true)
                .with_timer(UtcTime::rfc_3339())
                .json()
                .with_span_list(true)
                .with_current_span(true);

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
