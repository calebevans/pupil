use clap::Args;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

use secrecy::{ExposeSecret, SecretString};

use crate::agent_config::{
    self, AgentConfig, AuthConfig, SourceEntry,
};
use crate::container::{self, ContainerId, ContainerRuntime, RunOptions};
use crate::error::CliError;

/// Check URL-based curriculum sources for changes and re-learn any that changed.
#[derive(Args, Debug, Clone)]
pub struct SyncArgs {
    /// Agent name. Optional if pupil.yaml exists in the current directory.
    pub name: Option<String>,

    /// Skip change detection and re-fetch/re-learn all URL sources regardless
    /// of whether they changed.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Sync a single URL source instead of all sources.
    #[arg(long, value_name = "URL")]
    pub source: Option<String>,

    /// Check for changes and report what would be synced, but do not re-learn.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Output results as JSON to stdout.
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Run continuously, checking on the configured interval.
    #[arg(long, default_value_t = false)]
    pub daemon: bool,
}

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("sync config error: {0}")]
    Config(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("authentication error: {0}")]
    Auth(String),

    #[error("source not found: {0}")]
    NotFound(String),

    #[error("rate limited: {0}")]
    RateLimit(String),

    #[error("robots.txt blocked: {0}")]
    RobotsBlocked(String),

    #[error("exec error: {0}")]
    Exec(String),

    #[error("build error: {0}")]
    Build(String),

    #[error("manifest error: {0}")]
    Manifest(String),
}

impl From<SyncError> for CliError {
    fn from(e: SyncError) -> Self {
        CliError::ContainerRuntimeError {
            message: e.to_string(),
        }
    }
}

impl From<SyncError> for miette::Report {
    fn from(e: SyncError) -> Self {
        miette::miette!("{}", e)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStrategy {
    Auto,
    ConfluenceApi,
    NotionApi,
    Sitemap,
    Webhook,
}

impl Default for SyncStrategy {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug)]
pub enum ChangeResult {
    Unchanged {
        method: ChangeMethod,
    },
    Changed {
        new_hash: String,
        new_etag: Option<String>,
        new_last_modified: Option<String>,
        method: ChangeMethod,
        content_size: usize,
    },
    Disabled,
    Error(SyncError),
}

#[derive(Debug, Clone)]
pub enum ChangeMethod {
    HttpConditional304,
    ContentHashMatch,
    ContentHashDiff,
    EtagComparison,
    ConfluenceVersion,
    NotionTimestamp,
    SitemapLastmod,
    Forced,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct SyncState {
    pub last_checked: Option<String>,
    pub last_changed: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    #[serde(default)]
    pub check_count: u32,
    #[serde(default)]
    pub change_count: u32,
    #[serde(default)]
    pub consecutive_errors: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestSource {
    pub content_hash: String,
    pub memory_ids: Vec<String>,
    pub last_learned: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncState>,
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

    pub fn sync_state(&self, url: &str) -> Option<SyncState> {
        self.sources.get(url).and_then(|s| s.sync.clone())
    }

    pub fn update_source(&mut self, key: &str, hash: &str, memory_ids: &[String]) {
        let entry = self.sources.entry(key.to_string()).or_insert_with(|| ManifestSource {
            content_hash: String::new(),
            memory_ids: Vec::new(),
            last_learned: chrono::Utc::now().to_rfc3339(),
            sync: None,
        });
        entry.content_hash = hash.to_string();
        entry.memory_ids = memory_ids.to_vec();
        entry.last_learned = chrono::Utc::now().to_rfc3339();
    }

    pub fn update_source_memories(&mut self, key: &str, memory_ids: &[String]) {
        if let Some(entry) = self.sources.get_mut(key) {
            entry.memory_ids = memory_ids.to_vec();
            entry.last_learned = chrono::Utc::now().to_rfc3339();
        }
    }

    pub fn remove_source(&mut self, key: &str) {
        self.sources.remove(key);
    }

    pub fn update_sync_checked(&mut self, url: &str) {
        if let Some(entry) = self.sources.get_mut(url) {
            let sync = entry.sync.get_or_insert_with(SyncState::default);
            sync.last_checked = Some(chrono::Utc::now().to_rfc3339());
            sync.check_count += 1;
        }
    }

    pub fn update_sync_state(&mut self, url: &str, state: SyncState) {
        if let Some(entry) = self.sources.get_mut(url) {
            entry.sync = Some(state);
        } else {
            self.sources.insert(
                url.to_string(),
                ManifestSource {
                    content_hash: String::new(),
                    memory_ids: Vec::new(),
                    last_learned: chrono::Utc::now().to_rfc3339(),
                    sync: Some(state),
                },
            );
        }
    }

    pub fn update_sync_error(
        &mut self,
        url: &str,
        consecutive_errors: u32,
        error_message: &str,
    ) {
        if let Some(entry) = self.sources.get_mut(url) {
            let sync = entry.sync.get_or_insert_with(SyncState::default);
            sync.last_checked = Some(chrono::Utc::now().to_rfc3339());
            sync.check_count += 1;
            sync.consecutive_errors = consecutive_errors;
            sync.last_error = Some(error_message.to_string());
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct GlobalSyncConfig {
    pub enabled: bool,
    pub interval: String,
    pub on_change: OnChangeMode,
    pub post_test: bool,
    pub test_file: String,
    pub concurrency: usize,
    pub request_delay_ms: u64,
    pub timeout_secs: u64,
    pub user_agent: String,
    pub respect_robots_txt: bool,
}

impl Default for GlobalSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: "6h".to_string(),
            on_change: OnChangeMode::InPlace,
            post_test: false,
            test_file: "tests.yaml".to_string(),
            concurrency: 4,
            request_delay_ms: 500,
            timeout_secs: 30,
            user_agent: "PupilBot/1.0 (+https://github.com/calebevans/pupil)".to_string(),
            respect_robots_txt: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnChangeMode {
    InPlace,
    Rebuild,
    Notify,
}

impl Default for OnChangeMode {
    fn default() -> Self {
        Self::InPlace
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
#[serde(default)]
pub struct PerSourceSyncConfig {
    pub enabled: Option<bool>,
    pub interval: Option<String>,
    pub on_change: Option<OnChangeMode>,
    pub auth: Option<SyncAuthConfig>,
    pub strategy: Option<SyncStrategy>,
    pub request_delay_ms: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub webhook_secret: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncAuthConfig {
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
    Header {
        headers: HashMap<String, String>,
    },
}

pub struct EffectiveSyncConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub on_change: OnChangeMode,
    pub strategy: SyncStrategy,
    pub auth: Option<SyncAuthConfig>,
    pub request_delay_ms: u64,
    pub timeout_secs: u64,
}

impl EffectiveSyncConfig {
    pub fn resolve(
        global: &GlobalSyncConfig,
        source: &PerSourceSyncConfig,
    ) -> Result<Self, SyncError> {
        Ok(Self {
            enabled: source.enabled.unwrap_or(global.enabled),
            interval: parse_interval(
                source.interval.as_deref().unwrap_or(&global.interval),
            )?,
            on_change: source
                .on_change
                .clone()
                .unwrap_or_else(|| global.on_change.clone()),
            strategy: source.strategy.clone().unwrap_or_default(),
            auth: source.auth.clone(),
            request_delay_ms: source.request_delay_ms.unwrap_or(global.request_delay_ms),
            timeout_secs: source.timeout_secs.unwrap_or(global.timeout_secs),
        })
    }
}

fn parse_interval(s: &str) -> Result<Duration, SyncError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(SyncError::Config("interval cannot be empty".to_string()));
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let num: u64 = num_str.parse().map_err(|_| {
        SyncError::Config(format!("invalid interval number: {num_str:?}"))
    })?;

    let seconds = match suffix {
        "m" => num * 60,
        "h" => num * 3600,
        "d" => num * 86400,
        _ => {
            return Err(SyncError::Config(format!(
                "invalid interval suffix {suffix:?}. Use m (minutes), h (hours), or d (days)."
            )));
        }
    };

    let duration = Duration::from_secs(seconds);

    if duration < Duration::from_secs(300) {
        return Err(SyncError::Config(format!(
            "interval {s} is below the 5-minute minimum. \
             Use at least '5m' to prevent accidental abuse."
        )));
    }

    Ok(duration)
}

fn resolve_env_var(s: &str) -> Result<SecretString, SyncError> {
    let mut result = String::new();
    let mut remaining = s;

    while let Some(start) = remaining.find("${") {
        result.push_str(&remaining[..start]);

        let after_start = &remaining[start + 2..];
        let end = after_start.find('}').ok_or_else(|| {
            SyncError::Auth(format!("unterminated ${{}} in value: {s}"))
        })?;

        let var_name = &after_start[..end];
        let var_value = std::env::var(var_name).map_err(|_| {
            SyncError::Auth(format!("environment variable {var_name} is not set"))
        })?;

        result.push_str(&var_value);
        remaining = &after_start[end + 1..];
    }

    result.push_str(remaining);
    Ok(SecretString::from(result))
}

fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &SyncAuthConfig,
) -> reqwest::RequestBuilder {
    match auth {
        SyncAuthConfig::Bearer { token } => {
            match resolve_env_var(token) {
                Ok(resolved) => request.bearer_auth(resolved.expose_secret()),
                Err(_) => request,
            }
        }
        SyncAuthConfig::Basic { username, password } => {
            match (resolve_env_var(username), resolve_env_var(password)) {
                (Ok(user), Ok(pass)) => {
                    request.basic_auth(user.expose_secret(), Some(pass.expose_secret()))
                }
                _ => request,
            }
        }
        SyncAuthConfig::Header { headers } => {
            let mut req = request;
            for (name, value) in headers {
                if let Ok(resolved) = resolve_env_var(value) {
                    req = req.header(name.as_str(), resolved.expose_secret());
                }
            }
            req
        }
    }
}

fn apply_agent_auth(
    request: reqwest::RequestBuilder,
    auth: &AuthConfig,
) -> reqwest::RequestBuilder {
    match auth.auth_type.as_str() {
        "bearer" => {
            if let Some(ref token) = auth.token {
                match resolve_env_var(token) {
                    Ok(resolved) => request.bearer_auth(resolved.expose_secret()),
                    Err(_) => request,
                }
            } else {
                request
            }
        }
        "basic" => {
            match (auth.username.as_ref(), auth.password.as_ref()) {
                (Some(user), Some(pass)) => {
                    match (resolve_env_var(user), resolve_env_var(pass)) {
                        (Ok(u), Ok(p)) => {
                            request.basic_auth(u.expose_secret(), Some(p.expose_secret()))
                        }
                        _ => request,
                    }
                }
                _ => request,
            }
        }
        "header" => {
            if let Some(ref headers) = auth.headers {
                let mut req = request;
                for (name, value) in headers {
                    if let Ok(resolved) = resolve_env_var(value) {
                        req = req.header(name.as_str(), resolved.expose_secret());
                    }
                }
                req
            } else {
                request
            }
        }
        _ => request,
    }
}

async fn check_auto_strategy(
    client: &reqwest::Client,
    url: &str,
    sync_state: Option<&SyncState>,
    auth: Option<&SyncAuthConfig>,
    agent_auth: Option<&AuthConfig>,
) -> ChangeResult {
    let mut request = client.get(url);

    if let Some(state) = sync_state {
        if let Some(ref etag) = state.etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        if let Some(ref last_modified) = state.last_modified {
            request = request.header("If-Modified-Since", last_modified.as_str());
        }
    }

    if let Some(auth) = auth {
        request = apply_auth(request, auth);
    } else if let Some(agent_auth) = agent_auth {
        request = apply_agent_auth(request, agent_auth);
    }

    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            return ChangeResult::Error(SyncError::Network(format!("request failed: {e}")));
        }
    };

    let status = response.status();

    let new_etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let new_last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    match status.as_u16() {
        304 => ChangeResult::Unchanged {
            method: ChangeMethod::HttpConditional304,
        },
        200 => {
            let body = match response.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return ChangeResult::Error(SyncError::Network(format!(
                        "failed to read response body: {e}"
                    )));
                }
            };

            let mut hasher = Sha256::new();
            hasher.update(&body);
            let new_hash = format!("sha256:{}", hex::encode(hasher.finalize()));
            let content_size = body.len();

            let stored_hash = sync_state.and_then(|s| s.content_hash.as_deref());
            if stored_hash == Some(new_hash.as_str()) {
                ChangeResult::Unchanged {
                    method: ChangeMethod::ContentHashMatch,
                }
            } else {
                ChangeResult::Changed {
                    new_hash,
                    new_etag,
                    new_last_modified,
                    method: ChangeMethod::ContentHashDiff,
                    content_size,
                }
            }
        }
        401 | 403 => ChangeResult::Error(SyncError::Auth(format!(
            "HTTP {status}: authentication failed"
        ))),
        404 => ChangeResult::Error(SyncError::NotFound(format!(
            "HTTP 404: source URL not found"
        ))),
        429 => ChangeResult::Error(SyncError::RateLimit(format!("HTTP 429: rate limited"))),
        code => ChangeResult::Error(SyncError::Http(format!(
            "unexpected HTTP status: {code}"
        ))),
    }
}

#[derive(Debug, Clone)]
pub struct UrlSource {
    pub url: String,
    pub sync: Option<PerSourceSyncConfig>,
    pub agent_auth: Option<AuthConfig>,
    pub learning_profile: Option<String>,
    pub learning_prompt: Option<String>,
}

fn extract_url_sources(config: &AgentConfig) -> Vec<UrlSource> {
    config
        .curriculum
        .sources
        .iter()
        .filter_map(|source| match source {
            SourceEntry::Long(sc) => sc.url.as_ref().map(|url| {
                let per_source_sync = sc.sync.as_ref().map(|ss| PerSourceSyncConfig {
                    enabled: ss.enabled,
                    interval: ss.interval.clone(),
                    strategy: ss.strategy.as_deref().map(|s| match s {
                        "auto" => SyncStrategy::Auto,
                        "confluence_api" => SyncStrategy::ConfluenceApi,
                        "notion_api" => SyncStrategy::NotionApi,
                        "sitemap" => SyncStrategy::Sitemap,
                        "webhook" => SyncStrategy::Webhook,
                        _ => SyncStrategy::Auto,
                    }),
                    auth: ss.auth.as_ref().map(|a| match a.auth_type.as_str() {
                        "bearer" => SyncAuthConfig::Bearer {
                            token: a.token.clone().unwrap_or_default(),
                        },
                        "basic" => SyncAuthConfig::Basic {
                            username: a.username.clone().unwrap_or_default(),
                            password: a.password.clone().unwrap_or_default(),
                        },
                        "header" => SyncAuthConfig::Header {
                            headers: a.headers.clone().unwrap_or_default(),
                        },
                        _ => SyncAuthConfig::Bearer {
                            token: a.token.clone().unwrap_or_default(),
                        },
                    }),
                    webhook_secret: ss.webhook_secret.clone(),
                    on_change: None,
                    request_delay_ms: None,
                    timeout_secs: None,
                });
                UrlSource {
                    url: url.clone(),
                    sync: per_source_sync,
                    agent_auth: sc.sync.as_ref().and_then(|ss| ss.auth.clone()),
                    learning_profile: sc.learning_profile.clone(),
                    learning_prompt: sc.learning_prompt.clone(),
                }
            }),
            SourceEntry::Short(_) => None,
        })
        .collect()
}

fn global_sync_config_from(config: &AgentConfig) -> GlobalSyncConfig {
    match config.curriculum.sync.as_ref() {
        Some(sc) => GlobalSyncConfig {
            enabled: sc.enabled,
            interval: sc.interval.clone(),
            on_change: match sc.on_change.as_str() {
                "in_place" => OnChangeMode::InPlace,
                "rebuild" => OnChangeMode::Rebuild,
                "notify" => OnChangeMode::Notify,
                _ => OnChangeMode::InPlace,
            },
            post_test: false,
            test_file: "tests.yaml".to_string(),
            concurrency: sc.concurrency.unwrap_or(4) as usize,
            request_delay_ms: sc.request_delay_ms.unwrap_or(500),
            timeout_secs: sc.timeout_secs.unwrap_or(30),
            user_agent: sc
                .user_agent
                .clone()
                .unwrap_or_else(|| {
                    "PupilBot/1.0 (+https://github.com/calebevans/pupil)".to_string()
                }),
            respect_robots_txt: sc.respect_robots_txt,
        },
        None => GlobalSyncConfig::default(),
    }
}

#[derive(Debug)]
pub struct ReconcileResult {
    pub memories_forgotten: usize,
    pub memories_stored: usize,
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

async fn reconcile_source(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    source_url: &str,
    manifest: &mut Manifest,
) -> Result<ReconcileResult, SyncError> {
    let old_memory_count = manifest.source_memory_count(source_url).unwrap_or(0);

    if old_memory_count > 0 {
        let forget_output = runtime
            .exec(
                &ContainerId(container_id.to_string()),
                &["pupil-agent", "forget-source", source_url],
                &[],
            )
            .await
            .map_err(|e| SyncError::Exec(format!("forget failed for {source_url}: {e}")))?;

        if forget_output.exit_code != 0 {
            return Err(SyncError::Exec(format!(
                "forget exited with non-zero status for {source_url}: {}",
                forget_output.stderr
            )));
        }
    }

    let learn_output = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &["pupil-agent", "learn", "--source", source_url],
            &[],
        )
        .await
        .map_err(|e| SyncError::Exec(format!("learn failed for {source_url}: {e}")))?;

    if learn_output.exit_code != 0 {
        return Err(SyncError::Exec(format!(
            "learn exited with non-zero status for {source_url}: {}",
            learn_output.stderr
        )));
    }

    let memories_stored = parse_memories_count(&learn_output.stdout);
    let new_memory_ids = parse_memory_ids(&learn_output.stdout);

    manifest.update_source_memories(source_url, &new_memory_ids);

    Ok(ReconcileResult {
        memories_forgotten: old_memory_count,
        memories_stored,
    })
}

#[allow(dead_code)]
async fn handle_change(
    mode: &OnChangeMode,
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    source_url: &str,
    manifest: &mut Manifest,
    agent_name: &str,
    _agent_dir: &Path,
) -> Result<ReconcileResult, SyncError> {
    match mode {
        OnChangeMode::InPlace => {
            reconcile_source(runtime, container_id, source_url, manifest).await
        }
        OnChangeMode::Rebuild => {
            let output = std::process::Command::new("pupil")
                .args(["build", agent_name, "--no-confirm"])
                .current_dir(_agent_dir)
                .output()
                .map_err(|e| SyncError::Build(format!("failed to run pupil build: {e}")))?;

            if !output.status.success() {
                return Err(SyncError::Build(format!(
                    "pupil build failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            Ok(ReconcileResult {
                memories_forgotten: 0,
                memories_stored: 0,
            })
        }
        OnChangeMode::Notify => Ok(ReconcileResult {
            memories_forgotten: 0,
            memories_stored: 0,
        }),
    }
}

pub struct RobotsCache {
    entries: HashMap<String, RobotsCacheEntry>,
    #[allow(dead_code)]
    user_agent: String,
}

struct RobotsCacheEntry {
    robot: texting_robots::Robot,
    crawl_delay: Option<Duration>,
    fetched_at: std::time::Instant,
}

impl RobotsCache {
    pub fn new(config: &GlobalSyncConfig) -> Self {
        Self {
            entries: HashMap::new(),
            user_agent: config.user_agent.clone(),
        }
    }

    pub async fn check_allowed(
        &mut self,
        client: &reqwest::Client,
        url: &str,
        user_agent: &str,
    ) -> Result<(), SyncError> {
        let host = extract_host(url);
        let cache_ttl = Duration::from_secs(24 * 3600);

        let needs_refresh = match self.entries.get(&host) {
            Some(entry) => entry.fetched_at.elapsed() > cache_ttl,
            None => true,
        };

        if needs_refresh {
            let robots_url = format!("{}://{}/robots.txt", extract_scheme(url), host);
            let robots_text = match client.get(&robots_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    resp.text().await.unwrap_or_default()
                }
                _ => String::new(),
            };

            let robot = texting_robots::Robot::new(user_agent, robots_text.as_bytes())
                .unwrap_or_else(|_| texting_robots::Robot::new(user_agent, b"").unwrap());

            let crawl_delay = robot.delay.map(|d| Duration::from_secs_f64(d.into()));

            self.entries.insert(
                host.clone(),
                RobotsCacheEntry {
                    robot,
                    crawl_delay,
                    fetched_at: std::time::Instant::now(),
                },
            );
        }

        let entry = self.entries.get(&host).unwrap();

        if !entry.robot.allowed(url) {
            return Err(SyncError::RobotsBlocked(format!(
                "robots.txt disallows access to {url} for user-agent {user_agent}"
            )));
        }

        Ok(())
    }

    pub fn effective_delay(&self, url: &str, config_delay_ms: u64) -> Duration {
        let host = extract_host(url);
        let config_delay = Duration::from_millis(config_delay_ms);

        match self.entries.get(&host) {
            Some(entry) => match entry.crawl_delay {
                Some(crawl_delay) if crawl_delay > config_delay => crawl_delay,
                _ => config_delay,
            },
            None => config_delay,
        }
    }
}

pub struct RateLimiter {
    last_request: HashMap<String, std::time::Instant>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            last_request: HashMap::new(),
        }
    }

    pub async fn wait(&mut self, url: &str, delay: Duration) {
        let host = extract_host(url);

        if let Some(last) = self.last_request.get(&host) {
            let elapsed = last.elapsed();
            if elapsed < delay {
                tokio::time::sleep(delay - elapsed).await;
            }
        }

        self.last_request.insert(host, std::time::Instant::now());
    }
}

#[allow(dead_code)]
fn backoff_for_429(
    response_headers: &reqwest::header::HeaderMap,
    attempt: u32,
) -> Duration {
    if let Some(retry_after) = response_headers.get("retry-after") {
        if let Ok(s) = retry_after.to_str() {
            if let Ok(secs) = s.parse::<u64>() {
                return Duration::from_secs(secs);
            }
        }
    }

    let base = Duration::from_secs(1);
    let max = Duration::from_secs(60);
    let backoff = base.saturating_mul(2u32.saturating_pow(attempt));
    std::cmp::min(backoff, max)
}

fn extract_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default()
}

fn extract_scheme(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .map(|u| u.scheme().to_string())
        .unwrap_or_else(|| "https".to_string())
}

#[derive(Debug, serde::Deserialize)]
struct TestOutput {
    summary: TestSummary,
    #[allow(dead_code)]
    tests: Vec<TestResultEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TestSummary {
    total: usize,
    passed: usize,
    failed: usize,
}

#[derive(Debug, serde::Deserialize)]
struct TestResultEntry {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    passed: bool,
    #[allow(dead_code)]
    latency_ms: u64,
    #[allow(dead_code)]
    assertions: Vec<AssertionResultEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct AssertionResultEntry {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    assertion_type: String,
    #[allow(dead_code)]
    passed: bool,
    #[allow(dead_code)]
    detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostTestResult {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
}

async fn run_post_test(
    runtime: &dyn ContainerRuntime,
    container_id: &str,
    test_file: &str,
) -> Result<PostTestResult, SyncError> {
    eprintln!("[sync] Running post-sync tests from {test_file}...");

    let output = runtime
        .exec(
            &ContainerId(container_id.to_string()),
            &["pupil-agent", "test", "--file", test_file, "--json"],
            &[],
        )
        .await
        .map_err(|e| SyncError::Exec(format!("post-test failed: {e}")))?;

    let test_output: TestOutput = serde_json::from_str(&output.stdout)
        .map_err(|e| SyncError::Exec(format!("failed to parse test output: {e}")))?;

    let result = PostTestResult {
        total: test_output.summary.total,
        passed: test_output.summary.passed,
        failed: test_output.summary.failed,
    };

    if result.failed > 0 {
        eprintln!(
            "[sync] Post-sync tests: {}/{} passed ({} failed)",
            result.passed, result.total, result.failed
        );
    } else {
        eprintln!(
            "[sync] Post-sync tests: all {} passed",
            result.total
        );
    }

    Ok(result)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncReport {
    pub agent: String,
    pub timestamp: String,
    pub sources: Vec<SourceSyncResult>,
    pub summary: SyncSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_test: Option<PostTestResult>,
}

impl SyncReport {
    pub fn empty() -> Self {
        Self {
            agent: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            sources: Vec::new(),
            summary: SyncSummary::default(),
            post_test: None,
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncSummary {
    pub total: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub errors: usize,
    pub memories_forgotten: usize,
    pub memories_stored: usize,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SourceSyncResult {
    pub url: String,
    pub status: SyncSourceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memories_forgotten: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memories_stored: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_size: Option<usize>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncSourceStatus {
    #[default]
    Unchanged,
    Synced,
    WouldSync,
    Disabled,
    RobotsBlocked(String),
    Error(String),
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn print_sync_report(report: &SyncReport) {
    println!();
    for source in &report.sources {
        let status_str = match &source.status {
            SyncSourceStatus::Unchanged => "304 Not Modified (skipped)".to_string(),
            SyncSourceStatus::Synced => {
                let forgotten = source.memories_forgotten.unwrap_or(0);
                let stored = source.memories_stored.unwrap_or(0);
                let duration = source.duration_secs.unwrap_or(0.0);
                format!("{forgotten} memories forgotten, {stored} memories stored ({duration:.1}s)")
            }
            SyncSourceStatus::WouldSync => {
                let size = source
                    .content_size
                    .map(|s| format!(", ~{} changed", format_bytes(s)))
                    .unwrap_or_default();
                format!("would sync{size}")
            }
            SyncSourceStatus::Disabled => "sync disabled (skipped)".to_string(),
            SyncSourceStatus::RobotsBlocked(msg) => format!("blocked by robots.txt: {msg}"),
            SyncSourceStatus::Error(msg) => format!("ERROR: {msg}"),
        };

        let url_display = if source.url.len() > 50 {
            format!("{}...", &source.url[..47])
        } else {
            source.url.clone()
        };

        println!("  {url_display:<52} {status_str}");
    }

    println!();
    println!("Sync complete.");
    println!("  Checked: {} sources", report.summary.total);
    println!("  Changed: {} sources", report.summary.changed);
    println!(
        "  Skipped: {} sources ({} unchanged, {} disabled)",
        report.summary.skipped + report.summary.unchanged,
        report.summary.unchanged,
        report.summary.skipped
    );
    if report.summary.changed > 0 {
        println!(
            "  Memories: -{} old, +{} new",
            report.summary.memories_forgotten, report.summary.memories_stored
        );
    }
    if report.summary.errors > 0 {
        println!("  Errors: {}", report.summary.errors);
    }

    if let Some(ref post_test) = report.post_test {
        println!();
        if post_test.failed > 0 {
            println!(
                "  Post-sync tests: {}/{} passed ({} failed)",
                post_test.passed, post_test.total, post_test.failed
            );
        } else {
            println!(
                "  Post-sync tests: all {} passed",
                post_test.total
            );
        }
    }

    println!();
}

async fn load_manifest(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
) -> Result<Manifest, SyncError> {
    let output = runtime
        .exec(container_id, &["cat", "/data/.pupil-manifest.json"], &[])
        .await;

    match output {
        Ok(out) => serde_json::from_str(&out.stdout)
            .map_err(|e| SyncError::Manifest(format!("failed to parse manifest: {e}"))),
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

async fn save_manifest(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
    manifest: &Manifest,
) -> Result<(), SyncError> {
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| SyncError::Manifest(format!("failed to serialize manifest: {e}")))?;

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
        .map_err(|e| SyncError::Manifest(format!("failed to save manifest: {e}")))?;

    Ok(())
}

async fn ensure_sync_container(
    runtime: &dyn ContainerRuntime,
    agent_name: &str,
    _config: &AgentConfig,
) -> Result<(ContainerId, Manifest), SyncError> {
    let running_name = format!("pupil-{agent_name}");
    let container_id_candidate = ContainerId(running_name.clone());

    let inspect = runtime
        .exec(&container_id_candidate, &["true"], &[])
        .await;

    let container_id = match inspect {
        Ok(_) => ContainerId(running_name),
        Err(_) => {
            let image_name = format!("pupil-agent-{}:latest", agent_name);
            let temp_name = format!("pupil-sync-{agent_name}");

            let _ = runtime.rm(&ContainerId(temp_name.clone()), true).await;

            runtime
                .run(
                    &image_name,
                    &RunOptions {
                        name: Some(temp_name.clone()),
                        entrypoint: Some("sleep".to_string()),
                        command: vec!["infinity".to_string()],
                        detach: true,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| SyncError::Exec(format!("failed to start sync container: {e}")))?;

            ContainerId(temp_name)
        }
    };

    let manifest = load_manifest(runtime, &container_id).await?;

    Ok((container_id, manifest))
}

async fn run_sync_once(
    config: &AgentConfig,
    agent_name: &str,
    _agent_dir: &Path,
    sources: &[&UrlSource],
    force: bool,
    dry_run: bool,
) -> Result<SyncReport, SyncError> {
    let global_sync = global_sync_config_from(config);

    let client = reqwest::Client::builder()
        .user_agent(&global_sync.user_agent)
        .timeout(Duration::from_secs(global_sync.timeout_secs))
        .build()
        .map_err(|e| SyncError::Network(format!("failed to build HTTP client: {e}")))?;

    let runtime = container::detect().map_err(|e| SyncError::Exec(e.to_string()))?;

    let (container_id, mut manifest) =
        ensure_sync_container(runtime.as_ref(), agent_name, config).await?;

    let mut robots_cache = RobotsCache::new(&global_sync);

    let mut host_last_request: HashMap<String, std::time::Instant> = HashMap::new();

    let mut report = SyncReport {
        agent: agent_name.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        sources: Vec::new(),
        summary: SyncSummary::default(),
        post_test: None,
    };

    if !dry_run {
        println!("Checking {} URL source(s) for changes...", sources.len());
    }

    for source in sources {
        let effective = EffectiveSyncConfig::resolve(
            &global_sync,
            source.sync.as_ref().unwrap_or(&PerSourceSyncConfig::default()),
        )?;

        if !effective.enabled {
            report.sources.push(SourceSyncResult {
                url: source.url.clone(),
                status: SyncSourceStatus::Disabled,
                ..Default::default()
            });
            report.summary.skipped += 1;
            continue;
        }

        if global_sync.respect_robots_txt && effective.auth.is_none() {
            if let Err(e) = robots_cache
                .check_allowed(&client, &source.url, &global_sync.user_agent)
                .await
            {
                report.sources.push(SourceSyncResult {
                    url: source.url.clone(),
                    status: SyncSourceStatus::RobotsBlocked(e.to_string()),
                    ..Default::default()
                });
                report.summary.skipped += 1;
                continue;
            }
        }

        let host = extract_host(&source.url);
        if let Some(last) = host_last_request.get(&host) {
            let delay = Duration::from_millis(effective.request_delay_ms);
            let elapsed = last.elapsed();
            if elapsed < delay {
                tokio::time::sleep(delay - elapsed).await;
            }
        }

        let sync_state = manifest.sync_state(&source.url);
        let change_result = if force {
            ChangeResult::Changed {
                new_hash: String::new(),
                new_etag: None,
                new_last_modified: None,
                method: ChangeMethod::Forced,
                content_size: 0,
            }
        } else {
            match effective.strategy {
                SyncStrategy::Auto => {
                    check_auto_strategy(
                        &client,
                        &source.url,
                        sync_state.as_ref(),
                        effective.auth.as_ref(),
                        source.agent_auth.as_ref(),
                    )
                    .await
                }
                SyncStrategy::ConfluenceApi => ChangeResult::Error(SyncError::Config(
                    "confluence_api strategy is not yet implemented".to_string(),
                )),
                SyncStrategy::NotionApi => ChangeResult::Error(SyncError::Config(
                    "notion_api strategy is not yet implemented".to_string(),
                )),
                SyncStrategy::Sitemap => ChangeResult::Error(SyncError::Config(
                    "sitemap strategy is not yet implemented".to_string(),
                )),
                SyncStrategy::Webhook => ChangeResult::Error(SyncError::Config(
                    "webhook strategy is not yet implemented".to_string(),
                )),
            }
        };

        host_last_request.insert(host, std::time::Instant::now());

        match change_result {
            ChangeResult::Unchanged { method } => {
                report.sources.push(SourceSyncResult {
                    url: source.url.clone(),
                    status: SyncSourceStatus::Unchanged,
                    method: Some(format!("{method:?}")),
                    ..Default::default()
                });
                report.summary.unchanged += 1;
                manifest.update_sync_checked(&source.url);
            }
            ChangeResult::Changed {
                new_hash,
                new_etag,
                new_last_modified,
                method,
                content_size,
            } => {
                if dry_run {
                    report.sources.push(SourceSyncResult {
                        url: source.url.clone(),
                        status: SyncSourceStatus::WouldSync,
                        method: Some(format!("{method:?}")),
                        content_size: Some(content_size),
                        ..Default::default()
                    });
                    report.summary.changed += 1;
                } else {
                    let start = std::time::Instant::now();
                    let reconcile_result = reconcile_source(
                        runtime.as_ref(),
                        &container_id.0,
                        &source.url,
                        &mut manifest,
                    )
                    .await;

                    match reconcile_result {
                        Ok(recon) => {
                            manifest.update_sync_state(
                                &source.url,
                                SyncState {
                                    last_checked: Some(chrono::Utc::now().to_rfc3339()),
                                    last_changed: Some(chrono::Utc::now().to_rfc3339()),
                                    etag: new_etag,
                                    last_modified: new_last_modified,
                                    content_hash: Some(new_hash),
                                    check_count: sync_state
                                        .as_ref()
                                        .map(|s| s.check_count + 1)
                                        .unwrap_or(1),
                                    change_count: sync_state
                                        .as_ref()
                                        .map(|s| s.change_count + 1)
                                        .unwrap_or(1),
                                    consecutive_errors: 0,
                                    last_error: None,
                                },
                            );

                            report.sources.push(SourceSyncResult {
                                url: source.url.clone(),
                                status: SyncSourceStatus::Synced,
                                method: Some(format!("{method:?}")),
                                memories_forgotten: Some(recon.memories_forgotten),
                                memories_stored: Some(recon.memories_stored),
                                duration_secs: Some(start.elapsed().as_secs_f64()),
                                ..Default::default()
                            });
                            report.summary.changed += 1;
                            report.summary.memories_forgotten += recon.memories_forgotten;
                            report.summary.memories_stored += recon.memories_stored;
                        }
                        Err(e) => {
                            let consecutive = sync_state
                                .as_ref()
                                .map(|s| s.consecutive_errors + 1)
                                .unwrap_or(1);
                            manifest.update_sync_error(
                                &source.url,
                                consecutive,
                                &e.to_string(),
                            );

                            report.sources.push(SourceSyncResult {
                                url: source.url.clone(),
                                status: SyncSourceStatus::Error(e.to_string()),
                                ..Default::default()
                            });
                            report.summary.errors += 1;
                        }
                    }
                }
            }
            ChangeResult::Disabled => {
                report.sources.push(SourceSyncResult {
                    url: source.url.clone(),
                    status: SyncSourceStatus::Disabled,
                    ..Default::default()
                });
                report.summary.skipped += 1;
            }
            ChangeResult::Error(e) => {
                let consecutive = sync_state
                    .as_ref()
                    .map(|s| s.consecutive_errors + 1)
                    .unwrap_or(1);
                manifest.update_sync_error(&source.url, consecutive, &e.to_string());

                report.sources.push(SourceSyncResult {
                    url: source.url.clone(),
                    status: SyncSourceStatus::Error(e.to_string()),
                    ..Default::default()
                });
                report.summary.errors += 1;
            }
        }
    }

    save_manifest(runtime.as_ref(), &container_id, &manifest).await?;

    report.summary.total = sources.len();

    if !dry_run && report.summary.changed > 0 && global_sync.post_test {
        report.post_test = Some(
            run_post_test(runtime.as_ref(), &container_id.0, &global_sync.test_file).await?,
        );
    }

    Ok(report)
}

fn compute_effective_interval(base: Duration, consecutive_errors: u32) -> Duration {
    if consecutive_errors == 0 {
        return base;
    }

    let max_interval = Duration::from_secs(24 * 3600);
    let multiplier = 2u64.saturating_pow(consecutive_errors);
    let backoff = base.saturating_mul(multiplier as u32);

    std::cmp::min(backoff, max_interval)
}

fn compute_shortest_interval(
    sources: &[UrlSource],
    global: &GlobalSyncConfig,
) -> Result<Duration, SyncError> {
    let mut shortest = parse_interval(&global.interval)?;

    for source in sources {
        if let Some(ref sync_config) = source.sync {
            let effective = EffectiveSyncConfig::resolve(global, sync_config)?;
            if effective.enabled && effective.interval < shortest {
                shortest = effective.interval;
            }
        }
    }

    Ok(shortest)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86400 && secs % 86400 == 0 {
        format!("{}d", secs / 86400)
    } else if secs >= 3600 && secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

async fn run_daemon(
    config: AgentConfig,
    agent_name: String,
    agent_dir: PathBuf,
    url_sources: Vec<UrlSource>,
) -> Result<(), miette::Report> {
    let global_sync = global_sync_config_from(&config);

    let wake_interval = compute_shortest_interval(&url_sources, &global_sync)?;

    eprintln!(
        "[sync] Daemon mode: checking every {}",
        format_duration(wake_interval)
    );
    eprintln!("[sync] Press Ctrl+C to stop.");

    let mut ticker = tokio::time::interval(wake_interval);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let runtime = container::detect().map_err(|e| miette::miette!("Container runtime not found: {e}"))?;
                let (_container_id, manifest) =
                    ensure_sync_container(runtime.as_ref(), &agent_name, &config)
                        .await
                        .map_err(|e| miette::miette!("{e}"))?;

                let mut sources_to_check = Vec::new();
                for source in &url_sources {
                    let effective = EffectiveSyncConfig::resolve(
                        &global_sync,
                        source.sync.as_ref().unwrap_or(&PerSourceSyncConfig::default()),
                    ).map_err(|e| miette::miette!("{e}"))?;

                    if !effective.enabled {
                        continue;
                    }

                    let sync_state = manifest.sync_state(&source.url);
                    let should_check = match sync_state {
                        Some(ref state) => {
                            let last_checked = state
                                .last_checked
                                .as_deref()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .map(|dt| dt.with_timezone(&chrono::Utc));

                            match last_checked {
                                Some(lc) => {
                                    let elapsed = chrono::Utc::now() - lc;
                                    let effective_interval = compute_effective_interval(
                                        effective.interval,
                                        state.consecutive_errors,
                                    );
                                    elapsed >= chrono::Duration::from_std(effective_interval)
                                        .unwrap_or(chrono::Duration::MAX)
                                }
                                None => true,
                            }
                        }
                        None => true,
                    };

                    if should_check {
                        sources_to_check.push(source);
                    }
                }

                if !sources_to_check.is_empty() {
                    eprintln!(
                        "[sync] Checking {} source(s)...",
                        sources_to_check.len()
                    );

                    let refs: Vec<&UrlSource> = sources_to_check.iter().copied().collect();
                    match run_sync_once(
                        &config,
                        &agent_name,
                        &agent_dir,
                        &refs,
                        false,
                        false,
                    )
                    .await
                    {
                        Ok(report) => {
                            if report.summary.changed > 0 {
                                eprintln!(
                                    "[sync] {} source(s) changed, {} memories updated",
                                    report.summary.changed,
                                    report.summary.memories_stored,
                                );
                            } else {
                                eprintln!("[sync] No changes detected");
                            }
                        }
                        Err(e) => {
                            eprintln!("[sync] Error during sync cycle: {e}");
                        }
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                eprintln!("[sync] Daemon shutting down.");
                break;
            }
        }
    }

    Ok(())
}

pub async fn execute(args: SyncArgs) -> Result<(), CliError> {
    if args.force && args.dry_run {
        return Err(CliError::ConfigInvalid {
            message: "--force and --dry-run cannot be used together. \
                      Use --force to re-learn everything, or --dry-run to preview changes."
                .to_string(),
        });
    }
    if args.daemon && args.dry_run {
        return Err(CliError::ConfigInvalid {
            message: "--daemon and --dry-run cannot be used together. \
                      Daemon mode performs actual syncs on each interval."
                .to_string(),
        });
    }

    let (agent_dir, config) = agent_config::resolve_agent(args.name.as_deref())?;
    let agent_name = config.name.clone();

    let url_sources = extract_url_sources(&config);
    if url_sources.is_empty() {
        if args.json {
            println!(
                "{}",
                serde_json::to_string(&SyncReport::empty()).unwrap()
            );
        } else {
            println!("No URL sources configured in pupil.yaml. Nothing to sync.");
        }
        return Ok(());
    }

    let sources_to_sync: Vec<&UrlSource> = if let Some(ref source_url) = args.source {
        let matching: Vec<_> = url_sources
            .iter()
            .filter(|s| s.url == *source_url)
            .collect();
        if matching.is_empty() {
            return Err(CliError::ConfigInvalid {
                message: format!(
                    "{source_url:?} is not a configured URL source in pupil.yaml. \
                     Run `pupil sync` without --source to check all URL sources."
                ),
            });
        }
        matching
    } else {
        url_sources.iter().collect()
    };

    if args.daemon {
        return run_daemon(config, agent_name, agent_dir, url_sources)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: e.to_string(),
            });
    }

    let report = run_sync_once(
        &config,
        &agent_name,
        &agent_dir,
        &sources_to_sync,
        args.force,
        args.dry_run,
    )
    .await
    .map_err(|e| CliError::ContainerRuntimeError {
        message: e.to_string(),
    })?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        print_sync_report(&report);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minutes() {
        assert_eq!(parse_interval("30m").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn test_parse_hours() {
        assert_eq!(parse_interval("6h").unwrap(), Duration::from_secs(21600));
    }

    #[test]
    fn test_parse_days() {
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86400));
    }

    #[test]
    fn test_parse_seven_days() {
        assert_eq!(parse_interval("7d").unwrap(), Duration::from_secs(604800));
    }

    #[test]
    fn test_parse_minimum() {
        assert!(parse_interval("5m").is_ok());
    }

    #[test]
    fn test_parse_below_minimum() {
        assert!(parse_interval("1m").is_err());
        assert!(parse_interval("4m").is_err());
    }

    #[test]
    fn test_parse_invalid_suffix() {
        assert!(parse_interval("6x").is_err());
    }

    #[test]
    fn test_parse_invalid_number() {
        assert!(parse_interval("abch").is_err());
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_interval("").is_err());
    }

    #[test]
    fn test_no_errors() {
        let base = Duration::from_secs(3600);
        assert_eq!(compute_effective_interval(base, 0), base);
    }

    #[test]
    fn test_one_error_doubles() {
        let base = Duration::from_secs(3600);
        let result = compute_effective_interval(base, 1);
        assert_eq!(result, Duration::from_secs(7200));
    }

    #[test]
    fn test_two_errors_quadruples() {
        let base = Duration::from_secs(3600);
        let result = compute_effective_interval(base, 2);
        assert_eq!(result, Duration::from_secs(14400));
    }

    #[test]
    fn test_capped_at_24h() {
        let base = Duration::from_secs(3600);
        let result = compute_effective_interval(base, 10);
        assert_eq!(result, Duration::from_secs(86400));
    }

    #[test]
    fn test_short_base_with_errors() {
        let base = Duration::from_secs(1800);
        let result = compute_effective_interval(base, 3);
        assert_eq!(result, Duration::from_secs(14400));
    }

    #[test]
    fn test_resolve_no_vars() {
        let result = resolve_env_var("plain-text").unwrap();
        assert_eq!(result.expose_secret(), "plain-text");
    }

    #[test]
    fn test_resolve_single_var() {
        unsafe { std::env::set_var("TEST_TOKEN_123", "secret-value") };
        let result = resolve_env_var("${TEST_TOKEN_123}").unwrap();
        assert_eq!(result.expose_secret(), "secret-value");
        unsafe { std::env::remove_var("TEST_TOKEN_123") };
    }

    #[test]
    fn test_resolve_var_with_prefix() {
        unsafe { std::env::set_var("TEST_TOKEN_456", "mytoken") };
        let result = resolve_env_var("Bearer ${TEST_TOKEN_456}").unwrap();
        assert_eq!(result.expose_secret(), "Bearer mytoken");
        unsafe { std::env::remove_var("TEST_TOKEN_456") };
    }

    #[test]
    fn test_resolve_missing_var() {
        let result = resolve_env_var("${DEFINITELY_NOT_SET_XYZZY}");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_unterminated() {
        let result = resolve_env_var("${UNTERMINATED");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_multiple_vars() {
        unsafe { std::env::set_var("TEST_USER_789", "alice") };
        unsafe { std::env::set_var("TEST_PASS_789", "hunter2") };
        let result = resolve_env_var("${TEST_USER_789}:${TEST_PASS_789}").unwrap();
        assert_eq!(result.expose_secret(), "alice:hunter2");
        unsafe { std::env::remove_var("TEST_USER_789") };
        unsafe { std::env::remove_var("TEST_PASS_789") };
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://wiki.internal/eng-handbook"),
            "wiki.internal"
        );
    }

    #[test]
    fn test_extract_host_with_port() {
        assert_eq!(extract_host("http://localhost:8080/page"), "localhost");
    }

    #[test]
    fn test_extract_scheme_https() {
        assert_eq!(extract_scheme("https://example.com/path"), "https");
    }

    #[test]
    fn test_extract_scheme_http() {
        assert_eq!(extract_scheme("http://example.com/path"), "http");
    }

    #[test]
    fn test_format_minutes() {
        assert_eq!(format_duration(Duration::from_secs(1800)), "30m");
    }

    #[test]
    fn test_format_hours() {
        assert_eq!(format_duration(Duration::from_secs(21600)), "6h");
    }

    #[test]
    fn test_format_days() {
        assert_eq!(format_duration(Duration::from_secs(86400)), "1d");
    }

    #[test]
    fn test_format_seconds_fallback() {
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn test_format_non_even_hours() {
        assert_eq!(format_duration(Duration::from_secs(5400)), "90m");
    }
}
