#[cfg(feature = "learn")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "learn")]
use std::collections::HashMap;
#[cfg(feature = "learn")]
use std::path::Path;

#[cfg(feature = "learn")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentManifest {
    pub version: u32,
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding_dims: Option<u32>,
    pub sources: HashMap<String, SourceEntry>,
    #[serde(default)]
    pub builds: Vec<BuildRecord>,
}

#[cfg(feature = "learn")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEntry {
    pub content_hash: String,
    #[serde(default)]
    pub prompt_hash: String,
    pub memory_ids: Vec<String>,
    pub last_learned: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncState>,
}

#[cfg(feature = "learn")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_changed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub check_count: u64,
    #[serde(default)]
    pub change_count: u64,
    #[serde(default)]
    pub consecutive_errors: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg(feature = "learn")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRecord {
    pub timestamp: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub sources_learned: usize,
    pub sources_skipped: usize,
    pub memories_created: usize,
}

#[cfg(feature = "learn")]
impl ContentManifest {
    pub fn new(namespace: &str) -> Self {
        Self {
            version: 1,
            namespace: namespace.to_string(),
            embedding_provider: None,
            embedding_model: None,
            embedding_dims: None,
            sources: HashMap::new(),
            builds: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(json) => {
                let manifest: Self = serde_json::from_str(&json).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to parse manifest at '{}': {}",
                        path.display(),
                        e
                    )
                })?;
                if manifest.version != 1 {
                    return Err(anyhow::anyhow!(
                        "Unsupported manifest version {} (expected 1) at '{}'",
                        manifest.version,
                        path.display()
                    ));
                }
                Ok(manifest)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %path.display(),
                    "Manifest not found, starting fresh"
                );
                Ok(Self {
                    version: 1,
                    namespace: String::new(),
                    embedding_provider: None,
                    embedding_model: None,
                    embedding_dims: None,
                    sources: HashMap::new(),
                    builds: Vec::new(),
                })
            }
            Err(e) => Err(anyhow::anyhow!(
                "Failed to read manifest at '{}': {}",
                path.display(),
                e
            )),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize manifest: {}", e))?;

        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json.as_bytes()).map_err(|e| {
            anyhow::anyhow!(
                "Failed to write temp manifest at '{}': {}",
                tmp_path.display(),
                e
            )
        })?;
        std::fs::rename(&tmp_path, path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to rename temp manifest to '{}': {}",
                path.display(),
                e
            )
        })?;

        tracing::debug!(
            path = %path.display(),
            sources = self.sources.len(),
            "Manifest saved"
        );
        Ok(())
    }

    pub fn is_unchanged(
        &self,
        source_key: &str,
        content_hash: &str,
        prompt_hash: &str,
    ) -> Option<bool> {
        self.sources.get(source_key).map(|entry| {
            entry.content_hash == content_hash && entry.prompt_hash == prompt_hash
        })
    }

    pub fn memory_ids(&self, source_key: &str) -> &[String] {
        self.sources
            .get(source_key)
            .map(|e| e.memory_ids.as_slice())
            .unwrap_or(&[])
    }

    pub fn source_keys(&self) -> Vec<String> {
        self.sources.keys().cloned().collect()
    }

    pub fn remove_source(&mut self, source_key: &str) -> Option<SourceEntry> {
        self.sources.remove(source_key)
    }

    pub fn upsert_source(&mut self, source_key: String, entry: SourceEntry) {
        self.sources.insert(source_key, entry);
    }

    pub fn add_build_record(&mut self, record: BuildRecord) {
        self.builds.push(record);
    }

    pub fn find_orphaned_sources(&self, current_source_keys: &[String]) -> Vec<String> {
        self.sources
            .keys()
            .filter(|key| !current_source_keys.contains(key))
            .cloned()
            .collect()
    }
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_new() {
        let manifest = ContentManifest::new("test-ns");
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.namespace, "test-ns");
        assert!(manifest.sources.is_empty());
        assert!(manifest.builds.is_empty());
    }

    #[test]
    fn test_is_unchanged_new_source() {
        let manifest = ContentManifest::new("knowledge");
        assert_eq!(manifest.is_unchanged("test.md", "sha256:abc", "sha256:def"), None);
    }

    #[test]
    fn test_is_unchanged_same() {
        let mut manifest = ContentManifest::new("knowledge");
        manifest.upsert_source(
            "test.txt".to_string(),
            SourceEntry {
                content_hash: "sha256:abc".to_string(),
                prompt_hash: "sha256:def".to_string(),
                memory_ids: vec!["id-1".to_string()],
                last_learned: "2026-01-01T00:00:00Z".to_string(),
                sync: None,
            },
        );
        assert_eq!(
            manifest.is_unchanged("test.txt", "sha256:abc", "sha256:def"),
            Some(true)
        );
    }

    #[test]
    fn test_is_unchanged_content_changed() {
        let mut manifest = ContentManifest::new("knowledge");
        manifest.upsert_source(
            "test.txt".to_string(),
            SourceEntry {
                content_hash: "sha256:old".to_string(),
                prompt_hash: "sha256:def".to_string(),
                memory_ids: vec!["id-1".to_string()],
                last_learned: "2026-01-01T00:00:00Z".to_string(),
                sync: None,
            },
        );
        assert_eq!(
            manifest.is_unchanged("test.txt", "sha256:new", "sha256:def"),
            Some(false)
        );
    }

    #[test]
    fn test_is_unchanged_prompt_changed() {
        let mut manifest = ContentManifest::new("knowledge");
        manifest.upsert_source(
            "test.txt".to_string(),
            SourceEntry {
                content_hash: "sha256:abc".to_string(),
                prompt_hash: "sha256:old".to_string(),
                memory_ids: vec!["id-1".to_string()],
                last_learned: "2026-01-01T00:00:00Z".to_string(),
                sync: None,
            },
        );
        assert_eq!(
            manifest.is_unchanged("test.txt", "sha256:abc", "sha256:new"),
            Some(false)
        );
    }

    #[test]
    fn test_find_orphaned_sources() {
        let mut manifest = ContentManifest::new("knowledge");
        for key in &["a.md", "b.md", "c.md"] {
            manifest.upsert_source(
                key.to_string(),
                SourceEntry {
                    content_hash: "sha256:x".to_string(),
                    prompt_hash: "sha256:y".to_string(),
                    memory_ids: vec![],
                    last_learned: "2026-01-01T00:00:00Z".to_string(),
                    sync: None,
                },
            );
        }
        let current = vec!["a.md".to_string(), "c.md".to_string()];
        let mut orphaned = manifest.find_orphaned_sources(&current);
        orphaned.sort();
        assert_eq!(orphaned, vec!["b.md".to_string()]);
    }

    #[test]
    fn test_remove_source() {
        let mut manifest = ContentManifest::new("knowledge");
        manifest.upsert_source(
            "old-doc.md".to_string(),
            SourceEntry {
                content_hash: "sha256:abc".to_string(),
                prompt_hash: "sha256:def".to_string(),
                memory_ids: vec!["id-1".to_string(), "id-2".to_string(), "id-3".to_string()],
                last_learned: "2026-01-01T00:00:00Z".to_string(),
                sync: None,
            },
        );
        let removed = manifest.remove_source("old-doc.md");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().memory_ids.len(), 3);
        assert!(manifest.sources.get("old-doc.md").is_none());
    }

    #[test]
    fn test_manifest_save_load_roundtrip() {
        let tmp_dir = std::env::temp_dir();
        let manifest_path = tmp_dir.join("test-pupil-manifest.json");

        let mut manifest = ContentManifest::new("test-ns");
        manifest.embedding_provider = Some("ollama".into());
        manifest.embedding_model = Some("embeddinggemma".into());
        manifest.embedding_dims = Some(768);
        manifest.upsert_source(
            "doc.md".to_string(),
            SourceEntry {
                content_hash: "sha256:abc123".to_string(),
                prompt_hash: "sha256:def456".to_string(),
                memory_ids: vec!["id-1".to_string(), "id-2".to_string()],
                last_learned: "2026-06-25T10:30:00Z".to_string(),
                sync: None,
            },
        );

        manifest.save(&manifest_path).unwrap();
        let loaded = ContentManifest::load(&manifest_path).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.namespace, "test-ns");
        assert_eq!(loaded.sources.len(), 1);
        assert_eq!(loaded.sources["doc.md"].content_hash, "sha256:abc123");
        assert_eq!(loaded.sources["doc.md"].memory_ids.len(), 2);

        let _ = std::fs::remove_file(&manifest_path);
    }
}
