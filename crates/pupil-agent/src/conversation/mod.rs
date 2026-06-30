use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::llm::{ChatResponse, Message, ToolResult};

#[derive(Debug, Error)]
pub enum ConversationError {
    #[error("Failed to read session file: {path}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse session file: {path}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("Failed to write session file: {path}")]
    WriteError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

const SESSION_DIR: &str = "/data/sessions";

#[derive(Debug, Serialize, Deserialize)]
pub struct ConversationManager {
    messages: Vec<Message>,
    session_id: Uuid,
    system_prompt: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
    #[serde(skip)]
    turn_input_tokens: u64,
    #[serde(skip)]
    turn_output_tokens: u64,
}

impl ConversationManager {
    pub fn new(system_prompt: String, session_id: Uuid) -> Self {
        let system_message = Message::system(system_prompt.clone());

        Self {
            messages: vec![system_message],
            session_id,
            system_prompt,
            total_input_tokens: 0,
            total_output_tokens: 0,
            turn_input_tokens: 0,
            turn_output_tokens: 0,
        }
    }

    pub fn push_user(&mut self, content: &str) {
        self.turn_input_tokens = 0;
        self.turn_output_tokens = 0;
        self.messages.push(Message::user(content));
    }

    pub fn push_context(&mut self, content: &str) {
        self.messages.push(Message::user(content));
    }

    pub fn push_assistant(&mut self, response: &ChatResponse) {
        self.messages.push(Message::assistant_with_tool_calls(
            response.content.clone(),
            response.tool_calls.clone(),
        ));
    }

    pub fn push_tool_results(&mut self, results: Vec<ToolResult>) {
        for result in results {
            let content = if result.content.len() > 8192 {
                let original_len = result.content.len();
                // Find a valid char boundary at or before 8192
                let boundary = result.content[..8192.min(result.content.len())]
                    .char_indices()
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                let mut truncated = result.content[..boundary].to_string();
                truncated.push_str(&format!(
                    "\n\n[Truncated: result was {original_len} \
                     bytes, showing first {boundary}]"
                ));
                truncated
            } else {
                result.content
            };

            if result.is_success {
                self.messages
                    .push(Message::tool_result_success(result.call_id, content));
            } else {
                self.messages
                    .push(Message::tool_result_error(result.call_id, content));
            }
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn last_assistant_text(&self) -> String {
        self.messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Assistant { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.messages.truncate(1);
        self.turn_input_tokens = 0;
        self.turn_output_tokens = 0;
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;
        self.turn_input_tokens += input_tokens;
        self.turn_output_tokens += output_tokens;
    }

    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens
    }

    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens
    }

    pub fn total_tokens_this_turn(&self) -> u64 {
        self.turn_input_tokens + self.turn_output_tokens
    }

    pub fn save(&self) -> Result<(), ConversationError> {
        let dir = Path::new(SESSION_DIR);
        std::fs::create_dir_all(dir).map_err(|e| ConversationError::WriteError {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let path = dir.join(format!("{}.json", self.session_id));
        let tmp_path = dir.join(format!("{}.json.tmp", self.session_id));

        let json = serde_json::to_string_pretty(self).map_err(|e| {
            ConversationError::WriteError {
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::Other, e),
            }
        })?;

        std::fs::write(&tmp_path, json.as_bytes()).map_err(|e| ConversationError::WriteError {
            path: tmp_path.clone(),
            source: e,
        })?;

        std::fs::rename(&tmp_path, &path).map_err(|e| ConversationError::WriteError {
            path: path.clone(),
            source: e,
        })?;

        tracing::debug!(
            session_id = %self.session_id,
            messages = self.messages.len(),
            "Session saved"
        );

        Ok(())
    }

    pub fn load(session_id: Uuid) -> Result<Self, ConversationError> {
        let path = Path::new(SESSION_DIR).join(format!("{}.json", session_id));

        let contents = std::fs::read_to_string(&path).map_err(|e| ConversationError::ReadError {
            path: path.clone(),
            source: e,
        })?;

        let manager: Self =
            serde_json::from_str(&contents).map_err(|e| ConversationError::ParseError {
                path: path.clone(),
                source: e,
            })?;

        tracing::debug!(
            session_id = %session_id,
            messages = manager.messages.len(),
            "Session loaded"
        );

        Ok(manager)
    }

    pub fn list_sessions() -> Vec<Uuid> {
        let dir = Path::new(SESSION_DIR);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name();
                let name = name.to_str()?;
                if name.ends_with(".json") && !name.ends_with(".tmp") {
                    let stem = name.strip_suffix(".json")?;
                    Uuid::parse_str(stem).ok()
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn delete_session(session_id: Uuid) -> anyhow::Result<()> {
        let path = Path::new(SESSION_DIR).join(format!("{}.json", session_id));
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn push_assistant_raw(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn push_tool_result_raw(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn compact_with_summary(&mut self, summary: &str) {
        let system_message = self.messages[0].clone();
        let summary_message = Message::user(format!(
            "Here is a summary of what you have learned so far \
             from this source:\n\n{summary}\n\nContinue learning \
             from the next section."
        ));

        self.messages = vec![system_message, summary_message];
        self.turn_input_tokens = 0;
        self.turn_output_tokens = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> ConversationManager {
        ConversationManager::new("You are a test assistant.".to_string(), Uuid::new_v4())
    }

    #[test]
    fn test_new_conversation_has_system_message() {
        let mgr = make_manager();
        assert_eq!(mgr.messages().len(), 1);
        assert!(
            matches!(&mgr.messages()[0], Message::System { content } if content == "You are a test assistant.")
        );
    }

    #[test]
    fn test_push_user() {
        let mut mgr = make_manager();
        mgr.push_user("Hello");
        assert_eq!(mgr.messages().len(), 2);
        assert!(matches!(&mgr.messages()[1], Message::User { content } if content == "Hello"));
    }

    #[test]
    fn test_clear_resets_to_system_prompt() {
        let mut mgr = make_manager();
        mgr.push_user("Hello");
        mgr.push_user("World");
        assert_eq!(mgr.messages().len(), 3);
        mgr.clear();
        assert_eq!(mgr.messages().len(), 1);
        assert!(matches!(&mgr.messages()[0], Message::System { .. }));
    }

    #[test]
    fn test_token_tracking() {
        let mut mgr = make_manager();
        mgr.push_user("test");
        mgr.record_usage(100, 50);
        mgr.record_usage(200, 80);
        assert_eq!(mgr.total_input_tokens(), 300);
        assert_eq!(mgr.total_output_tokens(), 130);
        assert_eq!(mgr.total_tokens(), 430);
        assert_eq!(mgr.total_tokens_this_turn(), 430);
    }

    #[test]
    fn test_turn_tokens_reset_on_push_user() {
        let mut mgr = make_manager();
        mgr.push_user("first question");
        mgr.record_usage(100, 50);
        assert_eq!(mgr.total_tokens_this_turn(), 150);

        mgr.push_user("second question");
        assert_eq!(mgr.total_tokens_this_turn(), 0);
        assert_eq!(mgr.total_tokens(), 150);
    }

    #[test]
    fn test_tool_result_truncation() {
        let mut mgr = make_manager();
        let long_content = "x".repeat(10000);
        let results = vec![ToolResult::success("call_1".to_string(), long_content)];
        mgr.push_tool_results(results);
        match &mgr.messages()[1] {
            Message::ToolResult { content, .. } => {
                assert!(content.len() < 8500);
                assert!(content.contains("[Truncated"));
            }
            _ => panic!("Expected ToolResult message"),
        }
    }

    #[test]
    fn test_tool_result_short_not_truncated() {
        let mut mgr = make_manager();
        let short_content = "short result".to_string();
        let results = vec![ToolResult::success(
            "call_1".to_string(),
            short_content.clone(),
        )];
        mgr.push_tool_results(results);
        match &mgr.messages()[1] {
            Message::ToolResult { content, .. } => {
                assert_eq!(content, &short_content);
            }
            _ => panic!("Expected ToolResult message"),
        }
    }

    #[test]
    fn test_last_assistant_text_empty() {
        let mgr = make_manager();
        assert_eq!(mgr.last_assistant_text(), "");
    }

    #[test]
    fn test_compact_with_summary() {
        let mut mgr = make_manager();
        mgr.push_user("Section 1 content...");
        mgr.record_usage(500, 200);
        mgr.push_user("Section 2 content...");
        mgr.record_usage(500, 200);
        assert_eq!(mgr.messages().len(), 3);
        assert_eq!(mgr.total_tokens(), 1400);

        mgr.compact_with_summary("Learned about deployment and testing.");

        assert_eq!(mgr.messages().len(), 2);
        assert!(matches!(&mgr.messages()[0], Message::System { .. }));
        assert!(
            matches!(&mgr.messages()[1], Message::User { content } if content.contains("deployment and testing"))
        );
        assert_eq!(mgr.total_tokens(), 1400);
    }

    #[test]
    fn test_session_persistence_round_trip() {
        let mgr = make_manager();
        let json = serde_json::to_string(&mgr).unwrap();
        let loaded: ConversationManager = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.session_id, mgr.session_id);
        assert_eq!(loaded.messages().len(), mgr.messages().len());
        assert_eq!(loaded.total_input_tokens(), mgr.total_input_tokens());
    }
}
