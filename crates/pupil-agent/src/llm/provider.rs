use super::{LlmError, LlmProvider};
use super::genai_provider::GenaiProvider;
use super::openai_compat::OpenAiCompatProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    Gemini,
    Ollama,
    Bedrock,
    Vertex,
    Azure,
    OpenAiCompat {
        base_url: String,
    },
}

#[derive(Debug, Clone)]
pub struct ParsedModel {
    pub kind: ProviderKind,
    pub model_name: String,
}

pub fn parse_model_string(model: &str) -> Result<ParsedModel, LlmError> {
    // openai-compat:base_url/model
    if let Some(rest) = model.strip_prefix("openai-compat:") {
        if let Some(last_slash) = rest.rfind('/') {
            let base_url = rest[..last_slash].to_string();
            let model_name = rest[last_slash + 1..].to_string();
            if base_url.is_empty() || model_name.is_empty() {
                return Err(LlmError::UnknownModelFormat {
                    model: model.to_string(),
                });
            }
            return Ok(ParsedModel {
                kind: ProviderKind::OpenAiCompat { base_url },
                model_name,
            });
        }
        return Err(LlmError::UnknownModelFormat {
            model: model.to_string(),
        });
    }

    // Prefixed patterns: "prefix/model_name"
    let prefixed_patterns: &[(&str, fn(&str) -> ProviderKind)] = &[
        ("azure/", |_| ProviderKind::Azure),
        ("bedrock/", |_| ProviderKind::Bedrock),
        ("vertex/", |_| ProviderKind::Vertex),
        ("ollama/", |_| ProviderKind::Ollama),
        ("anthropic/", |_| ProviderKind::Anthropic),
        ("openai/", |_| ProviderKind::OpenAi),
        ("google/", |_| ProviderKind::Gemini),
    ];

    for (prefix, kind_fn) in prefixed_patterns {
        if let Some(name) = model.strip_prefix(prefix) {
            return Ok(ParsedModel {
                kind: kind_fn(name),
                model_name: name.to_string(),
            });
        }
    }

    // ollama:model (colon variant)
    if let Some(name) = model.strip_prefix("ollama:") {
        return Ok(ParsedModel {
            kind: ProviderKind::Ollama,
            model_name: name.to_string(),
        });
    }

    // Bare model names matched by prefix
    if model.starts_with("claude-") {
        return Ok(ParsedModel {
            kind: ProviderKind::Anthropic,
            model_name: model.to_string(),
        });
    }
    if model.starts_with("gpt-") {
        return Ok(ParsedModel {
            kind: ProviderKind::OpenAi,
            model_name: model.to_string(),
        });
    }
    if model.starts_with("gemini-") {
        return Ok(ParsedModel {
            kind: ProviderKind::Gemini,
            model_name: model.to_string(),
        });
    }

    Err(LlmError::UnknownModelFormat {
        model: model.to_string(),
    })
}

pub fn required_env_var(kind: &ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
        ProviderKind::OpenAi => Some("OPENAI_API_KEY"),
        ProviderKind::Gemini => Some("GOOGLE_API_KEY"),
        ProviderKind::Azure => Some("AZURE_OPENAI_API_KEY"),
        ProviderKind::Vertex => Some("GOOGLE_APPLICATION_CREDENTIALS"),
        ProviderKind::Ollama => None,
        ProviderKind::Bedrock => None,
        ProviderKind::OpenAiCompat { .. } => Some("OPENAI_API_KEY"),
    }
}

pub fn resolve_provider(model: &str) -> Result<Box<dyn LlmProvider>, LlmError> {
    let parsed = parse_model_string(model)?;

    match parsed.kind {
        ProviderKind::Azure => {
            let provider = OpenAiCompatProvider::new_azure(parsed.model_name)?;
            Ok(Box::new(provider))
        }
        ProviderKind::OpenAiCompat { base_url } => {
            let provider = OpenAiCompatProvider::new_custom(base_url, parsed.model_name)?;
            Ok(Box::new(provider))
        }
        _ => {
            let genai_model_name = match &parsed.kind {
                ProviderKind::Anthropic if !parsed.model_name.starts_with("claude-") => {
                    parsed.model_name.clone()
                }
                ProviderKind::Ollama => {
                    format!("ollama::{}", parsed.model_name)
                }
                ProviderKind::Bedrock => {
                    format!("bedrock::{}", parsed.model_name)
                }
                ProviderKind::Vertex => {
                    format!("vertex::{}", parsed.model_name)
                }
                ProviderKind::Gemini if !parsed.model_name.starts_with("gemini-") => {
                    parsed.model_name.clone()
                }
                ProviderKind::OpenAi if !parsed.model_name.starts_with("gpt-") => {
                    parsed.model_name.clone()
                }
                _ => parsed.model_name.clone(),
            };

            let provider = GenaiProvider::new(genai_model_name)?;
            Ok(Box::new(provider))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_bare() {
        let p = parse_model_string("claude-sonnet-4-6").unwrap();
        assert_eq!(p.kind, ProviderKind::Anthropic);
        assert_eq!(p.model_name, "claude-sonnet-4-6");
    }

    #[test]
    fn test_anthropic_prefixed() {
        let p = parse_model_string("anthropic/claude-haiku-4").unwrap();
        assert_eq!(p.kind, ProviderKind::Anthropic);
        assert_eq!(p.model_name, "claude-haiku-4");
    }

    #[test]
    fn test_openai_bare() {
        let p = parse_model_string("gpt-4o").unwrap();
        assert_eq!(p.kind, ProviderKind::OpenAi);
        assert_eq!(p.model_name, "gpt-4o");
    }

    #[test]
    fn test_openai_prefixed() {
        let p = parse_model_string("openai/gpt-4o-mini").unwrap();
        assert_eq!(p.kind, ProviderKind::OpenAi);
        assert_eq!(p.model_name, "gpt-4o-mini");
    }

    #[test]
    fn test_gemini_bare() {
        let p = parse_model_string("gemini-2.5-flash").unwrap();
        assert_eq!(p.kind, ProviderKind::Gemini);
        assert_eq!(p.model_name, "gemini-2.5-flash");
    }

    #[test]
    fn test_gemini_prefixed() {
        let p = parse_model_string("google/gemini-2.5-pro").unwrap();
        assert_eq!(p.kind, ProviderKind::Gemini);
        assert_eq!(p.model_name, "gemini-2.5-pro");
    }

    #[test]
    fn test_ollama_slash() {
        let p = parse_model_string("ollama/llama3").unwrap();
        assert_eq!(p.kind, ProviderKind::Ollama);
        assert_eq!(p.model_name, "llama3");
    }

    #[test]
    fn test_ollama_colon() {
        let p = parse_model_string("ollama:llama3").unwrap();
        assert_eq!(p.kind, ProviderKind::Ollama);
        assert_eq!(p.model_name, "llama3");
    }

    #[test]
    fn test_bedrock() {
        let p = parse_model_string("bedrock/anthropic.claude-v2").unwrap();
        assert_eq!(p.kind, ProviderKind::Bedrock);
        assert_eq!(p.model_name, "anthropic.claude-v2");
    }

    #[test]
    fn test_vertex() {
        let p = parse_model_string("vertex/gemini-2.5-pro").unwrap();
        assert_eq!(p.kind, ProviderKind::Vertex);
        assert_eq!(p.model_name, "gemini-2.5-pro");
    }

    #[test]
    fn test_azure() {
        let p = parse_model_string("azure/gpt-4o").unwrap();
        assert_eq!(p.kind, ProviderKind::Azure);
        assert_eq!(p.model_name, "gpt-4o");
    }

    #[test]
    fn test_openai_compat() {
        let p =
            parse_model_string("openai-compat:https://api.example.com/v1/my-model").unwrap();
        assert_eq!(
            p.kind,
            ProviderKind::OpenAiCompat {
                base_url: "https://api.example.com/v1".to_string()
            }
        );
        assert_eq!(p.model_name, "my-model");
    }

    #[test]
    fn test_openai_compat_no_slash() {
        let result = parse_model_string("openai-compat:no-slash");
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_model() {
        let result = parse_model_string("some-unknown-model");
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_string() {
        let result = parse_model_string("");
        assert!(result.is_err());
    }

    #[test]
    fn test_claude_opus() {
        let p = parse_model_string("claude-opus-4").unwrap();
        assert_eq!(p.kind, ProviderKind::Anthropic);
        assert_eq!(p.model_name, "claude-opus-4");
    }

    #[test]
    fn test_gpt_4_1_nano() {
        let p = parse_model_string("gpt-4.1-nano").unwrap();
        assert_eq!(p.kind, ProviderKind::OpenAi);
        assert_eq!(p.model_name, "gpt-4.1-nano");
    }
}
