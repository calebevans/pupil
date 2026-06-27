#[cfg(feature = "learn")]
use std::path::{Path, PathBuf};

#[cfg(feature = "learn")]
use crate::config::{AgentConfig, SourceEntry as ConfigSourceEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "learn")]
pub enum SourceType {
    Markdown,
    PlainText,
    Pdf,
    Html,
    Url,
}

#[cfg(feature = "learn")]
impl SourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceType::Markdown => "markdown",
            SourceType::PlainText => "text",
            SourceType::Pdf => "pdf",
            SourceType::Html => "html",
            SourceType::Url => "url",
        }
    }
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct ResolvedSource {
    pub source_key: String,
    pub file_path: Option<PathBuf>,
    pub url: Option<String>,
    pub source_type: SourceType,
    pub learning_profile: Option<String>,
    pub learning_prompt: Option<String>,
    pub namespace: String,
    pub extra_tags: Vec<String>,
}

#[cfg(feature = "learn")]
pub fn detect_from_path(path: &Path) -> SourceType {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("md" | "mdx" | "markdown") => SourceType::Markdown,
        Some("pdf") => SourceType::Pdf,
        Some("html" | "htm") => SourceType::Html,
        _ => SourceType::PlainText,
    }
}

#[cfg(feature = "learn")]
pub fn detect_type(source: &str) -> SourceType {
    if source.starts_with("http://") || source.starts_with("https://") {
        SourceType::Url
    } else {
        detect_from_path(Path::new(source))
    }
}

#[cfg(feature = "learn")]
pub fn resolve_sources(
    config: &AgentConfig,
    curriculum_dir: &Path,
) -> anyhow::Result<Vec<ResolvedSource>> {
    let curriculum = match &config.curriculum {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };

    let default_namespace = &curriculum.namespace;
    let default_profile = curriculum.learning_profile.as_deref();

    let mut resolved = Vec::new();

    for entry in &curriculum.sources {
        match entry {
            ConfigSourceEntry::Short(s) => {
                let sources = expand_source_string(
                    s,
                    curriculum_dir,
                    default_namespace,
                    default_profile,
                    None,
                    None,
                    &[],
                )?;
                resolved.extend(sources);
            }
            ConfigSourceEntry::Long(sc) => {
                let ns = sc
                    .namespace
                    .as_deref()
                    .unwrap_or(default_namespace);
                let profile = sc.learning_profile.as_deref().or(default_profile);
                let prompt = sc.learning_prompt.as_deref();
                let tags = &sc.tags;

                if let Some(ref url_str) = sc.url {
                    resolved.push(ResolvedSource {
                        source_key: url_str.clone(),
                        file_path: None,
                        url: Some(url_str.clone()),
                        source_type: SourceType::Url,
                        learning_profile: if prompt.is_none() {
                            Some(profile.unwrap_or("general").to_string())
                        } else {
                            None
                        },
                        learning_prompt: prompt.map(|s| s.to_string()),
                        namespace: ns.to_string(),
                        extra_tags: tags.clone(),
                    });
                } else if let Some(ref path_str) = sc.path {
                    let sources = expand_source_string(
                        path_str,
                        curriculum_dir,
                        ns,
                        profile,
                        prompt,
                        sc.learning_profile.as_deref(),
                        tags,
                    )?;
                    resolved.extend(sources);
                } else if let Some(ref glob_str) = sc.glob {
                    let sources = expand_glob(
                        glob_str,
                        curriculum_dir,
                        ns,
                        profile,
                        prompt,
                        sc.learning_profile.as_deref(),
                        tags,
                    )?;
                    resolved.extend(sources);
                }
            }
        }
    }

    Ok(resolved)
}

#[cfg(feature = "learn")]
fn expand_source_string(
    source: &str,
    curriculum_dir: &Path,
    namespace: &str,
    profile: Option<&str>,
    custom_prompt: Option<&str>,
    explicit_profile: Option<&str>,
    extra_tags: &[String],
) -> anyhow::Result<Vec<ResolvedSource>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(vec![ResolvedSource {
            source_key: source.to_string(),
            file_path: None,
            url: Some(source.to_string()),
            source_type: SourceType::Url,
            learning_profile: if custom_prompt.is_none() {
                Some(profile.unwrap_or("general").to_string())
            } else {
                None
            },
            learning_prompt: custom_prompt.map(|s| s.to_string()),
            namespace: namespace.to_string(),
            extra_tags: extra_tags.to_vec(),
        }]);
    }

    let source_normalized = source.trim_start_matches("./");

    let mut path = if Path::new(source_normalized).is_absolute() {
        PathBuf::from(source_normalized)
    } else {
        curriculum_dir.join(source_normalized)
    };

    // Detect self-referential path: if the source string names the
    // curriculum directory itself (e.g., "curriculum/" when curriculum_dir
    // is /curriculum or /path/to/agent/curriculum), resolve to
    // curriculum_dir directly instead of creating a nested path.
    if !path.exists() {
        // Check if the source is the curriculum dir's own name
        let source_base = Path::new(source_normalized)
            .components()
            .next()
            .and_then(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .unwrap_or("");
        let curriculum_base = curriculum_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if !source_base.is_empty()
            && source_base == curriculum_base
            && curriculum_dir.exists()
        {
            path = curriculum_dir.to_path_buf();
        }
    }

    if has_glob_chars(source) {
        return expand_glob(
            source,
            curriculum_dir,
            namespace,
            profile,
            custom_prompt,
            explicit_profile,
            extra_tags,
        );
    }

    if path.is_dir() {
        let files = walk_directory(&path)?;
        let mut results = Vec::new();
        for file in files {
            let source_key = make_source_key(&file, curriculum_dir);
            let source_type = detect_from_path(&file);
            results.push(ResolvedSource {
                source_key,
                file_path: Some(file),
                url: None,
                source_type,
                learning_profile: if custom_prompt.is_none() {
                    Some(
                        explicit_profile
                            .or(profile)
                            .unwrap_or("general")
                            .to_string(),
                    )
                } else {
                    None
                },
                learning_prompt: custom_prompt.map(|s| s.to_string()),
                namespace: namespace.to_string(),
                extra_tags: extra_tags.to_vec(),
            });
        }
        return Ok(results);
    }

    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Source path '{}' does not exist",
            path.display()
        ));
    }

    let source_key = make_source_key(&path, curriculum_dir);
    let source_type = detect_from_path(&path);
    Ok(vec![ResolvedSource {
        source_key,
        file_path: Some(path),
        url: None,
        source_type,
        learning_profile: if custom_prompt.is_none() {
            Some(
                explicit_profile
                    .or(profile)
                    .unwrap_or("general")
                    .to_string(),
            )
        } else {
            None
        },
        learning_prompt: custom_prompt.map(|s| s.to_string()),
        namespace: namespace.to_string(),
        extra_tags: extra_tags.to_vec(),
    }])
}

#[cfg(feature = "learn")]
fn expand_glob(
    pattern: &str,
    curriculum_dir: &Path,
    namespace: &str,
    profile: Option<&str>,
    custom_prompt: Option<&str>,
    explicit_profile: Option<&str>,
    extra_tags: &[String],
) -> anyhow::Result<Vec<ResolvedSource>> {
    let files = walk_directory(curriculum_dir)?;
    let mut results = Vec::new();

    for file in files {
        let rel = make_source_key(&file, curriculum_dir);
        if matches_simple_glob(pattern, &rel) {
            let source_type = detect_from_path(&file);
            results.push(ResolvedSource {
                source_key: rel,
                file_path: Some(file),
                url: None,
                source_type,
                learning_profile: if custom_prompt.is_none() {
                    Some(
                        explicit_profile
                            .or(profile)
                            .unwrap_or("general")
                            .to_string(),
                    )
                } else {
                    None
                },
                learning_prompt: custom_prompt.map(|s| s.to_string()),
                namespace: namespace.to_string(),
                extra_tags: extra_tags.to_vec(),
            });
        }
    }

    Ok(results)
}

#[cfg(feature = "learn")]
fn has_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

#[cfg(feature = "learn")]
fn matches_simple_glob(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return text.ends_with(&format!(".{suffix}"));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return text.starts_with(&format!("{prefix}/"));
    }
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let starts = parts[0].is_empty() || text.starts_with(parts[0]);
            let suffix = parts[1].strip_prefix('/').unwrap_or(parts[1]);
            let ends = suffix.is_empty() || {
                if let Some(ext) = suffix.strip_prefix("*.") {
                    text.ends_with(&format!(".{ext}"))
                } else {
                    text.ends_with(suffix)
                }
            };
            return starts && ends;
        }
    }
    text == pattern
}

#[cfg(feature = "learn")]
fn make_source_key(path: &Path, curriculum_dir: &Path) -> String {
    path.strip_prefix(curriculum_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(feature = "learn")]
fn walk_directory(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_directory_inner(dir, &mut files)?;
    files.sort();
    Ok(files)
}

#[cfg(feature = "learn")]
fn walk_directory_inner(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to read directory '{}': {}",
                dir.display(),
                e
            ));
        }
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name.starts_with('.') {
            continue;
        }
        if name.ends_with(".swp") || name.ends_with(".swo") || name.ends_with('~') {
            continue;
        }

        if path.is_dir() {
            walk_directory_inner(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_detect_from_path() {
        assert_eq!(detect_from_path(Path::new("doc.md")), SourceType::Markdown);
        assert_eq!(detect_from_path(Path::new("doc.mdx")), SourceType::Markdown);
        assert_eq!(
            detect_from_path(Path::new("doc.markdown")),
            SourceType::Markdown
        );
        assert_eq!(detect_from_path(Path::new("doc.pdf")), SourceType::Pdf);
        assert_eq!(detect_from_path(Path::new("page.html")), SourceType::Html);
        assert_eq!(detect_from_path(Path::new("page.htm")), SourceType::Html);
        assert_eq!(
            detect_from_path(Path::new("notes.txt")),
            SourceType::PlainText
        );
        assert_eq!(
            detect_from_path(Path::new("data.csv")),
            SourceType::PlainText
        );
        assert_eq!(
            detect_from_path(Path::new("config.toml")),
            SourceType::PlainText
        );
        assert_eq!(
            detect_from_path(Path::new("noext")),
            SourceType::PlainText
        );
    }

    #[test]
    fn test_detect_type() {
        assert_eq!(
            detect_type("https://example.com/page"),
            SourceType::Url
        );
        assert_eq!(
            detect_type("http://wiki.internal/doc"),
            SourceType::Url
        );
        assert_eq!(
            detect_type("./curriculum/doc.md"),
            SourceType::Markdown
        );
        assert_eq!(detect_type("doc.pdf"), SourceType::Pdf);
    }

    #[test]
    fn test_matches_simple_glob() {
        assert!(matches_simple_glob("*.md", "handbook.md"));
        assert!(!matches_simple_glob("*.md", "handbook.txt"));
        assert!(matches_simple_glob("runbooks/*", "runbooks/deploy.md"));
        assert!(!matches_simple_glob("runbooks/*", "docs/deploy.md"));
        assert!(matches_simple_glob("**/*.md", "deep/path/file.md"));
        assert!(matches_simple_glob("*", "anything"));
    }

    #[test]
    fn test_expand_source_string_no_double_path() {
        let tmp = tempfile::tempdir().unwrap();
        let curriculum_dir = tmp.path().join("curriculum");
        std::fs::create_dir_all(&curriculum_dir).unwrap();
        std::fs::write(curriculum_dir.join("test.md"), "# Test").unwrap();

        // Simulate what happens in-container: curriculum_dir IS /tmp/.../curriculum
        // and the source string is "./curriculum/"
        let results = expand_source_string(
            "./curriculum/",
            &curriculum_dir,
            "knowledge",
            None,
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_key, "test.md");
        assert!(results[0].file_path.as_ref().unwrap().exists());
    }

    #[test]
    fn test_expand_source_string_subdir_not_confused() {
        let tmp = tempfile::tempdir().unwrap();
        let curriculum_dir = tmp.path().join("curriculum");
        let subdir = curriculum_dir.join("docs");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("guide.md"), "# Guide").unwrap();

        // "docs/" should resolve to curriculum_dir/docs/, not curriculum_dir
        let results = expand_source_string(
            "docs/",
            &curriculum_dir,
            "knowledge",
            None,
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_key, "docs/guide.md");
    }
}
