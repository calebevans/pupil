use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::config::{AgentConfig, HealthCheckConfig};

#[derive(Debug)]
pub struct AgentEntry {
    pub name: String,
    pub url: String,
    pub description: String,
    pub topics: Vec<String>,
    pub exclusive_topics: Vec<String>,
    pub sample_questions: Vec<String>,
    pub priority: i32,
    pub healthy: AtomicBool,
    pub consecutive_failures: AtomicU32,
    pub consecutive_successes: AtomicU32,
}

impl AgentEntry {
    pub fn from_config(config: &AgentConfig) -> Self {
        Self {
            name: config.name.clone(),
            url: config.url.clone(),
            description: config.description.clone(),
            topics: config.topics.iter().map(|t| t.to_lowercase()).collect(),
            exclusive_topics: config
                .exclusive_topics
                .iter()
                .map(|t| t.to_lowercase())
                .collect(),
            sample_questions: config.sample_questions.clone(),
            priority: config.priority,
            healthy: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    pub fn mark_healthy(&self) {
        self.healthy.store(true, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Relaxed);
        self.consecutive_successes.store(0, Ordering::Relaxed);
    }
}

pub struct AgentRegistry {
    pub(crate) agents: Vec<Arc<AgentEntry>>,
    pub(crate) http_client: reqwest::Client,
    pub(crate) health_config: HealthCheckConfig,
}

impl AgentRegistry {
    pub fn new(agent_configs: &[AgentConfig], health_config: HealthCheckConfig) -> Self {
        let agents = agent_configs
            .iter()
            .map(|c| Arc::new(AgentEntry::from_config(c)))
            .collect();

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(health_config.timeout_secs))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            agents,
            http_client,
            health_config,
        }
    }

    pub fn all_agents(&self) -> &[Arc<AgentEntry>] {
        &self.agents
    }

    pub fn healthy_agents(&self) -> Vec<Arc<AgentEntry>> {
        self.agents
            .iter()
            .filter(|a| a.is_healthy())
            .cloned()
            .collect()
    }

    pub fn get_agent(&self, name: &str) -> Option<Arc<AgentEntry>> {
        self.agents.iter().find(|a| a.name == name).cloned()
    }

    pub fn add_agent(&mut self, config: &AgentConfig) {
        self.agents.push(Arc::new(AgentEntry::from_config(config)));
    }

    pub fn remove_agent(&mut self, name: &str) -> bool {
        let before = self.agents.len();
        self.agents.retain(|a| a.name != name);
        self.agents.len() < before
    }

    pub async fn check_health_all(&self) {
        for agent in &self.agents {
            let url = format!("{}/health", agent.url);
            let result = self.http_client.get(&url).send().await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    let prev_successes =
                        agent.consecutive_successes.fetch_add(1, Ordering::Relaxed);
                    agent.consecutive_failures.store(0, Ordering::Relaxed);

                    if !agent.is_healthy()
                        && (prev_successes + 1) >= self.health_config.healthy_threshold
                    {
                        agent.mark_healthy();
                        tracing::info!(
                            agent = %agent.name,
                            url = %agent.url,
                            "Agent recovered, added back to pool"
                        );
                    }
                }
                Ok(resp) => {
                    let prev_failures =
                        agent.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                    agent.consecutive_successes.store(0, Ordering::Relaxed);

                    if agent.is_healthy()
                        && (prev_failures + 1) >= self.health_config.unhealthy_threshold
                    {
                        agent.mark_unhealthy();
                        tracing::warn!(
                            agent = %agent.name,
                            url = %agent.url,
                            status = %resp.status(),
                            "Agent unhealthy ({} consecutive failures), removed from pool",
                            prev_failures + 1
                        );
                    }
                }
                Err(e) => {
                    let prev_failures =
                        agent.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                    agent.consecutive_successes.store(0, Ordering::Relaxed);

                    if agent.is_healthy()
                        && (prev_failures + 1) >= self.health_config.unhealthy_threshold
                    {
                        agent.mark_unhealthy();
                        tracing::warn!(
                            agent = %agent.name,
                            url = %agent.url,
                            error = %e,
                            "Agent unreachable ({} consecutive failures), removed from pool",
                            prev_failures + 1
                        );
                    }
                }
            }
        }
    }

    pub fn spawn_health_check_loop(
        self: &Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let registry = Arc::clone(self);
        let interval = Duration::from_secs(registry.health_config.interval_secs);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Health check loop shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        registry.check_health_all().await;
                    }
                }
            }
        })
    }
}
