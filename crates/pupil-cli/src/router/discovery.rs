use std::path::Path;

use super::config::{
    AgentConfig, ClassifierConfig, EmbeddingConfig, HealthCheckConfig, HybridConfig,
    RouterConfig, RouterSettings, default_classifier_model, default_confidence_threshold,
    default_embedding_dims, default_embedding_model, default_embedding_provider,
    default_max_inter_agent_depth, default_ollama_url, default_session_ttl, default_strategy,
    default_listen,
};

pub async fn generate_config_from_agents(
    path: &Path,
    output: &Path,
) -> miette::Result<()> {
    let mut agents = Vec::new();

    let entries = std::fs::read_dir(path)
        .map_err(|e| miette::miette!("Cannot read directory {}: {e}", path.display()))?;

    for entry in entries {
        let entry =
            entry.map_err(|e| miette::miette!("Directory entry error: {e}"))?;
        let entry_path = entry.path();

        if !entry_path.is_dir() {
            continue;
        }

        let pupil_yaml = entry_path.join("pupil.yaml");
        if !pupil_yaml.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&pupil_yaml)
            .map_err(|e| miette::miette!("Cannot read {}: {e}", pupil_yaml.display()))?;

        let doc: serde_json::Value = serde_yml::from_str(&content)
            .map_err(|e| miette::miette!("Invalid YAML in {}: {e}", pupil_yaml.display()))?;

        let name = doc["name"]
            .as_str()
            .unwrap_or_else(|| {
                entry_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
            })
            .to_string();

        let description = doc["description"].as_str().unwrap_or("").to_string();

        let routing = &doc["routing"];

        let topics: Vec<String> = routing["topics"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let exclusive_topics: Vec<String> = routing["exclusive_topics"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let sample_questions: Vec<String> = routing["sample_questions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let priority = routing["priority"].as_i64().unwrap_or(0) as i32;

        agents.push(AgentConfig {
            name: name.clone(),
            url: format!("http://{}:8080", name),
            description,
            topics,
            exclusive_topics,
            sample_questions,
            priority,
        });

        eprintln!("  Discovered: {name}");
    }

    if agents.is_empty() {
        return Err(miette::miette!(
            "No agent directories with pupil.yaml found in {}",
            path.display()
        ));
    }

    let config = RouterConfig {
        router: RouterSettings {
            listen: default_listen(),
            strategy: default_strategy(),
            fallback: "error".to_string(),
            confidence_threshold: default_confidence_threshold(),
            hybrid: HybridConfig::default(),
            embedding: Some(EmbeddingConfig {
                provider: default_embedding_provider(),
                model: default_embedding_model(),
                base_url: default_ollama_url(),
                dimensions: default_embedding_dims(),
            }),
            classifier: Some(ClassifierConfig {
                model: default_classifier_model(),
            }),
            health_check: HealthCheckConfig::default(),
            session_affinity: true,
            session_ttl_secs: default_session_ttl(),
            max_inter_agent_depth: default_max_inter_agent_depth(),
        },
        agents,
    };

    let yaml = serde_yml::to_string(&config)
        .map_err(|e| miette::miette!("Failed to serialize config: {e}"))?;

    std::fs::write(output, &yaml)
        .map_err(|e| miette::miette!("Failed to write {}: {e}", output.display()))?;

    eprintln!(
        "\nGenerated {} with {} agents. Edit agent URLs to match your deployment.",
        output.display(),
        config.agents.len()
    );

    Ok(())
}
