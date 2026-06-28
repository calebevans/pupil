use crate::llm::ToolDefinition;
use super::AgentRegistryEntry;

pub fn ask_agent_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "ask_agent".to_string(),
        description: "Send a question to another agent and receive its response. \
            Use this when the question falls outside your expertise and another \
            agent is better suited to answer it. Do not use this to delegate \
            work that you can handle yourself."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "The name of the agent to ask (must be one of the available agents listed in your system prompt)."
                },
                "question": {
                    "type": "string",
                    "description": "The question to send to the other agent. Be specific and self-contained; the other agent does not have access to your conversation history."
                }
            },
            "required": ["agent", "question"]
        }),
    }
}

pub fn build_system_prompt_section(agents: &[AgentRegistryEntry]) -> String {
    let mut section = String::new();
    section.push_str("## Available Agents\n\n");
    section.push_str(
        "You can delegate questions to the following agents using the `ask_agent` tool:\n\n",
    );
    for agent in agents {
        if agent.description.is_empty() {
            section.push_str(&format!("- **{}**\n", agent.name));
        } else {
            section.push_str(&format!("- **{}**: {}\n", agent.name, agent.description));
        }
    }
    section.push_str(
        "\nOnly use ask_agent when the question genuinely falls outside your expertise. \
         Provide a self-contained question with all necessary context, as the other \
         agent cannot see your conversation history.",
    );
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ask_agent_tool_definition() {
        let tool = ask_agent_tool_definition();
        assert_eq!(tool.name, "ask_agent");
        assert!(!tool.description.is_empty());
        let props = tool.input_schema["properties"].as_object().unwrap();
        assert!(props.contains_key("agent"));
        assert!(props.contains_key("question"));
        let required = tool.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn test_build_system_prompt_section() {
        let agents = vec![
            AgentRegistryEntry {
                name: "db-expert".to_string(),
                url: "http://db:8080".to_string(),
                description: "SQL and schema design".to_string(),
            },
            AgentRegistryEntry {
                name: "fe-expert".to_string(),
                url: "http://fe:8080".to_string(),
                description: "".to_string(),
            },
        ];
        let section = build_system_prompt_section(&agents);
        assert!(section.contains("## Available Agents"));
        assert!(section.contains("**db-expert**: SQL and schema design"));
        assert!(section.contains("**fe-expert**"));
        assert!(section.contains("ask_agent"));
    }
}
