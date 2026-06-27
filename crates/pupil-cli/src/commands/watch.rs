use clap::Args;
use console::Style;
use notify::EventKind;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc as tokio_mpsc;

use crate::agent_config::{self, AgentConfig, CurriculumConfig, SourceConfig, SourceEntry, BASE_IMAGE};
use crate::container::{self, ContainerId, ContainerRuntime, RunOptions};
use crate::error::CliError;

/// Development mode: watch curriculum for changes and hot-reload the agent.
#[derive(Args, Debug, Clone)]
pub struct WatchArgs {
    /// Agent name. Optional if pupil.yaml exists in the current directory.
    pub name: Option<String>,

    /// Run the test suite after each successful re-learning pass.
    #[arg(long, default_value_t = false)]
    pub test: bool,

    /// Path to the test file. Only used when --test is enabled.
    /// Defaults to tests.yaml in the agent directory.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Debounce timeout in milliseconds. Events within this window
    /// are coalesced into a single logical change.
    #[arg(long, default_value_t = 500, value_name = "MS")]
    pub debounce: u64,

    /// Skip the initial full learning pass on startup. Assumes the
    /// dev volume already contains learned data from a previous session.
    #[arg(long, default_value_t = false)]
    pub no_initial_learn: bool,
}

#[derive(Error, Debug)]
pub enum WatchError {
    #[error("container error: {0}")]
    Container(String),

    #[error("config read error: {0}")]
    ConfigRead(String),

    #[error("config parse error: {0}")]
    ConfigParse(String),

    #[error("re-learn exec error: {0}")]
    RelearnExec(String),

    #[error("test exec error: {0}")]
    TestExec(String),

    #[error("test output parse error: {0}")]
    TestParse(String),

    #[error("watcher error: {0}")]
    Watcher(String),
}

impl From<WatchError> for CliError {
    fn from(e: WatchError) -> Self {
        CliError::ContainerRuntimeError {
            message: e.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum WatchEvent {
    CurriculumChanged(Vec<PathBuf>),
    ConfigChanged,
    WatchError(String),
}

#[derive(Debug)]
pub struct RelearnResult {
    pub source: PathBuf,
    pub action: RelearnAction,
    pub memories_stored: Option<usize>,
    pub duration: Duration,
}

#[derive(Debug)]
pub enum RelearnAction {
    Learned,
    Relearned,
    Forgotten,
    Skipped,
    Error(String),
}

#[derive(Debug)]
#[allow(dead_code)]
enum ConfigChangeKind {
    LearningChanged(Vec<String>),
    RuntimeChanged,
    Both {
        learning_sources: Vec<String>,
        runtime: bool,
    },
    NoOp,
}

pub struct WatchPrinter {
    prefix_style: Style,
    error_style: Style,
    success_style: Style,
}

impl WatchPrinter {
    pub fn new() -> Self {
        Self {
            prefix_style: Style::new().dim().italic(),
            error_style: Style::new().red().bold(),
            success_style: Style::new().green(),
        }
    }

    pub fn status(&self, message: &str) {
        eprintln!(
            "{} {}",
            self.prefix_style.apply_to("[watch]"),
            self.prefix_style.apply_to(message),
        );
    }

    pub fn relearn_result(&self, result: &RelearnResult) {
        let source_name = result.source.display();
        let duration_secs = result.duration.as_secs_f64();

        match &result.action {
            RelearnAction::Learned => {
                let count = result.memories_stored.unwrap_or(0);
                eprintln!(
                    "{} {}: {} memories stored ({:.1}s)",
                    self.prefix_style.apply_to("[watch]"),
                    self.success_style.apply_to(source_name.to_string()),
                    count,
                    duration_secs,
                );
            }
            RelearnAction::Relearned => {
                let count = result.memories_stored.unwrap_or(0);
                eprintln!(
                    "{} {} changed, re-learned: {} memories stored ({:.1}s)",
                    self.prefix_style.apply_to("[watch]"),
                    self.success_style.apply_to(source_name.to_string()),
                    count,
                    duration_secs,
                );
            }
            RelearnAction::Forgotten => {
                let count = result.memories_stored.unwrap_or(0);
                eprintln!(
                    "{} {} removed, {} memories forgotten",
                    self.prefix_style.apply_to("[watch]"),
                    source_name,
                    count,
                );
            }
            RelearnAction::Skipped => {}
            RelearnAction::Error(msg) => {
                eprintln!(
                    "{} {}: {} - {}. Skipping.",
                    self.prefix_style.apply_to("[watch]"),
                    self.error_style.apply_to("ERROR"),
                    source_name,
                    msg,
                );
            }
        }
    }

    pub fn test_result(&self, test_name: &str, passed: bool, duration_secs: f64, detail: &str) {
        let status = if passed {
            self.success_style.apply_to("PASS").to_string()
        } else {
            self.error_style.apply_to("FAIL").to_string()
        };

        eprintln!(
            "{} {}: \"{}\" ({:.1}s){}",
            self.prefix_style.apply_to("[watch]"),
            status,
            test_name,
            duration_secs,
            if detail.is_empty() {
                String::new()
            } else {
                format!(" - {detail}")
            },
        );
    }

    pub fn test_summary(&self, passed: usize, total: usize, duration_secs: f64) {
        eprintln!(
            "{} Tests: {} passed, {} failed ({:.1}s)",
            self.prefix_style.apply_to("[watch]"),
            passed,
            total - passed,
            duration_secs,
        );
    }

    pub fn banner(&self) {
        eprintln!(
            "{} Watching curriculum/ and pupil.yaml for changes",
            self.prefix_style.apply_to("[watch]"),
        );
        eprintln!(
            "{} Chat ready. Type your message, or Ctrl+C to stop.",
            self.prefix_style.apply_to("[watch]"),
        );
    }

    pub fn shutdown_summary(&self, stats: &WatchSessionStats) {
        eprintln!(
            "{} Shutting down...",
            self.prefix_style.apply_to("[watch]"),
        );
        eprintln!(
            "{} Session summary: {} files re-learned, {} memories updated, {} test runs",
            self.prefix_style.apply_to("[watch]"),
            stats.files_relearned,
            stats.memories_updated,
            stats.test_runs,
        );
        eprintln!(
            "{} Dev container stopped. Run `pupil watch` to resume.",
            self.prefix_style.apply_to("[watch]"),
        );
    }
}

#[derive(Debug, Default)]
pub struct WatchSessionStats {
    pub files_relearned: usize,
    pub memories_updated: usize,
    pub test_runs: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestSource {
    pub content_hash: String,
    pub memory_ids: Vec<String>,
    pub last_learned: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Manifest {
    pub version: u32,
    pub namespace: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dims: u32,
    pub sources: HashMap<String, ManifestSource>,
}

impl Manifest {
    pub fn source_hash(&self, key: &str) -> Option<String> {
        self.sources.get(key).map(|s| s.content_hash.clone())
    }

    pub fn source_memory_count(&self, key: &str) -> Option<usize> {
        self.sources.get(key).map(|s| s.memory_ids.len())
    }

    pub fn update_source(&mut self, key: &str, hash: &str, memory_ids: &[String]) {
        let entry = self.sources.entry(key.to_string()).or_insert_with(|| ManifestSource {
            content_hash: String::new(),
            memory_ids: Vec::new(),
            last_learned: chrono::Utc::now().to_rfc3339(),
        });
        entry.content_hash = hash.to_string();
        entry.memory_ids = memory_ids.to_vec();
        entry.last_learned = chrono::Utc::now().to_rfc3339();
    }

    pub fn remove_source(&mut self, key: &str) {
        self.sources.remove(key);
    }
}

#[derive(Debug, serde::Deserialize)]
struct TestOutput {
    summary: TestSummary,
    tests: Vec<TestResultEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TestSummary {
    total: usize,
    passed: usize,
    #[allow(dead_code)]
    failed: usize,
}

#[derive(Debug, serde::Deserialize)]
struct TestResultEntry {
    name: String,
    passed: bool,
    latency_ms: u64,
    assertions: Vec<AssertionResultEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct AssertionResultEntry {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    assertion_type: String,
    passed: bool,
    detail: String,
}

fn dev_container_name(agent_name: &str) -> String {
    format!("pupil-dev-{}", sanitize_name(agent_name))
}

fn dev_volume_name(agent_name: &str) -> String {
    format!("pupil-dev-{}-data", sanitize_name(agent_name))
}

fn sanitize_name(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let mut result = String::with_capacity(s.len());
    let mut prev_hyphen = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result.trim_matches('-').to_string()
}

#[allow(dead_code)]
fn dev_compose_path(agent_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("pupil").join("watch");
    std::fs::create_dir_all(&dir).ok();
    dir.join(format!("{}-compose.yaml", sanitize_name(agent_name)))
}

fn dev_container_run_options(
    agent_dir: &Path,
    agent_name: &str,
    _config: &AgentConfig,
    env_vars: &[(String, String)],
) -> RunOptions {
    let curriculum_host = agent_dir.join("curriculum").to_string_lossy().to_string();
    let volume_name = dev_volume_name(agent_name);

    let mut env_map = HashMap::new();
    for (k, v) in env_vars {
        env_map.insert(k.clone(), v.clone());
    }

    RunOptions {
        name: Some(dev_container_name(agent_name)),
        volumes: vec![
            format!("{}:/curriculum:ro", curriculum_host),
            format!("{}:/data", volume_name),
        ],
        env: env_map,
        entrypoint: Some("sleep".to_string()),
        command: vec!["infinity".to_string()],
        detach: true,
        ..Default::default()
    }
}

fn should_process(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };

    if name.ends_with(".swp")
        || name.ends_with(".swo")
        || name.ends_with('~')
        || name.starts_with(".#")
        || name.ends_with(".tmp")
        || name.ends_with(".bak")
        || name == "4913"
    {
        return false;
    }

    if name.starts_with('.') {
        return false;
    }

    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "txt" | "pdf" | "html" | "htm" | "json" | "csv" | "yaml" | "yml")
    )
}

fn filter_and_classify_events(
    events: &[DebouncedEvent],
    curriculum_dir: &Path,
    config_path: &Path,
) -> Vec<WatchEvent> {
    let mut curriculum_paths: Vec<PathBuf> = Vec::new();
    let mut config_changed = false;

    for event in events {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
            _ => continue,
        }

        for path in &event.paths {
            if path == config_path {
                config_changed = true;
                continue;
            }

            if !path.starts_with(curriculum_dir) {
                continue;
            }

            if !should_process(path) {
                continue;
            }

            if let Ok(rel) = path.strip_prefix(curriculum_dir) {
                if !curriculum_paths.contains(&rel.to_path_buf()) {
                    curriculum_paths.push(rel.to_path_buf());
                }
            }
        }
    }

    let mut result = Vec::new();
    if !curriculum_paths.is_empty() {
        result.push(WatchEvent::CurriculumChanged(curriculum_paths));
    }
    if config_changed {
        result.push(WatchEvent::ConfigChanged);
    }
    result
}

fn start_watcher(
    curriculum_path: PathBuf,
    config_path: PathBuf,
    debounce_ms: u64,
) -> Result<tokio_mpsc::UnboundedReceiver<WatchEvent>, WatchError> {
    let (async_tx, async_rx) = tokio_mpsc::unbounded_channel();

    let curriculum_path_clone = curriculum_path.clone();

    tokio::task::spawn_blocking(move || {
        let (sync_tx, sync_rx) = mpsc::channel();

        let mut debouncer = new_debouncer(
            Duration::from_millis(debounce_ms),
            None,
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    let _ = sync_tx.send(events);
                }
            },
        )
        .expect("failed to create filesystem watcher");

        debouncer
            .watch(&curriculum_path, RecursiveMode::Recursive)
            .expect("failed to watch curriculum directory");

        debouncer
            .watch(&config_path, RecursiveMode::NonRecursive)
            .expect("failed to watch pupil.yaml");

        loop {
            match sync_rx.recv() {
                Ok(events) => {
                    let watch_events =
                        filter_and_classify_events(&events, &curriculum_path_clone, &config_path);
                    for event in watch_events {
                        if async_tx.send(event).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => {
                    return;
                }
            }
        }
    });

    Ok(async_rx)
}

fn compute_file_hash(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = hasher.finalize();
    Ok(format!("sha256:{}", hex::encode(hash)))
}

fn parse_memories_count(stdout: &str) -> usize {
    for line in stdout.lines() {
        let line = line.trim();
        if line.contains("memories stored") || line.contains("memories created") {
            if let Some(num_str) = line.split_whitespace().next() {
                if let Ok(n) = num_str.parse::<usize>() {
                    return n;
                }
            }
        }
    }
    0
}

fn parse_sources_count(stdout: &str) -> usize {
    for line in stdout.lines() {
        let line = line.trim();
        if line.contains("sources") || line.contains("source") {
            if let Some(num_str) = line.split_whitespace().next() {
                if let Ok(n) = num_str.parse::<usize>() {
                    return n;
                }
            }
        }
    }
    0
}

fn parse_memory_ids(stdout: &str) -> Vec<String> {
    if let Ok(ids) = serde_json::from_str::<Vec<String>>(stdout.trim()) {
        return ids;
    }

    let uuid_pattern =
        regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
            .unwrap();
    uuid_pattern
        .find_iter(stdout)
        .map(|m| m.as_str().to_string())
        .collect()
}

async fn relearn_source(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    curriculum_host_path: &Path,
    source: &Path,
    manifest: &mut Manifest,
) -> RelearnResult {
    let start = std::time::Instant::now();

    let full_path = curriculum_host_path.join(source);
    let new_hash = match compute_file_hash(&full_path) {
        Ok(h) => h,
        Err(e) => {
            return RelearnResult {
                source: source.to_path_buf(),
                action: RelearnAction::Error(format!("failed to hash file: {e}")),
                memories_stored: None,
                duration: start.elapsed(),
            };
        }
    };

    let source_key = source.to_string_lossy().to_string();
    let manifest_hash = manifest.source_hash(&source_key);

    if manifest_hash.as_deref() == Some(new_hash.as_str()) {
        return RelearnResult {
            source: source.to_path_buf(),
            action: RelearnAction::Skipped,
            memories_stored: None,
            duration: start.elapsed(),
        };
    }

    let is_relearn = manifest_hash.is_some();

    if is_relearn {
        let forget_result = runtime
            .exec(
                &ContainerId(container_id.to_string()),
                &["pupil-agent", "forget-source", &source_key],
                &[],
            )
            .await;

        if let Err(e) = forget_result {
            return RelearnResult {
                source: source.to_path_buf(),
                action: RelearnAction::Error(format!("forget failed: {e}")),
                memories_stored: None,
                duration: start.elapsed(),
            };
        }
    }

    let learn_result = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &["pupil-agent", "learn", "--source", &source_key],
            &[],
        )
        .await;

    match learn_result {
        Ok(output) => {
            let memories_stored = parse_memories_count(&output.stdout);
            let memory_ids = parse_memory_ids(&output.stdout);
            manifest.update_source(&source_key, &new_hash, &memory_ids);

            RelearnResult {
                source: source.to_path_buf(),
                action: if is_relearn {
                    RelearnAction::Relearned
                } else {
                    RelearnAction::Learned
                },
                memories_stored: Some(memories_stored),
                duration: start.elapsed(),
            }
        }
        Err(e) => RelearnResult {
            source: source.to_path_buf(),
            action: RelearnAction::Error(format!("learn failed: {e}")),
            memories_stored: None,
            duration: start.elapsed(),
        },
    }
}

async fn forget_source(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    source: &Path,
    manifest: &mut Manifest,
) -> RelearnResult {
    let start = std::time::Instant::now();
    let source_key = source.to_string_lossy().to_string();
    let memory_count = manifest.source_memory_count(&source_key).unwrap_or(0);

    let result = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &["pupil-agent", "forget-source", &source_key],
            &[],
        )
        .await;

    match result {
        Ok(_) => {
            manifest.remove_source(&source_key);
            RelearnResult {
                source: source.to_path_buf(),
                action: RelearnAction::Forgotten,
                memories_stored: Some(memory_count),
                duration: start.elapsed(),
            }
        }
        Err(e) => RelearnResult {
            source: source.to_path_buf(),
            action: RelearnAction::Error(format!("forget failed: {e}")),
            memories_stored: None,
            duration: start.elapsed(),
        },
    }
}

fn hash_effective_prompt(source: &SourceConfig, curriculum: &CurriculumConfig) -> String {
    let effective = match (&source.learning_prompt, &source.learning_profile) {
        (Some(prompt), _) => prompt.clone(),
        (_, Some(profile)) => profile.clone(),
        (None, None) => curriculum
            .learning_profile
            .clone()
            .unwrap_or_else(|| "general".to_string()),
    };
    let mut hasher = Sha256::new();
    hasher.update(effective.as_bytes());
    hex::encode(hasher.finalize())
}

fn source_config_key(source: &SourceConfig) -> String {
    if let Some(ref path) = source.path {
        path.clone()
    } else if let Some(ref url) = source.url {
        url.clone()
    } else if let Some(ref glob) = source.glob {
        glob.clone()
    } else {
        String::new()
    }
}

fn diff_learning_config(old: &AgentConfig, new: &AgentConfig) -> Vec<String> {
    let mut affected = Vec::new();

    let new_sources: Vec<&SourceConfig> = new
        .curriculum
        .sources
        .iter()
        .filter_map(|s| match s {
            SourceEntry::Long(sc) => Some(sc),
            SourceEntry::Short(_) => None,
        })
        .collect();

    let old_sources: Vec<&SourceConfig> = old
        .curriculum
        .sources
        .iter()
        .filter_map(|s| match s {
            SourceEntry::Long(sc) => Some(sc),
            SourceEntry::Short(_) => None,
        })
        .collect();

    for new_source in &new_sources {
        let key = source_config_key(new_source);
        let new_prompt_hash = hash_effective_prompt(new_source, &new.curriculum);
        let old_prompt_hash = old_sources
            .iter()
            .find(|s| source_config_key(s) == key)
            .map(|s| hash_effective_prompt(s, &old.curriculum));

        match old_prompt_hash {
            Some(old_hash) if old_hash != new_prompt_hash => {
                affected.push(key);
            }
            None => {}
            _ => {}
        }
    }

    affected
}

fn mcp_servers_changed(old: &AgentConfig, new: &AgentConfig) -> bool {
    let old_json = serde_json::to_string(&old.mcp_servers).unwrap_or_default();
    let new_json = serde_json::to_string(&new.mcp_servers).unwrap_or_default();
    old_json != new_json
}

fn diff_config(old: &AgentConfig, new: &AgentConfig) -> ConfigChangeKind {
    let runtime_changed = old.system_prompt != new.system_prompt
        || old.model != new.model
        || mcp_servers_changed(old, new);

    let affected_sources = diff_learning_config(old, new);
    let learning_changed = !affected_sources.is_empty();

    match (learning_changed, runtime_changed) {
        (true, true) => ConfigChangeKind::Both {
            learning_sources: affected_sources,
            runtime: true,
        },
        (true, false) => ConfigChangeKind::LearningChanged(affected_sources),
        (false, true) => ConfigChangeKind::RuntimeChanged,
        (false, false) => ConfigChangeKind::NoOp,
    }
}

async fn restart_agent_process(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    _config: &AgentConfig,
) -> Result<(), WatchError> {
    let _ = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &["pkill", "-TERM", "pupil-agent"],
            &[],
        )
        .await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    Ok(())
}

async fn handle_config_change(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    config_path: &Path,
    old_config: &mut AgentConfig,
    manifest: &mut Manifest,
    curriculum_host_path: &Path,
    printer: &WatchPrinter,
) -> Result<(), WatchError> {
    let config_text = std::fs::read_to_string(config_path).map_err(|e| {
        WatchError::ConfigRead(format!("failed to read pupil.yaml: {e}"))
    })?;

    let new_config: AgentConfig = serde_yml::from_str(&config_text).map_err(|e| {
        WatchError::ConfigParse(format!("failed to parse pupil.yaml: {e}"))
    })?;

    let change_kind = diff_config(old_config, &new_config);

    match change_kind {
        ConfigChangeKind::NoOp => {
            printer.status("pupil.yaml changed but no impactful differences detected");
        }
        ConfigChangeKind::LearningChanged(sources) => {
            printer.status(&format!(
                "learning profile changed for {} source(s), re-learning...",
                sources.len()
            ));
            for source_key in &sources {
                let source_path = PathBuf::from(source_key);
                let result = relearn_source(
                    runtime,
                    container_id,
                    curriculum_host_path,
                    &source_path,
                    manifest,
                )
                .await;
                printer.relearn_result(&result);
            }
        }
        ConfigChangeKind::RuntimeChanged => {
            printer.status("pupil.yaml changed, restarting agent...");
            restart_agent_process(runtime, container_id, &new_config).await?;
            printer.status("agent restarted with updated config");
        }
        ConfigChangeKind::Both {
            learning_sources,
            runtime: _,
        } => {
            printer.status(&format!(
                "learning profile changed for {} source(s), re-learning...",
                learning_sources.len()
            ));
            for source_key in &learning_sources {
                let source_path = PathBuf::from(source_key);
                let result = relearn_source(
                    runtime,
                    container_id,
                    curriculum_host_path,
                    &source_path,
                    manifest,
                )
                .await;
                printer.relearn_result(&result);
            }
            printer.status("pupil.yaml runtime config changed, restarting agent...");
            restart_agent_process(runtime, container_id, &new_config).await?;
            printer.status("agent restarted with updated config");
        }
    }

    *old_config = new_config;
    Ok(())
}

async fn run_tests(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    test_file: &Path,
    printer: &WatchPrinter,
) -> Result<(usize, usize), WatchError> {
    let test_file_str = test_file.to_string_lossy();
    printer.status(&format!("Running tests from {test_file_str}..."));

    let start = std::time::Instant::now();

    let output = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &[
                "pupil-agent",
                "test",
                "--file",
                &test_file_str,
                "--json",
            ],
            &[],
        )
        .await
        .map_err(|e| WatchError::TestExec(format!("failed to exec test: {e}")))?;

    let duration = start.elapsed();

    let test_output: TestOutput = serde_json::from_str(&output.stdout).map_err(|e| {
        WatchError::TestParse(format!("failed to parse test output: {e}"))
    })?;

    for test_result in &test_output.tests {
        let detail = if test_result.passed {
            String::new()
        } else {
            test_result
                .assertions
                .iter()
                .find(|a| !a.passed)
                .map(|a| a.detail.clone())
                .unwrap_or_default()
        };

        printer.test_result(
            &test_result.name,
            test_result.passed,
            test_result.latency_ms as f64 / 1000.0,
            &detail,
        );
    }

    let passed = test_output.summary.passed;
    let total = test_output.summary.total;
    printer.test_summary(passed, total, duration.as_secs_f64());

    Ok((passed, total))
}

async fn load_dev_manifest(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
) -> Result<Manifest, WatchError> {
    let output = runtime
        .exec(container_id, &["cat", "/data/.pupil-manifest.json"], &[])
        .await;

    match output {
        Ok(out) => serde_json::from_str(&out.stdout).map_err(|e| {
            WatchError::Container(format!("failed to parse manifest: {e}"))
        }),
        Err(_) => Ok(Manifest {
            version: 1,
            namespace: "knowledge".to_string(),
            embedding_provider: "ollama".to_string(),
            embedding_model: "embeddinggemma".to_string(),
            embedding_dims: 768,
            sources: HashMap::new(),
        }),
    }
}

async fn save_dev_manifest(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
    manifest: &Manifest,
) -> Result<(), WatchError> {
    let json = serde_json::to_string_pretty(manifest).map_err(|e| {
        WatchError::Container(format!("failed to serialize manifest: {e}"))
    })?;

    runtime
        .exec(
            container_id,
            &[
                "sh",
                "-c",
                &format!(
                    "cat > /data/.pupil-manifest.json << 'MANIFEST_EOF'\n{json}\nMANIFEST_EOF"
                ),
            ],
            &[],
        )
        .await
        .map_err(|e| WatchError::Container(format!("failed to save manifest: {e}")))?;

    Ok(())
}

async fn ensure_dev_container(
    runtime: &dyn ContainerRuntime,
    agent_dir: &Path,
    agent_name: &str,
    config: &AgentConfig,
    env_vars: &[(String, String)],
    printer: &WatchPrinter,
) -> Result<ContainerId, WatchError> {
    let container_name = dev_container_name(agent_name);

    let inspect_result = runtime
        .exec(
            &ContainerId(container_name.clone()),
            &["true"],
            &[],
        )
        .await;

    match inspect_result {
        Ok(_) => {
            printer.status(&format!(
                "Attaching to existing dev container {container_name}"
            ));
            return Ok(ContainerId(container_name));
        }
        Err(_) => {
            let _ = runtime
                .rm(&ContainerId(container_name.clone()), true)
                .await;
        }
    }

    printer.status("Starting dev container...");
    let start = std::time::Instant::now();

    let opts = dev_container_run_options(agent_dir, agent_name, config, env_vars);
    let container_id = runtime
        .run(BASE_IMAGE, &opts)
        .await
        .map_err(|e| WatchError::Container(format!("failed to start dev container: {e}")))?;

    printer.status(&format!(
        "Dev container started ({:.1}s)",
        start.elapsed().as_secs_f64()
    ));

    Ok(container_id)
}

async fn start_agent_exec(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
) -> Result<tokio::process::Child, WatchError> {
    let runtime_cmd = runtime.name();

    let child = TokioCommand::new(runtime_cmd)
        .args(["exec", "-i", container_id, "pupil-agent"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| WatchError::Container(format!("failed to start agent: {e}")))?;

    Ok(child)
}

fn resolve_env_vars(config: &AgentConfig) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    let (key_name, is_set) = agent_config::resolve_api_key(&config.model);
    if is_set {
        if let Ok(val) = std::env::var(&key_name) {
            vars.push((key_name, val));
        }
    }
    if let Some(ref learning_model) = config.learning_model {
        let (learn_key, learn_set) = agent_config::resolve_api_key(learning_model);
        if learn_set && !vars.iter().any(|(k, _)| k == &learn_key) {
            if let Ok(val) = std::env::var(&learn_key) {
                vars.push((learn_key, val));
            }
        }
    }
    vars
}

pub async fn execute(args: WatchArgs) -> Result<(), CliError> {
    let (agent_dir, config) = agent_config::resolve_agent(args.name.as_deref())?;
    let agent_name = config.name.clone();
    let curriculum_host_path = agent_dir.join("curriculum");
    let config_path = agent_dir.join("pupil.yaml");

    if args.debounce < 100 || args.debounce > 10000 {
        return Err(CliError::ConfigInvalid {
            message: format!(
                "Debounce must be between 100ms and 10000ms, got {}ms. \
                 The default of 500ms works well for most editors.",
                args.debounce
            ),
        });
    }

    let test_file = if args.test {
        let path = args.file.unwrap_or_else(|| agent_dir.join("tests.yaml"));
        if !path.exists() {
            return Err(CliError::ConfigInvalid {
                message: format!(
                    "Test file not found: {}. Create a tests.yaml file or specify --file <path>.",
                    path.display()
                ),
            });
        }
        Some(path)
    } else {
        if args.file.is_some() {
            eprintln!(
                "{} --file has no effect without --test",
                Style::new().dim().italic().apply_to("[watch]")
            );
        }
        None
    };

    let runtime = container::detect().map_err(|e| CliError::ContainerRuntimeError {
        message: format!("Container runtime not found: {e}"),
    })?;

    let env_vars = resolve_env_vars(&config);

    let printer = WatchPrinter::new();
    let container_id =
        ensure_dev_container(runtime.as_ref(), &agent_dir, &agent_name, &config, &env_vars, &printer)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: e.to_string(),
            })?;

    if !args.no_initial_learn {
        printer.status("Running initial learning pass...");
        let start = std::time::Instant::now();
        let output = runtime
            .exec(&container_id, &["pupil-agent", "learn"], &[])
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("initial learn failed: {e}"),
            })?;

        let memories = parse_memories_count(&output.stdout);
        let sources = parse_sources_count(&output.stdout);
        printer.status(&format!(
            "Initial learning: {} sources, {} memories ({:.1}s)",
            sources,
            memories,
            start.elapsed().as_secs_f64(),
        ));
    } else {
        printer.status("Skipping initial learning pass (--no-initial-learn)");
    }

    let mut manifest = load_dev_manifest(runtime.as_ref(), &container_id)
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: e.to_string(),
        })?;

    // Keep the child process handle alive to prevent premature termination.
    // Reassigned on agent restart; the final value is intentionally never read.
    #[allow(unused_assignments, unused_variables)]
    let mut agent_exec = start_agent_exec(runtime.as_ref(), &container_id.0)
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: e.to_string(),
        })?;

    let mut watch_rx =
        start_watcher(curriculum_host_path.clone(), config_path.clone(), args.debounce)
            .map_err(|e| CliError::ContainerRuntimeError {
                message: e.to_string(),
            })?;

    printer.banner();

    let mut stats = WatchSessionStats::default();
    let mut current_config = config.clone();

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut agent_stdout = BufReader::new(agent_exec.stdout.take().unwrap());
    let mut agent_stdin = agent_exec.stdin.take().unwrap();
    let mut stdout_buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            line = stdin.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Err(e) = agent_stdin
                            .write_all(format!("{line}\n").as_bytes())
                            .await
                        {
                            printer.status(&format!("Failed to send to agent: {e}"));
                        }
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        printer.status(&format!("stdin error: {e}"));
                        break;
                    }
                }
            }

            n = agent_stdout.read(&mut stdout_buf) => {
                match n {
                    Ok(0) => {
                        printer.status("Agent process exited. Restarting...");
                        match start_agent_exec(runtime.as_ref(), &container_id.0).await {
                            Ok(mut new_exec) => {
                                agent_stdout = BufReader::new(new_exec.stdout.take().unwrap());
                                agent_stdin = new_exec.stdin.take().unwrap();
                                #[allow(unused_assignments)]
                                { agent_exec = new_exec; }
                            }
                            Err(e) => {
                                printer.status(&format!("Failed to restart agent: {e}"));
                                break;
                            }
                        }
                    }
                    Ok(n) => {
                        let mut host_stdout = tokio::io::stdout();
                        let _ = host_stdout.write_all(&stdout_buf[..n]).await;
                        let _ = host_stdout.flush().await;
                    }
                    Err(e) => {
                        printer.status(&format!("Agent output error: {e}"));
                    }
                }
            }

            event = watch_rx.recv() => {
                match event {
                    Some(WatchEvent::CurriculumChanged(paths)) => {
                        let mut any_learned = false;
                        for path in &paths {
                            let full_path = curriculum_host_path.join(path);
                            if full_path.exists() {
                                printer.status(&format!("{} changed, re-learning...", path.display()));
                                let result = relearn_source(
                                    runtime.as_ref(),
                                    &container_id.0,
                                    &curriculum_host_path,
                                    path,
                                    &mut manifest,
                                ).await;
                                if matches!(result.action, RelearnAction::Learned | RelearnAction::Relearned) {
                                    any_learned = true;
                                    stats.files_relearned += 1;
                                    stats.memories_updated += result.memories_stored.unwrap_or(0);
                                }
                                printer.relearn_result(&result);
                            } else {
                                printer.status(&format!("{} removed, forgetting...", path.display()));
                                let result = forget_source(
                                    runtime.as_ref(),
                                    &container_id.0,
                                    path,
                                    &mut manifest,
                                ).await;
                                stats.files_relearned += 1;
                                printer.relearn_result(&result);
                            }
                        }

                        if let Err(e) = save_dev_manifest(runtime.as_ref(), &container_id, &manifest).await {
                            printer.status(&format!("Failed to save manifest: {e}"));
                        }

                        if any_learned {
                            if let Some(ref test_path) = test_file {
                                stats.test_runs += 1;
                                let _ = run_tests(
                                    runtime.as_ref(),
                                    &container_id.0,
                                    test_path,
                                    &printer,
                                ).await;
                            }
                        }
                    }
                    Some(WatchEvent::ConfigChanged) => {
                        if let Err(e) = handle_config_change(
                            runtime.as_ref(),
                            &container_id.0,
                            &config_path,
                            &mut current_config,
                            &mut manifest,
                            &curriculum_host_path,
                            &printer,
                        ).await {
                            printer.status(&format!("Config change handling failed: {e}"));
                        }
                    }
                    Some(WatchEvent::WatchError(msg)) => {
                        printer.status(&format!("Watcher error: {msg}"));
                    }
                    None => {
                        printer.status("Filesystem watcher stopped unexpectedly");
                        break;
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    printer.shutdown_summary(&stats);

    let _ = runtime
        .exec(&container_id, &["pkill", "-TERM", "pupil-agent"], &[])
        .await;

    let _ = runtime.rm(&container_id, false).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_should_process_markdown() {
        assert!(should_process(Path::new("handbook.md")));
        assert!(should_process(Path::new("dir/nested/file.md")));
    }

    #[test]
    fn test_should_process_txt() {
        assert!(should_process(Path::new("notes.txt")));
    }

    #[test]
    fn test_should_process_pdf() {
        assert!(should_process(Path::new("spec.pdf")));
    }

    #[test]
    fn test_should_process_html() {
        assert!(should_process(Path::new("page.html")));
        assert!(should_process(Path::new("page.htm")));
    }

    #[test]
    fn test_should_process_json_csv_yaml() {
        assert!(should_process(Path::new("data.json")));
        assert!(should_process(Path::new("data.csv")));
        assert!(should_process(Path::new("config.yaml")));
        assert!(should_process(Path::new("config.yml")));
    }

    #[test]
    fn test_should_process_rejects_vim_swap() {
        assert!(!should_process(Path::new("file.md.swp")));
        assert!(!should_process(Path::new("file.md.swo")));
        assert!(!should_process(Path::new("4913")));
    }

    #[test]
    fn test_should_process_rejects_backup_files() {
        assert!(!should_process(Path::new("file.md~")));
        assert!(!should_process(Path::new("file.md.bak")));
        assert!(!should_process(Path::new("file.md.tmp")));
    }

    #[test]
    fn test_should_process_rejects_emacs_lockfiles() {
        assert!(!should_process(Path::new(".#file.md")));
    }

    #[test]
    fn test_should_process_rejects_dotfiles() {
        assert!(!should_process(Path::new(".DS_Store")));
        assert!(!should_process(Path::new(".gitignore")));
        assert!(!should_process(Path::new(".hidden.md")));
    }

    #[test]
    fn test_should_process_rejects_unknown_extensions() {
        assert!(!should_process(Path::new("image.png")));
        assert!(!should_process(Path::new("script.rs")));
        assert!(!should_process(Path::new("binary.exe")));
    }

    #[test]
    fn test_should_process_rejects_no_extension() {
        assert!(!should_process(Path::new("README")));
        assert!(!should_process(Path::new("Makefile")));
    }

    #[test]
    fn test_sanitize_simple() {
        assert_eq!(sanitize_name("onboarding-bot"), "onboarding-bot");
    }

    #[test]
    fn test_sanitize_uppercase() {
        assert_eq!(sanitize_name("MyAgent"), "myagent");
    }

    #[test]
    fn test_sanitize_special_chars() {
        assert_eq!(sanitize_name("my_agent.v2"), "my-agent-v2");
    }

    #[test]
    fn test_sanitize_consecutive_hyphens() {
        assert_eq!(sanitize_name("my--agent"), "my-agent");
    }

    #[test]
    fn test_sanitize_leading_trailing_hyphens() {
        assert_eq!(sanitize_name("-agent-"), "agent");
    }

    #[test]
    fn test_hash_deterministic() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"hello world").unwrap();
        let h1 = compute_file_hash(f.path()).unwrap();
        let h2 = compute_file_hash(f.path()).unwrap();
        assert_eq!(h1, h2);
        assert!(h1.starts_with("sha256:"));
    }

    #[test]
    fn test_hash_changes_with_content() {
        let mut f1 = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f1, b"content A").unwrap();
        let h1 = compute_file_hash(f1.path()).unwrap();

        let mut f2 = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f2, b"content B").unwrap();
        let h2 = compute_file_hash(f2.path()).unwrap();

        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_nonexistent_file() {
        let result = compute_file_hash(Path::new("/nonexistent/file.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_memories_stored() {
        assert_eq!(parse_memories_count("12 memories stored"), 12);
    }

    #[test]
    fn test_parse_memories_created() {
        assert_eq!(parse_memories_count("5 memories created"), 5);
    }

    #[test]
    fn test_parse_memories_multiline() {
        let output = "Processing handbook.md...\n14 memories stored\nDone.";
        assert_eq!(parse_memories_count(output), 14);
    }

    #[test]
    fn test_parse_memories_no_match() {
        assert_eq!(parse_memories_count("no relevant output"), 0);
    }

    #[test]
    fn test_parse_memories_empty() {
        assert_eq!(parse_memories_count(""), 0);
    }
}
