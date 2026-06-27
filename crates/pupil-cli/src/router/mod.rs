pub mod config;
pub mod discovery;
pub mod health;
pub mod proxy;
pub mod strategies;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::routing::{get, post};
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use config::{RoutingStrategyConfig, RouterConfig, RouterConfigError};
use health::{AgentEntry, AgentRegistry};
use proxy::RouterState;
use strategies::{
    EmbeddingClient, EmbeddingIndex, HybridRouter, KeywordIndex, LlmClassifier,
};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("Failed to load router config from {path}")]
    ConfigLoad {
        path: PathBuf,
        #[source]
        source: RouterConfigError,
    },

    #[error("No healthy agents available")]
    NoHealthyAgents,

    #[error("Embedding provider error: {0}")]
    EmbeddingError(String),

    #[error("LLM classifier error: {0}")]
    ClassifierError(String),

    #[error("Proxy error: {0}")]
    ProxyError(String),

    #[error("Health check failed for agent '{agent}': {reason}")]
    HealthCheckFailed { agent: String, reason: String },

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("HTTP server error: {0}")]
    ServerError(String),

    #[error("Failed to parse LLM classifier response: {0}")]
    ClassifierParseError(String),
}

// ---------------------------------------------------------------------------
// Routing decision types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingDecision {
    pub agent: String,
    pub confidence: f64,
    pub strategy_tier: StrategyTier,
    pub reasoning: Option<String>,
    pub alternatives: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyTier {
    ExclusiveTopic,
    Keyword,
    Embedding,
    LlmClassifier,
    Fallback,
}

impl std::fmt::Display for StrategyTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExclusiveTopic => write!(f, "exclusive_topic"),
            Self::Keyword => write!(f, "keyword"),
            Self::Embedding => write!(f, "embedding"),
            Self::LlmClassifier => write!(f, "llm_classifier"),
            Self::Fallback => write!(f, "fallback"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FallbackBehavior {
    Agent(String),
    Error,
    Ask,
}

impl FallbackBehavior {
    pub fn from_config(value: &str) -> Self {
        match value {
            "error" => Self::Error,
            "ask" => Self::Ask,
            agent_name => Self::Agent(agent_name.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Session affinity
// ---------------------------------------------------------------------------

struct AffinityEntry {
    agent_name: String,
    last_accessed: Instant,
}

pub struct SessionAffinityMap {
    map: DashMap<String, AffinityEntry>,
    ttl: Duration,
}

impl SessionAffinityMap {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            map: DashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn get(&self, session_id: &str) -> Option<String> {
        let mut entry = self.map.get_mut(session_id)?;

        if entry.last_accessed.elapsed() > self.ttl {
            drop(entry);
            self.map.remove(session_id);
            return None;
        }

        entry.last_accessed = Instant::now();
        Some(entry.agent_name.clone())
    }

    pub fn set(&self, session_id: &str, agent_name: &str) {
        self.map.insert(
            session_id.to_string(),
            AffinityEntry {
                agent_name: agent_name.to_string(),
                last_accessed: Instant::now(),
            },
        );
    }

    pub fn remove(&self, session_id: &str) {
        self.map.remove(session_id);
    }

    pub fn evict_expired(&self) {
        self.map
            .retain(|_, entry| entry.last_accessed.elapsed() <= self.ttl);
    }

    pub fn spawn_eviction_loop(
        self: &Arc<Self>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let map = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = ticker.tick() => map.evict_expired(),
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

pub fn install_prometheus() -> metrics_exporter_prometheus::PrometheusHandle {
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    builder
        .install_recorder()
        .expect("Failed to install Prometheus recorder")
}

// ---------------------------------------------------------------------------
// Routing engine
// ---------------------------------------------------------------------------

pub struct RoutingEngine {
    pub registry: Arc<AgentRegistry>,
    strategy: RoutingStrategyConfig,
    hybrid_config: config::HybridConfig,
    confidence_threshold: f64,
    fallback: FallbackBehavior,
    keyword_index: KeywordIndex,
    embedding_index: EmbeddingIndex,
    embedding_client: Option<EmbeddingClient>,
    classifier: Option<LlmClassifier>,
}

impl RoutingEngine {
    pub async fn new(config: &RouterConfig) -> Result<Self, RouterError> {
        let registry = Arc::new(AgentRegistry::new(
            &config.agents,
            config.router.health_check.clone(),
        ));

        let keyword_index = KeywordIndex::build(registry.all_agents());

        let needs_embedding = matches!(
            config.router.strategy,
            RoutingStrategyConfig::Embedding | RoutingStrategyConfig::Hybrid
        );

        let (embedding_client, embedding_index) = if needs_embedding {
            let emb_config = config.router.embedding.as_ref().ok_or_else(|| {
                RouterError::EmbeddingError(
                    "Embedding config required for this strategy".to_string(),
                )
            })?;

            let client = EmbeddingClient::new(emb_config)
                .map_err(|e| RouterError::EmbeddingError(e.to_string()))?;

            match EmbeddingIndex::build(registry.all_agents(), &client).await {
                Ok(index) => (Some(client), index),
                Err(e) => {
                    tracing::warn!(
                        "Embedding provider unavailable: {e}. \
                         Falling back to keyword-only routing until embeddings can be computed."
                    );
                    (None, EmbeddingIndex::empty())
                }
            }
        } else {
            (None, EmbeddingIndex::empty())
        };

        let needs_classifier = matches!(
            config.router.strategy,
            RoutingStrategyConfig::Llm | RoutingStrategyConfig::Hybrid
        );

        let classifier = if needs_classifier {
            let model = config
                .router
                .classifier
                .as_ref()
                .map(|c| c.model.clone())
                .unwrap_or_else(|| config.router.hybrid.classifier_model.clone());

            match LlmClassifier::new(&model) {
                Ok(cls) => Some(cls),
                Err(e) => {
                    tracing::warn!("LLM classifier unavailable: {e}. Skipping LLM tier.");
                    None
                }
            }
        } else {
            None
        };

        let fallback = FallbackBehavior::from_config(&config.router.fallback);

        Ok(Self {
            registry,
            strategy: config.router.strategy.clone(),
            hybrid_config: config.router.hybrid.clone(),
            confidence_threshold: config.router.confidence_threshold,
            fallback,
            keyword_index,
            embedding_index,
            embedding_client,
            classifier,
        })
    }

    pub async fn route(&self, query: &str) -> Result<RoutingDecision, RouterError> {
        if let Some(decision) = self.check_exclusive_topics(query) {
            return Ok(decision);
        }

        let decision = match &self.strategy {
            RoutingStrategyConfig::Keyword => self.keyword_route(query),
            RoutingStrategyConfig::Embedding => self.embedding_route(query).await,
            RoutingStrategyConfig::Llm => self.llm_route(query).await,
            RoutingStrategyConfig::Hybrid => self.hybrid_route(query).await,
        }?;

        if decision.confidence < self.confidence_threshold
            && decision.strategy_tier != StrategyTier::Fallback
        {
            return Ok(self.fallback_decision(query));
        }

        Ok(decision)
    }

    fn check_exclusive_topics(&self, query: &str) -> Option<RoutingDecision> {
        let query_lower = query.to_lowercase();
        let healthy = self.registry.healthy_agents();

        let mut best: Option<&Arc<AgentEntry>> = None;

        for agent in &healthy {
            for topic in &agent.exclusive_topics {
                if query_lower.contains(topic.as_str()) {
                    match best {
                        None => best = Some(agent),
                        Some(current) if agent.priority > current.priority => {
                            best = Some(agent);
                        }
                        _ => {}
                    }
                }
            }
        }

        best.map(|agent| RoutingDecision {
            agent: agent.name.clone(),
            confidence: 1.0,
            strategy_tier: StrategyTier::ExclusiveTopic,
            reasoning: Some("Matched exclusive topic".to_string()),
            alternatives: vec![],
        })
    }

    fn keyword_route(&self, query: &str) -> Result<RoutingDecision, RouterError> {
        strategies::keyword_match(query, &self.keyword_index, &self.registry)
    }

    async fn embedding_route(&self, query: &str) -> Result<RoutingDecision, RouterError> {
        let client = self.embedding_client.as_ref().ok_or_else(|| {
            RouterError::EmbeddingError("No embedding client configured".to_string())
        })?;

        strategies::embedding_similarity(query, &self.embedding_index, client, &self.registry).await
    }

    async fn llm_route(&self, query: &str) -> Result<RoutingDecision, RouterError> {
        let cls = self.classifier.as_ref().ok_or_else(|| {
            RouterError::ClassifierError("No classifier configured".to_string())
        })?;

        strategies::llm_classify(query, cls, &self.registry).await
    }

    async fn hybrid_route(&self, query: &str) -> Result<RoutingDecision, RouterError> {
        let ctx = HybridRouter {
            keyword_index: &self.keyword_index,
            embedding_index: &self.embedding_index,
            embedding_client: self.embedding_client.as_ref(),
            classifier: self.classifier.as_ref(),
            registry: &self.registry,
            keyword_threshold: self.hybrid_config.keyword_threshold,
            embedding_threshold: self.hybrid_config.embedding_threshold,
            llm_threshold: self.hybrid_config.llm_threshold,
        };
        strategies::hybrid_route(&ctx, query).await
    }

    fn fallback_decision(&self, _query: &str) -> RoutingDecision {
        let healthy = self.registry.healthy_agents();

        match &self.fallback {
            FallbackBehavior::Agent(name) => RoutingDecision {
                agent: name.clone(),
                confidence: 0.0,
                strategy_tier: StrategyTier::Fallback,
                reasoning: Some(
                    "No agent matched above threshold, using fallback agent".to_string(),
                ),
                alternatives: healthy
                    .iter()
                    .filter(|a| &a.name != name)
                    .map(|a| (a.name.clone(), 0.0))
                    .collect(),
            },
            FallbackBehavior::Error | FallbackBehavior::Ask => RoutingDecision {
                agent: String::new(),
                confidence: 0.0,
                strategy_tier: StrategyTier::Fallback,
                reasoning: Some("No agent matched above confidence threshold".to_string()),
                alternatives: healthy
                    .iter()
                    .map(|a| (a.name.clone(), 0.0))
                    .collect(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP server builder
// ---------------------------------------------------------------------------

pub fn build_router(
    state: Arc<RouterState>,
    prom_handle: metrics_exporter_prometheus::PrometheusHandle,
) -> axum::Router {
    axum::Router::new()
        .route("/v1/chat/completions", post(proxy::handle_chat_completion))
        .route("/v1/agents", get(proxy::handle_list_agents))
        .route("/v1/agents/{name}", get(proxy::handle_get_agent))
        .route("/health", get(proxy::handle_health))
        .route(
            "/metrics",
            get(move || {
                let h = prom_handle.clone();
                async move { h.render() }
            }),
        )
        .with_state(state)
}

pub async fn start_server(
    config_path: &std::path::Path,
    port_override: Option<u16>,
) -> miette::Result<()> {
    let config =
        config::load_router_config(config_path).map_err(|e| miette::miette!("{e}"))?;

    let listen = if let Some(port) = port_override {
        format!("0.0.0.0:{port}")
    } else {
        config.router.listen.clone()
    };

    tracing::info!(config = %config_path.display(), "Loading router config");
    tracing::info!(strategy = ?config.router.strategy, "Router strategy");

    let engine = RoutingEngine::new(&config)
        .await
        .map_err(|e| miette::miette!("{e}"))?;

    let engine = Arc::new(engine);

    tracing::info!("Health checking agents...");
    engine.registry.check_health_all().await;

    for agent in engine.registry.all_agents() {
        let status = if agent.is_healthy() {
            "healthy"
        } else {
            "unhealthy"
        };
        tracing::info!(
            name = %agent.name,
            url = %agent.url,
            status = status,
            "Agent status"
        );
    }

    let prom_handle = install_prometheus();

    let affinity = Arc::new(SessionAffinityMap::new(config.router.session_ttl_secs));

    let cancel = CancellationToken::new();

    let _health_handle = engine.registry.spawn_health_check_loop(cancel.clone());
    let _evict_handle = affinity.spawn_eviction_loop(cancel.clone());

    let state = Arc::new(RouterState {
        engine,
        affinity,
        http_client: reqwest::Client::new(),
    });

    let app = build_router(state, prom_handle);

    tracing::info!(listen = %listen, "Router listening");

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| miette::miette!("Failed to bind {listen}: {e}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Router shutting down...");
            cancel.cancel();
        })
        .await
        .map_err(|e| miette::miette!("Server error: {e}"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Session affinity tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod session_tests {
    use super::SessionAffinityMap;

    #[test]
    fn set_and_get() {
        let map = SessionAffinityMap::new(3600);
        map.set("session-1", "payments");
        assert_eq!(map.get("session-1"), Some("payments".to_string()));
    }

    #[test]
    fn get_nonexistent() {
        let map = SessionAffinityMap::new(3600);
        assert_eq!(map.get("session-999"), None);
    }

    #[test]
    fn remove() {
        let map = SessionAffinityMap::new(3600);
        map.set("session-1", "payments");
        map.remove("session-1");
        assert_eq!(map.get("session-1"), None);
    }

    #[test]
    fn ttl_expiry() {
        let map = SessionAffinityMap::new(0);
        map.set("session-1", "payments");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(map.get("session-1"), None);
    }

    #[test]
    fn evict_expired() {
        let map = SessionAffinityMap::new(0);
        map.set("session-1", "a");
        map.set("session-2", "b");
        std::thread::sleep(std::time::Duration::from_millis(10));
        map.evict_expired();
        assert_eq!(map.get("session-1"), None);
        assert_eq!(map.get("session-2"), None);
    }
}
