use crate::llm::{ResponseSchema, ToolDefinition};

pub struct SystemPromptBuilder {
    name: String,
    description: String,
    core_instructions: String,
    tools: Vec<ToolDefinition>,
    peers: Vec<PeerAgent>,
    namespace: String,
    collaboration_enabled: bool,
    response_schema: Option<ResponseSchema>,
}

pub struct PeerAgent {
    pub name: String,
    pub description: String,
}

const MEMORY_INSTRUCTIONS_TEMPLATE: &str = "\
You have access to a memory system (recalld) that stores knowledge \
you have been taught.

## How to search your memory

Use recall_memories to search. Available parameters:

- **query** (required): Natural language search query. Rephrase the question \
  to focus on the key fact you need.
- **namespace**: Use \"{namespace}\".
- **entities**: Filter to memories mentioning these people, places, or proper nouns. \
  Always pass entity names when the question is about specific individuals.
- **topics**: Filter by subject area (lowercase keywords like \"family\", \"occupation\").
- **tags**: Filter by structured tags (e.g., \"source/filename.md\", \"type/relationship\").
- **depth**: Graph hops (0-3). Use 1 or 2 to find connected memories. Critical for \
  questions that span multiple facts.
- **limit**: Number of results (default 10, max 100). Increase for broad searches.
- **compact**: Set to false to get full metadata including graph edges and scores.

## Search strategy

1. Always include entity names in the entities filter when asking about specific people or things.
2. Use depth 1-2 to follow graph connections between related memories.
3. For chain questions (\"Who is the X of the Y of Z?\"), search step by step:
   - Search for Z to find Y's name
   - Search for Y to find X's name
   - Each search should use the entity filter with the name you found
4. Make multiple recall_memories calls when needed. Each call can target a different angle.
5. If the first search returns nothing, try rephrasing or broadening the query.

## Other useful tools

- **list_memories**: Browse memories by entity or tag filter without a search query. \
  Use when you need to enumerate (e.g., \"list all people\" or \"what sources were learned\").
- **reinforce_memory**: After using a memory to answer a question, call reinforce_memory \
  with the memory's ID and quality=4 to strengthen it for future recall.
- **get_memory**: Retrieve a specific memory by ID if you have one from a previous search.

## After searching

- Synthesize results into a natural response
- If nothing found, say so honestly
- Never guess or fabricate information
- Reinforce memories that were useful (call reinforce_memory with quality 3 or 4)

When you learn something new from the user:
- Use store_memory with namespace \"{namespace}\"
- Include all relevant entities, topics, and tags";

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
            collaboration_enabled: false,
            response_schema: None,
        }
    }

    pub fn with_peers(mut self, peers: Vec<PeerAgent>) -> Self {
        self.peers = peers;
        self
    }

    pub fn with_collaboration(mut self, enabled: bool) -> Self {
        self.collaboration_enabled = enabled;
        self
    }

    pub fn with_response_schema(mut self, schema: &ResponseSchema) -> Self {
        self.response_schema = Some(schema.clone());
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
            if self.collaboration_enabled {
                prompt.push_str("\n\n## Available Agents\n\n");
                prompt.push_str(
                    "You can delegate questions to the following agents \
                     using the `ask_agent` tool:\n\n",
                );
                for peer in &self.peers {
                    if peer.description.is_empty() {
                        prompt.push_str(&format!("- **{}**\n", peer.name));
                    } else {
                        prompt.push_str(&format!(
                            "- **{}**: {}\n",
                            peer.name, peer.description
                        ));
                    }
                }
                prompt.push_str(
                    "\nOnly use ask_agent when the question genuinely \
                     falls outside your expertise. Provide a \
                     self-contained question with all necessary context, \
                     as the other agent cannot see your conversation \
                     history.",
                );
            } else {
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
        }

        prompt.push_str("\n\n## Response Guidelines\n");
        prompt.push_str(RESPONSE_GUIDELINES);

        if let Some(ref schema) = self.response_schema {
            prompt.push_str("\n\n## Response Format\n\n");
            prompt.push_str("Always respond with a JSON object matching this schema:\n");
            if let Ok(pretty) = serde_json::to_string_pretty(&schema.schema) {
                prompt.push_str(&pretty);
            } else {
                prompt.push_str(&schema.schema.to_string());
            }
            prompt.push_str("\n\nDo not include any text outside the JSON object.");
        }

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
