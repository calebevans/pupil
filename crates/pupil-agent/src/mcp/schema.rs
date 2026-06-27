use rmcp::model::CallToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ToolDefinition;

impl From<&ToolDefinition> for crate::llm::ToolDefinition {
    fn from(mcp_tool: &ToolDefinition) -> Self {
        crate::llm::ToolDefinition {
            name: mcp_tool.name.clone(),
            description: mcp_tool.description.clone().unwrap_or_default(),
            input_schema: mcp_tool.input_schema.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OpenAiFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunction {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub parameters: Value,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub input_schema: Value,
}

const OPENAI_STRIPPED_KEYWORDS: &[&str] = &[
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "patternProperties",
    "$ref",
    "$defs",
    "$id",
    "$schema",
    "$comment",
    "title",
    "examples",
    "default",
    "if",
    "then",
    "else",
    "not",
    "deprecated",
    "readOnly",
    "writeOnly",
    "contentMediaType",
    "contentEncoding",
];

pub fn strip_for_openai_strict(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        for keyword in OPENAI_STRIPPED_KEYWORDS {
            obj.remove(*keyword);
        }

        if let Some(props) = obj.get_mut("properties") {
            if let Some(props_obj) = props.as_object_mut() {
                for (_key, prop_schema) in props_obj.iter_mut() {
                    strip_for_openai_strict(prop_schema);
                }
            }
        }

        if let Some(items) = obj.get_mut("items") {
            strip_for_openai_strict(items);
        }

        for combiner in &["allOf", "anyOf", "oneOf"] {
            if let Some(arr) = obj.get_mut(*combiner) {
                if let Some(arr_vec) = arr.as_array_mut() {
                    for sub in arr_vec.iter_mut() {
                        strip_for_openai_strict(sub);
                    }
                }
            }
        }

        if let Some(ap) = obj.get_mut("additionalProperties") {
            if ap.is_object() {
                strip_for_openai_strict(ap);
            }
        }
    }
}

pub fn to_openai_tool(tool: &ToolDefinition, strict: bool) -> OpenAiTool {
    let mut parameters = tool.input_schema.clone();

    if strict {
        strip_for_openai_strict(&mut parameters);
    }

    OpenAiTool {
        tool_type: "function".to_string(),
        function: OpenAiFunction {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters,
            strict: if strict { Some(true) } else { None },
        },
    }
}

pub fn to_anthropic_tool(tool: &ToolDefinition) -> AnthropicTool {
    AnthropicTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }
}

pub fn to_openai_tools(tools: &[ToolDefinition], strict: bool) -> Vec<OpenAiTool> {
    tools.iter().map(|t| to_openai_tool(t, strict)).collect()
}

pub fn to_anthropic_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools.iter().map(to_anthropic_tool).collect()
}

pub fn tool_result_to_string(result: &CallToolResult) -> String {
    let is_error = result.is_error.unwrap_or(false);

    let texts: Vec<String> = result
        .content
        .iter()
        .filter_map(|content| {
            if let Some(text_content) = content.raw.as_text() {
                Some(text_content.text.clone())
            } else {
                let val = serde_json::to_value(&content.raw).ok()?;
                let type_str = val.get("type")?.as_str()?;
                Some(format!("[{type_str} content]"))
            }
        })
        .collect();

    let body = texts.join("\n");

    if is_error {
        format!("Error: {body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::ToolDefinition;

    fn sample_tool() -> ToolDefinition {
        ToolDefinition {
            name: "store_memory".to_string(),
            original_name: "store_memory".to_string(),
            description: Some("Store a new observation.".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Short description",
                        "maxLength": 2000,
                        "minLength": 1
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 0,
                        "uniqueItems": true
                    },
                    "score": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "format": "double"
                    }
                },
                "required": ["summary"],
                "$schema": "https://json-schema.org/draft/2020-12/schema"
            }),
        }
    }

    #[test]
    fn test_to_openai_strict_strips_keywords() {
        let tool = sample_tool();
        let openai = to_openai_tool(&tool, true);

        assert_eq!(openai.tool_type, "function");
        assert_eq!(openai.function.name, "store_memory");
        assert_eq!(openai.function.strict, Some(true));

        let params = &openai.function.parameters;

        assert!(params.get("$schema").is_none());

        let props = params.get("properties").unwrap();

        let summary = &props["summary"];
        assert!(summary.get("maxLength").is_none());
        assert!(summary.get("minLength").is_none());
        assert_eq!(summary.get("type").unwrap(), "string");
        assert_eq!(summary.get("description").unwrap(), "Short description");

        let tags = &props["tags"];
        assert!(tags.get("minItems").is_none());
        assert!(tags.get("uniqueItems").is_none());
        assert_eq!(tags.get("type").unwrap(), "array");

        let score = &props["score"];
        assert!(score.get("minimum").is_none());
        assert!(score.get("maximum").is_none());
        assert!(score.get("format").is_none());
        assert_eq!(score.get("type").unwrap(), "number");

        assert!(params.get("required").is_some());
    }

    #[test]
    fn test_to_openai_non_strict_preserves_keywords() {
        let tool = sample_tool();
        let openai = to_openai_tool(&tool, false);

        assert_eq!(openai.function.strict, None);

        let props = &openai.function.parameters["properties"];
        assert!(props["summary"].get("maxLength").is_some());
        assert!(props["score"].get("minimum").is_some());
    }

    #[test]
    fn test_to_anthropic_preserves_all_keywords() {
        let tool = sample_tool();
        let anthropic = to_anthropic_tool(&tool);

        assert_eq!(anthropic.name, "store_memory");
        assert_eq!(
            anthropic.description.as_deref(),
            Some("Store a new observation.")
        );

        let props = &anthropic.input_schema["properties"];
        assert_eq!(props["summary"]["maxLength"], 2000);
        assert_eq!(props["summary"]["minLength"], 1);
        assert_eq!(props["score"]["minimum"], 0.0);
        assert_eq!(props["score"]["maximum"], 1.0);
        assert_eq!(props["score"]["format"], "double");
        assert_eq!(props["tags"]["uniqueItems"], true);
    }

    #[test]
    fn test_strip_nested_schemas() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {
                        "deep": {
                            "type": "string",
                            "format": "uuid",
                            "pattern": "^[0-9a-f-]+$"
                        }
                    }
                },
                "list": {
                    "type": "array",
                    "items": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100
                    }
                }
            }
        });

        strip_for_openai_strict(&mut schema);

        let deep = &schema["properties"]["nested"]["properties"]["deep"];
        assert!(deep.get("format").is_none());
        assert!(deep.get("pattern").is_none());
        assert_eq!(deep.get("type").unwrap(), "string");

        let items = &schema["properties"]["list"]["items"];
        assert!(items.get("minimum").is_none());
        assert!(items.get("maximum").is_none());
        assert_eq!(items.get("type").unwrap(), "integer");
    }

    #[test]
    fn test_to_openai_tools_batch() {
        let tools = vec![sample_tool()];
        let result = to_openai_tools(&tools, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].function.name, "store_memory");
    }

    #[test]
    fn test_to_anthropic_tools_batch() {
        let tools = vec![sample_tool()];
        let result = to_anthropic_tools(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "store_memory");
    }

    #[test]
    fn test_empty_schema() {
        let tool = ToolDefinition {
            name: "no_params".to_string(),
            original_name: "no_params".to_string(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
        };

        let openai = to_openai_tool(&tool, true);
        assert_eq!(
            openai.function.parameters,
            serde_json::json!({"type": "object"})
        );
        assert!(openai.function.description.is_none());

        let anthropic = to_anthropic_tool(&tool);
        assert_eq!(
            anthropic.input_schema,
            serde_json::json!({"type": "object"})
        );
        assert!(anthropic.description.is_none());
    }

    #[test]
    fn test_tool_result_to_string_success() {
        let result = CallToolResult::success(vec![]);
        let text = tool_result_to_string(&result);
        assert_eq!(text, "");
    }

    #[test]
    fn test_tool_result_to_string_error() {
        let result = CallToolResult::error(vec![]);
        let text = tool_result_to_string(&result);
        assert_eq!(text, "Error: ");
    }

    #[test]
    fn test_strip_combiners() {
        let mut schema = serde_json::json!({
            "anyOf": [
                {
                    "type": "string",
                    "format": "email"
                },
                {
                    "type": "string",
                    "format": "uri"
                }
            ]
        });
        strip_for_openai_strict(&mut schema);

        let variants = schema["anyOf"].as_array().unwrap();
        for v in variants {
            assert!(v.get("format").is_none());
            assert_eq!(v.get("type").unwrap(), "string");
        }
    }
}
