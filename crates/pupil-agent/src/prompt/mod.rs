use crate::llm::ToolDefinition;

pub struct SystemPromptBuilder {
    name: String,
    description: String,
    core_instructions: String,
    tools: Vec<ToolDefinition>,
    peers: Vec<PeerAgent>,
    namespace: String,
}

pub struct PeerAgent {
    pub name: String,
    pub description: String,
}

const MEMORY_INSTRUCTIONS_TEMPLATE: &str = "\
You have access to a memory system (recalld) that stores knowledge \
you have been taught.

Before answering any question:
1. Use recall_memories with namespace \"{namespace}\" to search for relevant knowledge
2. If results are found, use them to inform your response
3. If no results are found, say so honestly

When you learn something new from the user:
- Use store_memory with namespace \"{namespace}\" to save it for future reference
- Include relevant entities, topics, and tags";

const RESPONSE_GUIDELINES: &str = "\
- Ground your answers in retrieved knowledge when available
- If you are uncertain, say so rather than guessing
- Be concise and direct
- When citing remembered knowledge, do not quote the raw memory; \
  synthesize it into a natural response";

impl SystemPromptBuilder {
    pub fn new(
        name: String,
        description: String,
        core_instructions: String,
        tools: Vec<ToolDefinition>,
        namespace: String,
    ) -> Self {
        Self {
            name,
            description,
            core_instructions,
            tools,
            peers: Vec::new(),
            namespace,
        }
    }

    pub fn with_peers(mut self, peers: Vec<PeerAgent>) -> Self {
        self.peers = peers;
        self
    }

    pub fn build(&self) -> String {
        let mut prompt = String::with_capacity(4096);

        if self.description.is_empty() {
            prompt.push_str(&format!("You are {}.", self.name));
        } else {
            prompt.push_str(&format!("You are {}: {}", self.name, self.description));
        }

        if !self.core_instructions.trim().is_empty() {
            prompt.push_str("\n\n## Core Instructions\n");
            prompt.push_str(self.core_instructions.trim());
        }

        let has_memory_tools = self
            .tools
            .iter()
            .any(|t| t.name == "recall_memories" || t.name == "store_memory");
        if has_memory_tools {
            prompt.push_str("\n\n## Memory Usage\n");
            let memory_instructions = MEMORY_INSTRUCTIONS_TEMPLATE
                .replace("{namespace}", &self.namespace);
            prompt.push_str(&memory_instructions);
        }

        if !self.tools.is_empty() {
            prompt.push_str("\n\n## Available Tools\n");
            prompt.push_str(&self.format_tools());
        }

        if !self.peers.is_empty() {
            prompt.push_str("\n\n## Other Available Agents\n");
            prompt.push_str(
                "If the user's question involves a domain outside \
                 your expertise, suggest they ask one of these \
                 specialized agents:\n",
            );
            for peer in &self.peers {
                prompt.push_str(&format!("- {}: {}\n", peer.name, peer.description));
            }
        }

        prompt.push_str("\n\n## Response Guidelines\n");
        prompt.push_str(RESPONSE_GUIDELINES);

        prompt
    }

    fn format_tools(&self) -> String {
        let mut out = String::new();

        for tool in &self.tools {
            out.push_str(&format!("- **{}**", tool.name));
            if !tool.description.is_empty() {
                out.push_str(&format!(": {}", tool.description));
            }
            out.push('\n');

            let schema = &tool.input_schema;
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                let required: Vec<&str> = schema
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                let params: Vec<String> = props
                    .iter()
                    .map(|(name, val)| {
                        let type_str = val
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("any");
                        let req = if required.contains(&name.as_str()) {
                            "required"
                        } else {
                            "optional"
                        };
                        format!("{name} ({type_str}, {req})")
                    })
                    .collect();

                if !params.is_empty() {
                    out.push_str(&format!("  Parameters: {}\n", params.join(", ")));
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        }
    }

    #[test]
    fn test_minimal_prompt() {
        let builder = SystemPromptBuilder::new(
            "test-agent".to_string(),
            "".to_string(),
            "".to_string(),
            vec![],
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(prompt.starts_with("You are test-agent."));
        assert!(prompt.contains("## Response Guidelines"));
        assert!(!prompt.contains("## Core Instructions"));
        assert!(!prompt.contains("## Available Tools"));
        assert!(!prompt.contains("## Memory Usage"));
    }

    #[test]
    fn test_prompt_with_description() {
        let builder = SystemPromptBuilder::new(
            "onboarding-bot".to_string(),
            "Onboarding assistant for engineers".to_string(),
            "".to_string(),
            vec![],
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(
            prompt.starts_with("You are onboarding-bot: Onboarding assistant for engineers")
        );
    }

    #[test]
    fn test_prompt_with_core_instructions() {
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "".to_string(),
            "Be helpful and kind.".to_string(),
            vec![],
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(prompt.contains("## Core Instructions"));
        assert!(prompt.contains("Be helpful and kind."));
    }

    #[test]
    fn test_memory_instructions_included_when_recalld_present() {
        let tools = vec![
            make_tool("recall_memories", "Search memories"),
            make_tool("store_memory", "Store a memory"),
        ];
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "".to_string(),
            "".to_string(),
            tools,
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(prompt.contains("## Memory Usage"));
        assert!(prompt.contains("recall_memories"));
    }

    #[test]
    fn test_memory_instructions_excluded_without_recalld() {
        let tools = vec![make_tool("web_search", "Search the web")];
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "".to_string(),
            "".to_string(),
            tools,
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(!prompt.contains("## Memory Usage"));
    }

    #[test]
    fn test_tools_section() {
        let tools = vec![make_tool("web_search", "Search the web")];
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "".to_string(),
            "".to_string(),
            tools,
            "knowledge".to_string(),
        );
        let prompt = builder.build();
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("**web_search**"));
        assert!(prompt.contains("query (string, required)"));
    }

    #[test]
    fn test_peers_section() {
        let peers = vec![
            PeerAgent {
                name: "payments-bot".to_string(),
                description: "Handles billing questions".to_string(),
            },
            PeerAgent {
                name: "infra-bot".to_string(),
                description: "Infrastructure expert".to_string(),
            },
        ];
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "".to_string(),
            "".to_string(),
            vec![],
            "knowledge".to_string(),
        )
        .with_peers(peers);
        let prompt = builder.build();
        assert!(prompt.contains("## Other Available Agents"));
        assert!(prompt.contains("- payments-bot: Handles billing"));
        assert!(prompt.contains("- infra-bot: Infrastructure expert"));
    }

    #[test]
    fn test_full_prompt_ordering() {
        let tools = vec![
            make_tool("recall_memories", "Search memories"),
            make_tool("store_memory", "Store a memory"),
        ];
        let builder = SystemPromptBuilder::new(
            "bot".to_string(),
            "A test bot".to_string(),
            "Be concise.".to_string(),
            tools,
            "knowledge".to_string(),
        );
        let prompt = builder.build();

        let identity_pos = prompt.find("You are bot").unwrap();
        let core_pos = prompt.find("## Core Instructions").unwrap();
        let memory_pos = prompt.find("## Memory Usage").unwrap();
        let tools_pos = prompt.find("## Available Tools").unwrap();
        let guidelines_pos = prompt.find("## Response Guidelines").unwrap();

        assert!(identity_pos < core_pos);
        assert!(core_pos < memory_pos);
        assert!(memory_pos < tools_pos);
        assert!(tools_pos < guidelines_pos);
    }
}
