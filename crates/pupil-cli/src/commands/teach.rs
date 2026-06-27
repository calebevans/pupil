use clap::Args;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::agent_config::{resolve_agent, AgentConfig, SourceEntry};
use crate::error::CliError;

#[derive(Args, Debug)]
pub struct TeachArgs {
    pub name: Option<String>,

    #[arg(required_unless_present = "url")]
    pub paths: Vec<PathBuf>,

    #[arg(long)]
    pub url: Option<String>,

    #[arg(short, long)]
    pub recursive: bool,

    #[arg(short, long)]
    pub glob: Option<String>,

    #[arg(long)]
    pub dry_run: bool,
}

pub async fn execute(args: TeachArgs) -> Result<(), CliError> {
    let (agent_dir, mut config) = resolve_agent(args.name.as_deref())?;
    let curriculum_dir = agent_dir.join("curriculum");

    if !curriculum_dir.exists() {
        std::fs::create_dir_all(&curriculum_dir)?;
    }

    if let Some(url) = &args.url {
        return handle_url_source(&agent_dir, &mut config, url, args.dry_run);
    }

    let files = expand_paths(&args.paths, args.recursive, args.glob.as_deref())?;

    if files.is_empty() {
        println!("No matching files found.");
        return Ok(());
    }

    let existing_hashes = hash_directory(&curriculum_dir)?;

    let mut copied = 0u64;
    let mut skipped_dup = 0u64;
    let mut skipped_exists = 0u64;

    for source_path in &files {
        let dest = compute_destination(&curriculum_dir, source_path, &args.paths)?;

        let source_hash = hash_file(source_path)?;
        if existing_hashes.contains(&source_hash) {
            skipped_dup += 1;
            if args.dry_run {
                println!(
                    "  [skip] {} (duplicate content already in curriculum)",
                    source_path.display()
                );
            }
            continue;
        }

        if dest.exists() {
            let dest_hash = hash_file(&dest)?;
            if dest_hash == source_hash {
                skipped_dup += 1;
                if args.dry_run {
                    println!(
                        "  [skip] {} (identical to {})",
                        source_path.display(),
                        dest.display()
                    );
                }
                continue;
            }
            if !args.dry_run {
                eprintln!(
                    "  [warn] Overwriting {} (content changed)",
                    dest.strip_prefix(&agent_dir).unwrap_or(&dest).display()
                );
            }
            skipped_exists += 1;
        }

        if args.dry_run {
            println!(
                "  [copy] {} -> {}",
                source_path.display(),
                dest.strip_prefix(&agent_dir).unwrap_or(&dest).display()
            );
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source_path, &dest)?;
        }
        copied += 1;
    }

    if args.dry_run {
        println!();
        println!(
            "Dry run: {} file(s) would be copied, {} skipped (duplicate), {} skipped (already exists)",
            copied, skipped_dup, skipped_exists
        );
    } else {
        println!(
            "Added {} file(s) to curriculum ({} skipped as duplicates)",
            copied, skipped_dup
        );
        if copied > 0 {
            println!("Run `pupil build` to learn the new content.");
        }
    }

    Ok(())
}

fn handle_url_source(
    agent_dir: &std::path::Path,
    config: &mut AgentConfig,
    url: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(CliError::ConfigInvalid {
            message: format!(
                "Invalid URL: {}. Must start with http:// or https://",
                url
            ),
        });
    }

    let already_exists = config.curriculum.sources.iter().any(|s| match s {
        SourceEntry::Short(s) => s == url,
        SourceEntry::Long(sc) => sc.url.as_deref() == Some(url),
    });

    if already_exists {
        println!("URL already in curriculum sources: {}", url);
        return Ok(());
    }

    if dry_run {
        println!("Would add URL source to pupil.yaml: {}", url);
        return Ok(());
    }

    config
        .curriculum
        .sources
        .push(SourceEntry::Short(url.to_string()));

    let yaml = serde_yml::to_string(config)
        .map_err(|e| CliError::ConfigInvalid {
            message: format!("Failed to serialize config: {}", e),
        })?;
    std::fs::write(agent_dir.join("pupil.yaml"), yaml)?;

    println!("Added URL source: {}", url);
    println!("Run `pupil build` to learn the content from this URL.");

    Ok(())
}

fn expand_paths(
    paths: &[PathBuf],
    recursive: bool,
    glob_pattern: Option<&str>,
) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    let glob_matcher = glob_pattern
        .map(|p| {
            glob::Pattern::new(p).map_err(|e| CliError::ConfigInvalid {
                message: format!("Invalid glob pattern '{}': {}", p, e),
            })
        })
        .transpose()?;

    for path in paths {
        if !path.exists() {
            return Err(CliError::ConfigInvalid {
                message: format!("Path does not exist: {}", path.display()),
            });
        }

        if path.is_file() {
            if let Some(matcher) = glob_matcher.as_ref() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !matcher.matches(name) {
                    continue;
                }
            }
            files.push(path.clone());
        } else if path.is_dir() {
            collect_from_dir(path, recursive, &glob_matcher, &mut files)?;
        }
    }

    Ok(files)
}

fn collect_from_dir(
    dir: &std::path::Path,
    recursive: bool,
    glob_matcher: &Option<glob::Pattern>,
    files: &mut Vec<PathBuf>,
) -> Result<(), CliError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(matcher) = glob_matcher {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !matcher.matches(name) {
                    continue;
                }
            }
            if is_supported_curriculum_file(&path) {
                files.push(path);
            }
        } else if path.is_dir() && recursive {
            collect_from_dir(&path, recursive, glob_matcher, files)?;
        }
    }
    Ok(())
}

pub fn is_supported_curriculum_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "txt" | "pdf" | "html" | "htm" | "json" | "csv" | "yaml" | "yml")
    )
}

fn hash_file(path: &std::path::Path) -> Result<String, CliError> {
    let contents = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    Ok(hex::encode(hasher.finalize()))
}

fn hash_directory(dir: &std::path::Path) -> Result<HashSet<String>, CliError> {
    let mut hashes = HashSet::new();
    if !dir.exists() {
        return Ok(hashes);
    }
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            hashes.insert(hash_file(entry.path())?);
        }
    }
    Ok(hashes)
}

fn compute_destination(
    curriculum_dir: &std::path::Path,
    source: &std::path::Path,
    original_args: &[PathBuf],
) -> Result<PathBuf, CliError> {
    for arg in original_args {
        if arg.is_dir() {
            if let Ok(relative) = source.strip_prefix(arg) {
                return Ok(curriculum_dir.join(relative));
            }
        }
    }
    let filename = source.file_name().ok_or_else(|| CliError::ConfigInvalid {
        message: format!("Could not determine filename for: {}", source.display()),
    })?;
    Ok(curriculum_dir.join(filename))
}
