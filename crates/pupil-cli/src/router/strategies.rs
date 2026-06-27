use std::collections::HashMap;
use std::sync::Arc;

use super::config::EmbeddingConfig;
use super::health::{AgentEntry, AgentRegistry};
use super::{RouterError, RoutingDecision, StrategyTier};

// ---------------------------------------------------------------------------
// Keyword / topic matching
// ---------------------------------------------------------------------------

pub struct KeywordIndex {
    topic_to_agents: HashMap<String, Vec<usize>>,
}

impl KeywordIndex {
    pub fn build(agents: &[Arc<AgentEntry>]) -> Self {
        let mut topic_to_agents: HashMap<String, Vec<usize>> = HashMap::new();

        for (idx, agent) in agents.iter().enumerate() {
            for topic in &agent.topics {
                topic_to_agents
                    .entry(topic.clone())
                    .or_default()
                    .push(idx);
            }
        }

        Self { topic_to_agents }
    }
}

pub fn keyword_match(
    query: &str,
    index: &KeywordIndex,
    registry: &AgentRegistry,
) -> Result<RoutingDecision, RouterError> {
    let query_lower = query.to_lowercase();
    let agents = registry.all_agents();

    let mut match_counts: HashMap<usize, usize> = HashMap::new();

    for (topic, agent_indices) in &index.topic_to_agents {
        if query_lower.contains(topic.as_str()) {
            for &idx in agent_indices {
                if agents[idx].is_healthy() {
                    *match_counts.entry(idx).or_insert(0) += 1;
                }
            }
        }
    }

    if match_counts.is_empty() {
        return Ok(RoutingDecision {
            agent: String::new(),
            confidence: 0.0,
            strategy_tier: StrategyTier::Keyword,
            reasoning: Some("No topic matches found".to_string()),
            alternatives: vec![],
        });
    }

    let mut scored: Vec<(usize, f64)> = match_counts
        .into_iter()
        .map(|(idx, count)| {
            let total = agents[idx].topics.len().max(1) as f64;
            let score = count as f64 / total;
            (idx, score)
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| agents[b.0].priority.cmp(&agents[a.0].priority))
    });

    let best_idx = scored[0].0;
    let best_score = scored[0].1;

    Ok(RoutingDecision {
        agent: agents[best_idx].name.clone(),
        confidence: best_score.min(1.0),
        strategy_tier: StrategyTier::Keyword,
        reasoning: Some(format!(
            "Matched {} topic(s)",
            (best_score * agents[best_idx].topics.len() as f64).round() as usize
        )),
        alternatives: scored[1..]
            .iter()
            .map(|(idx, score)| (agents[*idx].name.clone(), *score))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Embedding similarity
// ---------------------------------------------------------------------------

pub struct EmbeddingClient {
    http_client: reqwest::Client,
    base_url: String,
    model: String,
    #[allow(dead_code)]
    dimensions: usize,
}

impl EmbeddingClient {
    pub fn new(config: &EmbeddingConfig) -> Result<Self, String> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        Ok(Self {
            http_client,
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            dimensions: config.dimensions,
        })
    }

    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let url = format!("{}/api/embed", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });

        let resp = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Embedding request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Embedding API returned {status}: {body}"));
        }

        #[derive(serde::Deserialize)]
        struct EmbedResponse {
            embeddings: Vec<Vec<f32>>,
        }

        let parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse embedding response: {e}"))?;

        parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| "Empty embeddings array in response".to_string())
    }
}

pub struct EmbeddingIndex {
    entries: Vec<(usize, Vec<f32>)>,
}

impl EmbeddingIndex {
    pub fn empty() -> Self {
        Self { entries: vec![] }
    }

    pub async fn build(
        agents: &[Arc<AgentEntry>],
        client: &EmbeddingClient,
    ) -> Result<Self, String> {
        let mut entries = Vec::new();

        for (idx, agent) in agents.iter().enumerate() {
            if !agent.description.is_empty() {
                let vec = client.embed(&agent.description).await?;
                entries.push((idx, vec));
            }

            for question in &agent.sample_questions {
                let vec = client.embed(question).await?;
                entries.push((idx, vec));
            }
        }

        Ok(Self { entries })
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        tracing::warn!(
            a_len = a.len(),
            b_len = b.len(),
            "Vector dimension mismatch in cosine_similarity; returning 0.0"
        );
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    dot / (mag_a * mag_b)
}

pub async fn embedding_similarity(
    query: &str,
    index: &EmbeddingIndex,
    client: &EmbeddingClient,
    registry: &AgentRegistry,
) -> Result<RoutingDecision, RouterError> {
    let query_vec = client
        .embed(query)
        .await
        .map_err(RouterError::EmbeddingError)?;

    let agents = registry.all_agents();

    let mut agent_scores: HashMap<usize, f32> = HashMap::new();

    for (agent_idx, entry_vec) in &index.entries {
        if !agents[*agent_idx].is_healthy() {
            continue;
        }

        let sim = cosine_similarity(&query_vec, entry_vec);
        let current = agent_scores.entry(*agent_idx).or_insert(0.0);
        if sim > *current {
            *current = sim;
        }
    }

    if agent_scores.is_empty() {
        return Ok(RoutingDecision {
            agent: String::new(),
            confidence: 0.0,
            strategy_tier: StrategyTier::Embedding,
            reasoning: Some("No embedding matches found".to_string()),
            alternatives: vec![],
        });
    }

    let mut scored: Vec<(usize, f64)> = agent_scores
        .into_iter()
        .map(|(idx, score)| (idx, score as f64))
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| agents[b.0].priority.cmp(&agents[a.0].priority))
    });

    let best_idx = scored[0].0;
    let best_score = scored[0].1;

    Ok(RoutingDecision {
        agent: agents[best_idx].name.clone(),
        confidence: best_score,
        strategy_tier: StrategyTier::Embedding,
        reasoning: Some(format!(
            "Semantic similarity {:.2} with {}",
            best_score, agents[best_idx].name
        )),
        alternatives: scored[1..]
            .iter()
            .map(|(idx, score)| (agents[*idx].name.clone(), *score))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// LLM classifier
// ---------------------------------------------------------------------------

const CLASSIFIER_PROMPT_TEMPLATE: &str = r#"You are a query router. Given a user's question and a list of available agents, determine which agent should handle the question.

Available agents:
{agent_list}

User question: {query}

Respond with JSON only (no markdown, no explanation outside the JSON):
{
  "agent": "<agent-name or null if no match>",
  "confidence": <float 0.0-1.0>,
  "reasoning": "<one sentence>"
}

If no agent is a good match, respond with:
{
  "agent": null,
  "confidence": 0.0,
  "reasoning": "<why no agent matches>"
}"#;

#[derive(Debug, serde::Deserialize)]
struct ClassifierResponse {
    agent: Option<String>,
    confidence: f64,
    reasoning: String,
}

pub struct LlmClassifier {
    model: String,
    http_client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl LlmClassifier {
    pub fn new(model: &str) -> Result<Self, String> {
        let (base_url, api_key) = resolve_llm_config(model)?;

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        Ok(Self {
            model: model.to_string(),
            http_client,
            base_url,
            api_key,
        })
    }

    async fn classify(
        &self,
        query: &str,
        agent_list: &str,
    ) -> Result<ClassifierResponse, String> {
        let prompt = CLASSIFIER_PROMPT_TEMPLATE
            .replace("{agent_list}", agent_list)
            .replace("{query}", query);

        let response_text = self.call_llm(&prompt).await?;

        if let Ok(parsed) = serde_json::from_str::<ClassifierResponse>(&response_text) {
            return Ok(parsed);
        }

        let start = response_text.find('{');
        let end = response_text.rfind('}');
        if let (Some(s), Some(e)) = (start, end) {
            if s < e {
                let json_str = &response_text[s..=e];
                if let Ok(parsed) = serde_json::from_str::<ClassifierResponse>(json_str) {
                    return Ok(parsed);
                }
            }
        }

        Err(format!(
            "Failed to parse classifier response as JSON: {}",
            response_text
        ))
    }

    async fn call_llm(&self, prompt: &str) -> Result<String, String> {
        if self.model.starts_with("claude") || self.model.starts_with("anthropic") {
            self.call_anthropic(prompt).await
        } else {
            self.call_openai_compat(prompt).await
        }
    }

    async fn call_anthropic(&self, prompt: &str) -> Result<String, String> {
        let url = format!("{}/v1/messages", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 256,
            "temperature": 0,
            "messages": [
                {"role": "user", "content": prompt}
            ]
        });

        let resp = self
            .http_client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Anthropic API request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Anthropic API returned {status}: {body}"));
        }

        #[derive(serde::Deserialize)]
        struct ContentBlock {
            text: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct AnthropicResponse {
            content: Vec<ContentBlock>,
        }

        let parsed: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Anthropic response: {e}"))?;

        parsed
            .content
            .into_iter()
            .find_map(|b| b.text)
            .ok_or_else(|| "No text content in Anthropic response".to_string())
    }

    async fn call_openai_compat(&self, prompt: &str) -> Result<String, String> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 256,
            "temperature": 0,
            "messages": [
                {"role": "user", "content": prompt}
            ]
        });

        let mut req = self
            .http_client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body);

        if !self.api_key.is_empty() {
            req = req.header("authorization", format!("Bearer {}", self.api_key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("LLM API request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("LLM API returned {status}: {body}"));
        }

        #[derive(serde::Deserialize)]
        struct Choice {
            message: ChoiceMessage,
        }
        #[derive(serde::Deserialize)]
        struct ChoiceMessage {
            content: String,
        }
        #[derive(serde::Deserialize)]
        struct CompletionResponse {
            choices: Vec<Choice>,
        }

        let parsed: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse LLM response: {e}"))?;

        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| "No choices in LLM response".to_string())
    }
}

fn resolve_llm_config(model: &str) -> Result<(String, String), String> {
    if model.starts_with("claude") || model.contains("anthropic") {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| "ANTHROPIC_API_KEY not set (needed for classifier model)".to_string())?;
        Ok(("https://api.anthropic.com".to_string(), key))
    } else if model.starts_with("gpt") || model.contains("openai") {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| "OPENAI_API_KEY not set (needed for classifier model)".to_string())?;
        Ok(("https://api.openai.com".to_string(), key))
    } else if model.starts_with("gemini") || model.contains("google") {
        let key = std::env::var("GOOGLE_API_KEY")
            .map_err(|_| "GOOGLE_API_KEY not set (needed for classifier model)".to_string())?;
        Ok((
            "https://generativelanguage.googleapis.com".to_string(),
            key,
        ))
    } else if model.starts_with("ollama") {
        let base = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        Ok((base, String::new()))
    } else {
        let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let base = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com".to_string());
        Ok((base, key))
    }
}

pub async fn llm_classify(
    query: &str,
    classifier: &LlmClassifier,
    registry: &AgentRegistry,
) -> Result<RoutingDecision, RouterError> {
    let healthy = registry.healthy_agents();

    if healthy.is_empty() {
        return Err(RouterError::NoHealthyAgents);
    }

    let agent_list: String = healthy
        .iter()
        .map(|a| format!("- {}: {}", a.name, a.description))
        .collect::<Vec<_>>()
        .join("\n");

    let response = classifier
        .classify(query, &agent_list)
        .await
        .map_err(RouterError::ClassifierError)?;

    match response.agent {
        None => Ok(RoutingDecision {
            agent: String::new(),
            confidence: 0.0,
            strategy_tier: StrategyTier::LlmClassifier,
            reasoning: Some(response.reasoning),
            alternatives: vec![],
        }),
        Some(agent_name) => {
            let exists = healthy.iter().any(|a| a.name == agent_name);
            if !exists {
                tracing::warn!(
                    agent = %agent_name,
                    "LLM classifier selected an unknown agent"
                );
                return Ok(RoutingDecision {
                    agent: String::new(),
                    confidence: 0.0,
                    strategy_tier: StrategyTier::LlmClassifier,
                    reasoning: Some(format!("LLM selected unknown agent '{agent_name}'")),
                    alternatives: vec![],
                });
            }

            Ok(RoutingDecision {
                agent: agent_name,
                confidence: response.confidence.clamp(0.0, 1.0),
                strategy_tier: StrategyTier::LlmClassifier,
                reasoning: Some(response.reasoning),
                alternatives: vec![],
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Hybrid routing (cascading tiers)
// ---------------------------------------------------------------------------

pub struct HybridRouter<'a> {
    pub keyword_index: &'a KeywordIndex,
    pub embedding_index: &'a EmbeddingIndex,
    pub embedding_client: Option<&'a EmbeddingClient>,
    pub classifier: Option<&'a LlmClassifier>,
    pub registry: &'a AgentRegistry,
    pub keyword_threshold: f64,
    pub embedding_threshold: f64,
    pub llm_threshold: f64,
}

pub async fn hybrid_route(ctx: &HybridRouter<'_>, query: &str) -> Result<RoutingDecision, RouterError> {
    // Tier 1: keyword
    let kw = keyword_match(query, ctx.keyword_index, ctx.registry)?;
    if kw.confidence >= ctx.keyword_threshold {
        return Ok(kw);
    }

    // Tier 2: embedding (if configured)
    if let Some(client) = ctx.embedding_client {
        let emb = embedding_similarity(query, ctx.embedding_index, client, ctx.registry).await?;
        if emb.confidence >= ctx.embedding_threshold {
            return Ok(emb);
        }
    }

    // Tier 3: LLM classifier (if configured)
    if let Some(cls) = ctx.classifier {
        let llm = llm_classify(query, cls, ctx.registry).await?;
        if llm.confidence >= ctx.llm_threshold {
            return Ok(llm);
        }
    }

    // Tier 4: no match with sufficient confidence
    Ok(RoutingDecision {
        agent: String::new(),
        confidence: 0.0,
        strategy_tier: StrategyTier::Fallback,
        reasoning: Some("No strategy tier matched above threshold".to_string()),
        alternatives: vec![],
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32};

    fn make_agents() -> Vec<Arc<AgentEntry>> {
        vec![
            Arc::new(AgentEntry {
                name: "payments".to_string(),
                url: "http://payments:8082".to_string(),
                description: "Payments expert".to_string(),
                topics: vec![
                    "payments".into(),
                    "billing".into(),
                    "refunds".into(),
                    "stripe".into(),
                ],
                exclusive_topics: vec!["stripe-webhooks".into()],
                sample_questions: vec![],
                priority: 0,
                healthy: AtomicBool::new(true),
                consecutive_failures: AtomicU32::new(0),
                consecutive_successes: AtomicU32::new(0),
            }),
            Arc::new(AgentEntry {
                name: "infra".to_string(),
                url: "http://infra:8083".to_string(),
                description: "Infrastructure expert".to_string(),
                topics: vec![
                    "infrastructure".into(),
                    "deployment".into(),
                    "kubernetes".into(),
                    "monitoring".into(),
                ],
                exclusive_topics: vec![],
                sample_questions: vec![],
                priority: 0,
                healthy: AtomicBool::new(true),
                consecutive_failures: AtomicU32::new(0),
                consecutive_successes: AtomicU32::new(0),
            }),
        ]
    }

    fn make_registry(agents: Vec<Arc<AgentEntry>>) -> AgentRegistry {
        use super::super::config::HealthCheckConfig;
        AgentRegistry {
            agents,
            http_client: reqwest::Client::new(),
            health_config: HealthCheckConfig::default(),
        }
    }

    #[test]
    fn keyword_match_single_topic() {
        let agents = make_agents();
        let registry = make_registry(agents.clone());
        let index = KeywordIndex::build(&agents);

        let decision =
            keyword_match("How do I handle refunds?", &index, &registry).unwrap();
        assert_eq!(decision.agent, "payments");
        assert!(decision.confidence > 0.0);
        assert_eq!(decision.strategy_tier, StrategyTier::Keyword);
    }

    #[test]
    fn keyword_match_multiple_topics() {
        let agents = make_agents();
        let registry = make_registry(agents.clone());
        let index = KeywordIndex::build(&agents);

        // "refunds" and "stripe" match 2 of 4 topics = 0.5
        let decision =
            keyword_match("How do I process refunds through stripe?", &index, &registry)
                .unwrap();
        assert_eq!(decision.agent, "payments");
        assert!((decision.confidence - 0.5).abs() < 0.01);
    }

    #[test]
    fn keyword_no_match() {
        let agents = make_agents();
        let registry = make_registry(agents.clone());
        let index = KeywordIndex::build(&agents);

        let decision =
            keyword_match("Tell me about the weather", &index, &registry).unwrap();
        assert_eq!(decision.agent, "");
        assert_eq!(decision.confidence, 0.0);
    }

    #[test]
    fn keyword_unhealthy_agent_excluded() {
        let agents = make_agents();
        agents[0]
            .healthy
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let registry = make_registry(agents.clone());
        let index = KeywordIndex::build(&agents);

        let decision = keyword_match("process a refund", &index, &registry).unwrap();
        assert_ne!(decision.agent, "payments");
    }

    #[test]
    fn cosine_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similar_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.1, 2.1, 2.9];
        let sim = cosine_similarity(&a, &b);
        assert!(sim > 0.99);
    }
}
