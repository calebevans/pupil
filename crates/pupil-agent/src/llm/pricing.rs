use std::collections::HashMap;
use std::sync::LazyLock;

use super::TokenUsage;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPricing {
    pub const fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }

    pub fn cost(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        input_cost + output_cost
    }
}

impl PartialEq for ModelPricing {
    fn eq(&self, other: &Self) -> bool {
        (self.input_per_million - other.input_per_million).abs() < f64::EPSILON
            && (self.output_per_million - other.output_per_million).abs() < f64::EPSILON
    }
}

impl From<crate::config::PricingOverride> for ModelPricing {
    fn from(p: crate::config::PricingOverride) -> Self {
        Self {
            input_per_million: p.input_per_million,
            output_per_million: p.output_per_million,
        }
    }
}

pub static PRICING: LazyLock<HashMap<&'static str, ModelPricing>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // Anthropic
    m.insert("claude-opus-4", ModelPricing::new(15.00, 75.00));
    m.insert("claude-sonnet-4-6", ModelPricing::new(3.00, 15.00));
    m.insert("claude-sonnet-4", ModelPricing::new(3.00, 15.00));
    m.insert("claude-haiku-4", ModelPricing::new(0.80, 4.00));

    // OpenAI
    m.insert("gpt-4o", ModelPricing::new(2.50, 10.00));
    m.insert("gpt-4o-mini", ModelPricing::new(0.15, 0.60));
    m.insert("gpt-4.1", ModelPricing::new(2.00, 8.00));
    m.insert("gpt-4.1-mini", ModelPricing::new(0.40, 1.60));
    m.insert("gpt-4.1-nano", ModelPricing::new(0.10, 0.40));

    // Google Gemini
    m.insert("gemini-2.5-pro", ModelPricing::new(1.25, 10.00));
    m.insert("gemini-2.5-flash", ModelPricing::new(0.15, 0.60));

    m
});

pub fn model_pricing(
    model: &str,
    overrides: &HashMap<String, ModelPricing>,
) -> Option<ModelPricing> {
    if let Some(pricing) = overrides.get(model) {
        return Some(*pricing);
    }

    if let Some(pricing) = PRICING.get(model) {
        return Some(*pricing);
    }

    let stripped = model
        .strip_prefix("anthropic/")
        .or_else(|| model.strip_prefix("openai/"))
        .or_else(|| model.strip_prefix("google/"))
        .or_else(|| model.strip_prefix("azure/"))
        .or_else(|| model.strip_prefix("bedrock/"))
        .or_else(|| model.strip_prefix("vertex/"));

    if let Some(name) = stripped {
        if let Some(pricing) = PRICING.get(name) {
            return Some(*pricing);
        }
    }

    if model.starts_with("ollama/") || model.starts_with("ollama:") {
        return Some(ModelPricing::new(0.0, 0.0));
    }

    None
}

#[derive(Debug)]
pub struct CostTracker {
    model: String,
    pricing: Option<ModelPricing>,
    cumulative: TokenUsage,
    per_source: HashMap<String, TokenUsage>,
    budget_usd: Option<f64>,
}

impl CostTracker {
    pub fn new(model: impl Into<String>, overrides: &HashMap<String, ModelPricing>) -> Self {
        let model = model.into();
        let pricing = model_pricing(&model, overrides);

        if pricing.is_none() {
            tracing::warn!(
                model = %model,
                "Unknown model pricing. Cost will be reported as $0.00. \
                 Add pricing in pupil.yaml under the `pricing` section."
            );
        }

        Self {
            model,
            pricing,
            cumulative: TokenUsage::default(),
            per_source: HashMap::new(),
            budget_usd: None,
        }
    }

    pub fn with_budget(mut self, budget_usd: f64) -> Self {
        self.budget_usd = Some(budget_usd);
        self
    }

    pub fn record(&mut self, usage: &TokenUsage, key: &str) {
        self.cumulative.accumulate(usage);

        self.per_source
            .entry(key.to_string())
            .or_default()
            .accumulate(usage);
    }

    pub fn estimated_cost_usd(&self) -> f64 {
        self.pricing
            .map(|p| p.cost(&self.cumulative))
            .unwrap_or(0.0)
    }

    pub fn cost_for_key(&self, key: &str) -> f64 {
        let usage = self.per_source.get(key).cloned().unwrap_or_default();
        self.pricing.map(|p| p.cost(&usage)).unwrap_or(0.0)
    }

    pub fn budget_exceeded(&self) -> bool {
        self.budget_usd
            .map_or(false, |b| self.estimated_cost_usd() > b)
    }

    pub fn budget_usd(&self) -> Option<f64> {
        self.budget_usd
    }

    pub fn cumulative_usage(&self) -> &TokenUsage {
        &self.cumulative
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn pricing(&self) -> Option<ModelPricing> {
        self.pricing
    }

    pub fn per_key_usage(&self) -> impl Iterator<Item = (&str, &TokenUsage)> {
        self.per_source.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn reset(&mut self) {
        self.cumulative = TokenUsage::default();
        self.per_source.clear();
    }
}

pub fn estimate_source_cost(
    content: &str,
    model: &str,
    overrides: &HashMap<String, ModelPricing>,
    overhead_factor: f64,
) -> (u64, u64, f64) {
    let raw_tokens = (content.len() as f64 / 4.0) as u64;
    let total_tokens = (raw_tokens as f64 * overhead_factor) as u64;
    let input_tokens = (total_tokens as f64 * 0.7) as u64;
    let output_tokens = total_tokens - input_tokens;

    let usage = TokenUsage {
        input_tokens,
        output_tokens,
    };

    let cost = model_pricing(model, overrides)
        .map(|p| p.cost(&usage))
        .unwrap_or(0.0);

    (input_tokens, output_tokens, cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_model_pricing() {
        let p = model_pricing("claude-sonnet-4-6", &HashMap::new());
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million - 3.0).abs() < f64::EPSILON);
        assert!((p.output_per_million - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_prefixed_model_pricing() {
        let p = model_pricing("anthropic/claude-haiku-4", &HashMap::new());
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million - 0.80).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ollama_free() {
        let p = model_pricing("ollama/llama3", &HashMap::new());
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million).abs() < f64::EPSILON);
        assert!((p.output_per_million).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ollama_colon_free() {
        let p = model_pricing("ollama:mistral", &HashMap::new());
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million).abs() < f64::EPSILON);
    }

    #[test]
    fn test_unknown_model() {
        let p = model_pricing("mystery-model-9000", &HashMap::new());
        assert!(p.is_none());
    }

    #[test]
    fn test_override() {
        let mut overrides = HashMap::new();
        overrides.insert("my-custom".to_string(), ModelPricing::new(1.5, 6.0));
        let p = model_pricing("my-custom", &overrides);
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million - 1.5).abs() < f64::EPSILON);
        assert!((p.output_per_million - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_override_takes_precedence() {
        let mut overrides = HashMap::new();
        overrides.insert("gpt-4o".to_string(), ModelPricing::new(99.0, 99.0));
        let p = model_pricing("gpt-4o", &overrides);
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p.input_per_million - 99.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cost_calculation() {
        let pricing = ModelPricing::new(3.00, 15.00);
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
        };
        let cost = pricing.cost(&usage);
        assert!((cost - 4.50).abs() < 0.001);
    }

    #[test]
    fn test_cost_calculation_small() {
        let pricing = ModelPricing::new(3.00, 15.00);
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 200,
        };
        let cost = pricing.cost(&usage);
        assert!((cost - 0.006).abs() < 0.0001);
    }

    #[test]
    fn test_cost_tracker_basic() {
        let mut tracker = CostTracker::new("claude-sonnet-4-6", &HashMap::new());
        tracker.record(
            &TokenUsage {
                input_tokens: 100_000,
                output_tokens: 20_000,
            },
            "handbook.md",
        );
        tracker.record(
            &TokenUsage {
                input_tokens: 50_000,
                output_tokens: 10_000,
            },
            "guide.md",
        );

        assert_eq!(tracker.cumulative_usage().input_tokens, 150_000);
        assert_eq!(tracker.cumulative_usage().output_tokens, 30_000);
        assert!(tracker.estimated_cost_usd() > 0.0);
    }

    #[test]
    fn test_cost_tracker_budget() {
        let mut tracker =
            CostTracker::new("claude-sonnet-4-6", &HashMap::new()).with_budget(0.01);
        tracker.record(
            &TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
            },
            "big-doc.md",
        );
        assert!(tracker.budget_exceeded());
    }

    #[test]
    fn test_cost_tracker_no_budget() {
        let mut tracker = CostTracker::new("claude-sonnet-4-6", &HashMap::new());
        tracker.record(
            &TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
            },
            "big-doc.md",
        );
        assert!(!tracker.budget_exceeded());
    }

    #[test]
    fn test_estimate_source_cost() {
        let content = "a".repeat(4000); // ~1000 tokens raw
        let (input, output, cost) =
            estimate_source_cost(&content, "claude-sonnet-4-6", &HashMap::new(), 3.0);
        assert_eq!(input, 2100);
        assert_eq!(output, 900);
        assert!(cost > 0.0);
    }

    #[test]
    fn test_token_usage_accumulate() {
        let mut a = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        };
        let b = TokenUsage {
            input_tokens: 200,
            output_tokens: 75,
        };
        a.accumulate(&b);
        assert_eq!(a.input_tokens, 300);
        assert_eq!(a.output_tokens, 125);
        assert_eq!(a.total(), 425);
    }
}
