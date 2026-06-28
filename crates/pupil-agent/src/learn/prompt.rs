#[cfg(feature = "learn")]
use super::source::ResolvedSource;

#[cfg(feature = "learn")]
const BASE_INSTRUCTIONS: &str = "\
You are a learning agent. Your job is to read the following material and learn from it. \
Store what you learn as memories using the store_memory tool.

## Tool Usage Rules

When using store_memory:
- summary: A specific, searchable description of the knowledge (max 2000 chars). \
Write it as something a person would search for.
- fullText: Detailed content. Include reasoning, context, and specifics that the summary cannot capture. \
Omit only if the summary is fully self-contained.
- entities: ALL proper nouns -- people, tools, systems, organizations, product names. These power the knowledge graph.
- topics: 1-5 lowercase keywords describing the subject area (e.g., \"deployment\", \"authentication\").
- tags: Hierarchical categorization. Always include source/<filename>. \
Add type/<concept|procedure|reference|definition|relationship|faq|troubleshooting|policy|decision|breaking-change> as appropriate.
- parentId: If this memory is logically a child of another memory you already stored, pass the parent's ID.
- supersedes: If this memory corrects or replaces an existing memory, pass the old memory's ID.

When using recall_memories:
- Before storing a new memory, search for related topics to check if you already know something similar.
- If you find an existing memory that this new information updates or corrects, use supersedes when storing.

When using find_similar_memories:
- After storing a memory, optionally check for near-duplicates.
- If a duplicate is found (similarity > 0.90), consider whether both are needed or one should supersede the other.

## Important Constraints

- Store insights, not transcriptions. A memory should capture a concept, fact, procedure, or relationship \
in your own words. Do not copy-paste entire paragraphs verbatim (unless exact wording matters, as in policies).
- Create as many or as few memories as the material warrants. Dense technical content deserves many \
focused memories. Simple overviews need fewer, broader ones. Use your judgment.
- For procedures and workflows, store the steps as a coherent single memory, not one memory per step.
- For definitions and terminology, store each term as its own memory.
- For architectural decisions or design rationale, capture the decision AND the reasoning behind it.";

#[cfg(feature = "learn")]
pub fn load_profile(profile_name: &str) -> &'static str {
    match profile_name {
        "general" => include_str!("profiles/general.txt"),
        "reference" => include_str!("profiles/reference.txt"),
        "procedural" => include_str!("profiles/procedural.txt"),
        "conceptual" => include_str!("profiles/conceptual.txt"),
        "faq" => include_str!("profiles/faq.txt"),
        "policy" => include_str!("profiles/policy.txt"),
        "code" => include_str!("profiles/code.txt"),
        _ => {
            tracing::warn!(
                profile = profile_name,
                "Unknown learning profile; falling back to 'general'. \
                 Valid profiles: general, reference, procedural, conceptual, faq, policy, code"
            );
            include_str!("profiles/general.txt")
        }
    }
}

#[derive(Debug, Clone)]
#[cfg(feature = "learn")]
pub struct TemplateVars {
    pub source_file: String,
    pub source_path: String,
    pub agent_name: String,
    pub namespace: String,
    pub heading_path: String,
    pub source_type: String,
}

#[cfg(feature = "learn")]
pub fn substitute_vars(template: &str, vars: &TemplateVars) -> String {
    template
        .replace("{source_file}", &vars.source_file)
        .replace("{source_path}", &vars.source_path)
        .replace("{agent_name}", &vars.agent_name)
        .replace("{namespace}", &vars.namespace)
        .replace("{heading_path}", &vars.heading_path)
        .replace("{source_type}", &vars.source_type)
}

#[cfg(feature = "learn")]
pub fn build_learning_prompt(
    source: &ResolvedSource,
    curriculum_learning_profile: Option<&str>,
    default_learning_prompt: Option<&str>,
    vars: &TemplateVars,
) -> String {
    let guidelines = match (&source.learning_prompt, &source.learning_profile) {
        (Some(custom), None) => substitute_vars(custom, vars),
        (None, Some(profile)) => {
            let profile_text = load_profile(profile);
            substitute_vars(profile_text, vars)
        }
        (None, None) => {
            if let Some(prompt) = default_learning_prompt {
                substitute_vars(prompt, vars)
            } else {
                let profile_name = curriculum_learning_profile.unwrap_or("general");
                let profile_text = load_profile(profile_name);
                substitute_vars(profile_text, vars)
            }
        }
        (Some(_), Some(_)) => {
            tracing::warn!(
                source = %vars.source_file,
                "Both learning_prompt and learning_profile set; using learning_prompt"
            );
            let custom = source.learning_prompt.as_ref().unwrap();
            substitute_vars(custom, vars)
        }
    };

    format!(
        "{}\n\n## Guidelines\n\n{}\n\nSource: {}\nCurriculum: {}\nNamespace: {}",
        BASE_INSTRUCTIONS, guidelines, vars.source_file, vars.agent_name, vars.namespace,
    )
}

#[cfg(all(test, feature = "learn"))]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_vars() {
        let vars = TemplateVars {
            source_file: "api.md".into(),
            source_path: "curriculum/api.md".into(),
            agent_name: "my-agent".into(),
            namespace: "knowledge".into(),
            heading_path: "Auth > OAuth2".into(),
            source_type: "markdown".into(),
        };

        let template = "Studying {source_file} for {agent_name} in {namespace}";
        let result = substitute_vars(template, &vars);
        assert_eq!(result, "Studying api.md for my-agent in knowledge");

        let template2 = "Hello {unknown_var}!";
        let result2 = substitute_vars(template2, &vars);
        assert_eq!(result2, "Hello {unknown_var}!");
    }

    #[test]
    fn test_load_profile_general() {
        let profile = load_profile("general");
        assert!(profile.contains("Read carefully"));
    }

    #[test]
    fn test_load_profile_reference() {
        let profile = load_profile("reference");
        assert!(profile.contains("exhaustive"));
    }

    #[test]
    fn test_load_profile_unknown_falls_back_to_general() {
        let profile = load_profile("nonexistent");
        // Should fall back to general profile instead of panicking
        assert!(profile.contains("Read carefully"));
    }

    #[test]
    fn test_build_learning_prompt() {
        use crate::learn::source::SourceType;
        use std::path::PathBuf;

        let source = ResolvedSource {
            source_key: "api.md".into(),
            file_path: Some(PathBuf::from("/curriculum/api.md")),
            url: None,
            source_type: SourceType::Markdown,
            learning_profile: Some("general".to_string()),
            learning_prompt: None,
            namespace: "knowledge".into(),
            extra_tags: vec![],
        };

        let vars = TemplateVars {
            source_file: "api.md".into(),
            source_path: "curriculum/api.md".into(),
            agent_name: "test-agent".into(),
            namespace: "knowledge".into(),
            heading_path: String::new(),
            source_type: "markdown".into(),
        };

        let prompt = build_learning_prompt(&source, None, None, &vars);
        assert!(prompt.contains("Source: api.md"));
        assert!(prompt.contains("Curriculum: test-agent"));
        assert!(prompt.contains("Namespace: knowledge"));
    }
}
