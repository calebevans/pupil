//! Learning pipeline for curriculum ingestion.
//!
//! This module is gated behind the `learn` feature flag. It is only
//! compiled into the pupil-agent binary when learning is needed
//! (the default for build containers).

#[cfg(feature = "learn")]
pub mod source;
#[cfg(feature = "learn")]
pub mod extract;
#[cfg(feature = "learn")]
pub mod reader;
#[cfg(feature = "learn")]
pub mod learner;
#[cfg(feature = "learn")]
pub mod prompt;
#[cfg(feature = "learn")]
pub mod manifest;
#[cfg(feature = "learn")]
pub mod verifier;

#[cfg(feature = "learn")]
use crate::config::AgentConfig;
#[cfg(feature = "learn")]
use crate::llm::LlmProvider;
#[cfg(feature = "learn")]
use crate::mcp::McpManager;

#[cfg(feature = "learn")]
use self::manifest::{ContentManifest, SourceEntry};
#[cfg(feature = "learn")]
use self::prompt::TemplateVars;
#[cfg(feature = "learn")]
use self::source::ResolvedSource;

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct LearnOptions {
    /// When non-empty, learn only these sources (by source key).
    /// Empty means learn all sources.
    pub source_filter: Vec<String>,
    /// Forget memories for this source then exit.
    pub forget_source: Option<String>,
    /// Namespace for memory storage.
    pub namespace: String,
    /// Path to the content manifest file.
    pub manifest_path: std::path::PathBuf,
    /// Path to the curriculum directory.
    pub curriculum_dir: std::path::PathBuf,
    /// Optional remedial prompt to prepend to the learning system prompt.
    /// Provided during self-test remedial cycles to guide the agent toward
    /// weak topic areas.
    pub remedial_prompt: Option<String>,
    /// When true, re-learn sources even if the content hash is unchanged.
    /// Used during remedial learning where the content has not changed but
    /// the agent needs to re-read with different focus.
    pub force_relearn: bool,
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct LearningSummary {
    pub sources_learned: usize,
    pub sources_skipped: usize,
    pub total_memories_created: usize,
    pub total_memories_forgotten: usize,
    pub total_facts_verified: usize,
    pub total_facts_retrievable: usize,
    pub source_results: Vec<SourceLearnResult>,
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct SourceLearnResult {
    pub source_key: String,
    pub action: LearnAction,
    pub memories_created: usize,
    pub memories_forgotten: usize,
    pub memory_ids: Vec<String>,
    pub verification_total: usize,
    pub verification_retrievable: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(feature = "learn")]
pub enum LearnAction {
    Skipped,
    Learned,
    Relearned,
    Forgotten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "learn")]
pub enum LearnErrorKind {
    SourceNotFound,
    ExtractionFailed,
    FetchFailed,
    LlmError,
    ManifestError,
    McpError,
    SourceNotInManifest,
}

#[derive(Debug)]
#[cfg(feature = "learn")]
pub struct LearnError {
    pub kind: LearnErrorKind,
    pub inner: anyhow::Error,
}

#[cfg(feature = "learn")]
impl std::fmt::Display for LearnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)
    }
}

#[cfg(feature = "learn")]
impl std::error::Error for LearnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

#[cfg(feature = "learn")]
pub async fn run_learning(
    config: &AgentConfig,
    llm: &dyn LlmProvider,
    mcp: &McpManager,
    options: &LearnOptions,
) -> anyhow::Result<LearningSummary> {
    // 1. Load the manifest
    let mut manifest = ContentManifest::load(&options.manifest_path)?;
    if manifest.namespace.is_empty() {
        manifest.namespace = options.namespace.clone();
    }

    // 2. Handle --forget-source
    if let Some(ref source_key) = options.forget_source {
        return handle_forget_source(mcp, &mut manifest, source_key, &options.manifest_path).await;
    }

    // 3. Resolve sources
    let resolved = source::resolve_sources(config, &options.curriculum_dir)?;

    // 4. Filter sources
    let sources_to_process = if !options.source_filter.is_empty() {
        let mut matched = Vec::new();
        for filter_key in &options.source_filter {
            let found = resolved
                .iter()
                .find(|s| {
                    s.source_key == *filter_key
                        || s.file_path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(|n| n == filter_key)
                            .unwrap_or(false)
                        || s.url.as_deref() == Some(filter_key.as_str())
                        // Also match by relative path for source/ tags
                        || s.source_key.ends_with(filter_key)
                        || filter_key.ends_with(&s.source_key)
                })
                .cloned();

            match found {
                Some(s) => {
                    if !matched.iter().any(|m: &ResolvedSource| m.source_key == s.source_key) {
                        matched.push(s);
                    }
                }
                None => {
                    tracing::warn!(
                        source = %filter_key,
                        "Source filter '{}' did not match any curriculum source; skipping",
                        filter_key
                    );
                }
            }
        }

        if matched.is_empty() {
            return Err(anyhow::anyhow!(
                "None of the specified sources matched curriculum configuration: [{}]",
                options.source_filter.join(", ")
            ));
        }

        matched
    } else {
        resolved.clone()
    };

    let curriculum_learning_profile = config
        .curriculum
        .as_ref()
        .and_then(|c| c.learning_profile.as_deref());

    // 5. Process each source
    let mut source_results = Vec::new();

    for resolved_source in &sources_to_process {
        let result = process_source(
            config,
            llm,
            mcp,
            resolved_source,
            &mut manifest,
            &options.namespace,
            curriculum_learning_profile,
            options.force_relearn,
            options.remedial_prompt.as_deref(),
        )
        .await;

        match result {
            Ok(r) => source_results.push(r),
            Err(e) => {
                if !options.source_filter.is_empty() {
                    return Err(e);
                }
                tracing::error!(
                    source = %resolved_source.source_key,
                    error = %e,
                    "Failed to process source; continuing"
                );
            }
        }
    }

    // 6. Reconcile orphaned sources (only in full-build mode)
    if options.source_filter.is_empty() {
        let current_keys: Vec<String> = resolved.iter().map(|s| s.source_key.clone()).collect();
        let orphaned = manifest.find_orphaned_sources(&current_keys);
        for orphaned_key in orphaned {
            let memory_ids = manifest.memory_ids(&orphaned_key).to_vec();
            let forgotten_count = forget_memories(mcp, &memory_ids).await;
            tracing::info!(
                source = %orphaned_key,
                memories_forgotten = forgotten_count,
                "Cleaned up removed source"
            );
            manifest.remove_source(&orphaned_key);
            source_results.push(SourceLearnResult {
                source_key: orphaned_key,
                action: LearnAction::Forgotten,
                memories_created: 0,
                memories_forgotten: forgotten_count,
                memory_ids: vec![],
                verification_total: 0,
                verification_retrievable: 0,
            });
        }
    }

    // 7. Save the manifest
    if let Some(parent) = options.manifest_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    manifest.save(&options.manifest_path)?;

    // 8. Build summary
    let sources_learned = source_results
        .iter()
        .filter(|r| r.action == LearnAction::Learned || r.action == LearnAction::Relearned)
        .count();
    let sources_skipped = source_results
        .iter()
        .filter(|r| r.action == LearnAction::Skipped)
        .count();
    let total_memories_created: usize = source_results.iter().map(|r| r.memories_created).sum();
    let total_memories_forgotten: usize =
        source_results.iter().map(|r| r.memories_forgotten).sum();

    let total_facts_verified: usize = source_results.iter().map(|r| r.verification_total).sum();
    let total_facts_retrievable: usize =
        source_results.iter().map(|r| r.verification_retrievable).sum();

    Ok(LearningSummary {
        sources_learned,
        sources_skipped,
        total_memories_created,
        total_memories_forgotten,
        total_facts_verified,
        total_facts_retrievable,
        source_results,
    })
}

#[cfg(feature = "learn")]
async fn handle_forget_source(
    mcp: &McpManager,
    manifest: &mut ContentManifest,
    source_key: &str,
    manifest_path: &std::path::Path,
) -> anyhow::Result<LearningSummary> {
    let entry = manifest.sources.get(source_key).cloned();
    let entry = match entry {
        Some(e) => e,
        None => {
            return Err(anyhow::anyhow!(
                "Source '{}' not found in manifest",
                source_key
            ));
        }
    };

    let forgotten_count = forget_memories(mcp, &entry.memory_ids).await;
    manifest.remove_source(source_key);
    manifest.save(manifest_path)?;

    Ok(LearningSummary {
        sources_learned: 0,
        sources_skipped: 0,
        total_memories_created: 0,
        total_memories_forgotten: forgotten_count,
        total_facts_verified: 0,
        total_facts_retrievable: 0,
        source_results: vec![SourceLearnResult {
            source_key: source_key.to_string(),
            action: LearnAction::Forgotten,
            memories_created: 0,
            memories_forgotten: forgotten_count,
            memory_ids: vec![],
            verification_total: 0,
            verification_retrievable: 0,
        }],
    })
}

#[cfg(feature = "learn")]
async fn process_source(
    config: &AgentConfig,
    llm: &dyn LlmProvider,
    mcp: &McpManager,
    resolved_source: &ResolvedSource,
    manifest: &mut ContentManifest,
    namespace: &str,
    curriculum_learning_profile: Option<&str>,
    force_relearn: bool,
    remedial_prompt: Option<&str>,
) -> anyhow::Result<SourceLearnResult> {
    let source_key = &resolved_source.source_key;

    // Build the template vars for prompt construction
    let source_file = resolved_source
        .file_path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or(source_key);

    let vars = TemplateVars {
        source_file: source_file.to_string(),
        source_path: source_key.clone(),
        agent_name: config.name.clone(),
        namespace: namespace.to_string(),
        heading_path: String::new(),
        source_type: resolved_source.source_type.as_str().to_string(),
    };

    // Build the effective learning prompt (needed for prompt hash even if skipping)
    let mut learning_prompt = prompt::build_learning_prompt(
        resolved_source,
        curriculum_learning_profile,
        config.learning_prompt.as_deref(),
        &vars,
    );

    if let Some(remedial) = remedial_prompt {
        learning_prompt = format!("{}\n\n---\n\n{}", remedial, learning_prompt);
    }

    let prompt_hash = sha256_hex(learning_prompt.as_bytes());

    // Extract content (needed for content hash)
    let extracted = match extract::extract(resolved_source).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                source = %source_key,
                error = %e,
                "Unable to process source, skipping"
            );
            return Ok(SourceLearnResult {
                source_key: source_key.clone(),
                action: LearnAction::Skipped,
                memories_created: 0,
                memories_forgotten: 0,
                memory_ids: vec![],
                verification_total: 0,
                verification_retrievable: 0,
            });
        }
    };
    let content_hash = sha256_hex(&extracted.raw_bytes);

    // Check manifest for skip (unless force_relearn is set)
    if !force_relearn {
        match manifest.is_unchanged(source_key, &content_hash, &prompt_hash) {
            Some(true) => {
                tracing::info!(source = %source_key, "Skipping unchanged source");
                return Ok(SourceLearnResult {
                    source_key: source_key.clone(),
                    action: LearnAction::Skipped,
                    memories_created: 0,
                    memories_forgotten: 0,
                    memory_ids: manifest.memory_ids(source_key).to_vec(),
                    verification_total: 0,
                    verification_retrievable: 0,
                });
            }
            Some(false) => {
                // Source changed: forget old memories first
                let old_ids = manifest.memory_ids(source_key).to_vec();
                let forgotten_count = forget_memories(mcp, &old_ids).await;
                tracing::info!(
                    source = %source_key,
                    memories_forgotten = forgotten_count,
                    "Re-learning changed source"
                );

                let sections = reader::split_into_sections(&extracted, config.curriculum.as_ref().and_then(|c| c.max_section_chars));
                let learning_result =
                    learner::learn_source(llm, mcp, &sections, &learning_prompt, source_key, namespace)
                        .await?;

                let synthesis_prompt = prompt::build_synthesis_prompt(source_key, namespace);
                let synthesis_result = learner::synthesize_relationships(
                    llm,
                    mcp,
                    &learning_result.memory_ids,
                    &synthesis_prompt,
                    source_key,
                    namespace,
                )
                .await?;

                let mut all_memory_ids = learning_result.memory_ids;
                all_memory_ids.extend(synthesis_result.memory_ids);

                manifest.upsert_source(
                    source_key.clone(),
                    SourceEntry {
                        content_hash,
                        prompt_hash,
                        memory_ids: all_memory_ids.clone(),
                        last_learned: now_utc_iso8601(),
                        sync: None,
                    },
                );

                return Ok(SourceLearnResult {
                    source_key: source_key.clone(),
                    action: LearnAction::Relearned,
                    memories_created: all_memory_ids.len(),
                    memories_forgotten: forgotten_count,
                    memory_ids: all_memory_ids,
                    verification_total: 0,
                    verification_retrievable: 0,
                });
            }
            None => {
                // New source
            }
        }
    }

    // New source (or force_relearn): learn it
    let sections = reader::split_into_sections(&extracted, config.curriculum.as_ref().and_then(|c| c.max_section_chars));
    let learning_result =
        learner::learn_source(llm, mcp, &sections, &learning_prompt, source_key, namespace)
            .await?;

    let synthesis_prompt = prompt::build_synthesis_prompt(source_key, namespace);
    let synthesis_result = learner::synthesize_relationships(
        llm,
        mcp,
        &learning_result.memory_ids,
        &synthesis_prompt,
        source_key,
        namespace,
    )
    .await?;

    let mut all_memory_ids = learning_result.memory_ids;
    all_memory_ids.extend(synthesis_result.memory_ids);

    manifest.upsert_source(
        source_key.clone(),
        SourceEntry {
            content_hash,
            prompt_hash,
            memory_ids: all_memory_ids.clone(),
            last_learned: now_utc_iso8601(),
            sync: None,
        },
    );

    Ok(SourceLearnResult {
        source_key: source_key.clone(),
        action: LearnAction::Learned,
        memories_created: all_memory_ids.len(),
        memories_forgotten: 0,
        memory_ids: all_memory_ids,
        verification_total: 0,
        verification_retrievable: 0,
    })
}

#[cfg(feature = "learn")]
async fn forget_memories(mcp: &McpManager, memory_ids: &[String]) -> usize {
    let mut forgotten = 0;
    for id in memory_ids {
        let args = serde_json::json!({"id": id});
        let arguments = args.as_object().cloned();
        match mcp.call_tool("forget_memory", arguments).await {
            Ok(_) => {
                tracing::debug!(memory_id = %id, "Memory forgotten");
                forgotten += 1;
            }
            Err(e) => {
                tracing::warn!(
                    memory_id = %id,
                    error = %e,
                    "Failed to forget memory (may already be deleted)"
                );
            }
        }
    }
    forgotten
}

#[cfg(feature = "learn")]
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    format!("sha256:{}", hex::encode(result))
}

#[cfg(feature = "learn")]
fn now_utc_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

#[cfg(feature = "learn")]
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello world");
        assert!(hash.starts_with("sha256:"));
        assert_eq!(
            hash,
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        let empty_hash = sha256_hex(b"");
        assert_eq!(
            empty_hash,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_now_utc_iso8601_format() {
        let ts = now_utc_iso8601();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // 2026-06-25 is day 20629 since epoch
        let days_since_epoch = 20629;
        let (y, m, d) = days_to_ymd(days_since_epoch);
        assert_eq!(y, 2026);
        assert_eq!(m, 6);
        assert_eq!(d, 25);
    }
}
