use console::style;
use serde::Serialize;
use tabled::settings::object::Columns;
use tabled::settings::{Alignment, Modify, Style, Width};
use tabled::{Table, Tabled};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
}

impl OutputFormat {
    pub fn from_flags(json: bool, format: Option<&str>) -> Self {
        if json {
            return Self::Json;
        }
        match format {
            Some("json") => Self::Json,
            _ => Self::Human,
        }
    }
}

/// Print a table of data. In JSON mode, serializes as a JSON array.
pub fn print_table<T: Tabled + Serialize>(data: &[T], format: OutputFormat, title: Option<&str>) {
    match format {
        OutputFormat::Human => {
            if let Some(title) = title {
                println!("{}", style(title).bold());
                println!();
            }
            if data.is_empty() {
                println!("  {}", style("(none)").dim());
                return;
            }

            let term_width = console::Term::stdout()
                .size_checked()
                .map(|(_, w)| w as usize)
                .unwrap_or(120);

            let table = Table::new(data)
                .with(Style::rounded())
                .with(
                    Modify::new(Columns::last())
                        .with(Width::truncate(term_width / 2).suffix("...")),
                )
                .with(Modify::new(Columns::first()).with(Alignment::left()))
                .to_string();

            println!("{}", table);
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(data).expect("failed to serialize to JSON");
            println!("{}", json);
        }
    }
}

/// Print a single structured value. In Human mode, prints key-value pairs.
/// In JSON mode, serializes the entire value.
pub fn print_value<T: Serialize>(value: &T, format: OutputFormat, pairs: &[(&str, String)]) {
    match format {
        OutputFormat::Human => {
            for (key, val) in pairs {
                println!("  {}: {}", style(key).dim(), val);
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(value).expect("failed to serialize to JSON");
            println!("{}", json);
        }
    }
}

pub fn print_success(message: &str) {
    println!("  {} {}", style("[OK]").green().bold(), message);
}

pub fn print_warning(message: &str) {
    eprintln!("  {} {}", style("[WARN]").yellow().bold(), message);
}

pub fn print_error(message: &str) {
    eprintln!("  {} {}", style("[ERROR]").red().bold(), message);
}

pub fn print_header(title: &str) {
    println!();
    println!("{}", style(title).bold().underlined());
    println!();
}

pub fn print_kv(key: &str, value: &str) {
    println!("  {:20} {}", style(format!("{}:", key)).dim(), value);
}

/// Print a horizontal bar chart line (used in `pupil inspect stats`).
pub fn print_bar(label: &str, count: u64, total: u64, max_bar_width: usize) {
    let pct = if total > 0 {
        (count as f64 / total as f64) * 100.0
    } else {
        0.0
    };
    let bar_width = if total > 0 {
        ((count as f64 / total as f64) * max_bar_width as f64) as usize
    } else {
        0
    };
    let bar: String = "\u{2588}".repeat(bar_width);

    println!(
        "    {:<30} {:>5}  {:>5.1}%   {}",
        label,
        count,
        pct,
        style(bar).cyan()
    );
}
