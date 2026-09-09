//! Host-defined tools and the policies that guard them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A tool the model may ask the host application to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema encoded as a string.
    pub parameters_schema: String,
    pub risk: ToolRisk,
}

impl AgentToolDefinition {
    pub fn read_only(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema: parameters_schema.into(),
            risk: ToolRisk::ReadOnly,
        }
    }

    pub fn mutating(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema: parameters_schema.into(),
            risk: ToolRisk::Mutating,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), AgentError> {
        let mut chars = self.name.chars();
        let valid_first = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
        let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid_first || !valid_rest || self.name.len() > 64 {
            return Err(AgentError::InvalidTool {
                reason: format!(
                    "tool name {:?} must start with an ASCII letter and contain only letters, numbers, '_' or '-' (maximum 64 bytes)",
                    self.name
                ),
            });
        }
        if self.description.trim().is_empty() {
            return Err(AgentError::InvalidTool {
                reason: format!("tool {:?} needs a description", self.name),
            });
        }
        let schema: serde_json::Value =
            serde_json::from_str(&self.parameters_schema).map_err(|error| {
                AgentError::InvalidTool {
                    reason: format!("tool {:?} has invalid JSON Schema: {error}", self.name),
                }
            })?;
        if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
            return Err(AgentError::InvalidTool {
                reason: format!("tool {:?} schema must have type object", self.name),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolRisk {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub call: ToolCall,
    pub risk: ToolRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowForSession,
    Deny,
}

#[async_trait]
pub trait ToolExecutor: Send + Sync + 'static {
    async fn execute(&self, tool_name: &str, arguments: &str) -> Result<String, String>;
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync + 'static {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision;
}

#[derive(Debug, Default)]
pub struct RejectingExecutor;

#[async_trait]
impl ToolExecutor for RejectingExecutor {
    async fn execute(&self, tool_name: &str, _: &str) -> Result<String, String> {
        Err(format!("no executor is registered for tool {tool_name:?}"))
    }
}

#[derive(Debug, Default)]
pub struct DenyApprovals;

#[async_trait]
impl ApprovalHandler for DenyApprovals {
    async fn approve(&self, _: ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Deny
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub max_tool_rounds: u8,
    pub max_tool_output_chars: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tool_rounds: 8,
            max_tool_output_chars: 10_000,
        }
    }
}

impl AgentConfig {
    pub(crate) fn validated(self) -> Self {
        Self {
            max_tool_rounds: self.max_tool_rounds.clamp(1, 24),
            max_tool_output_chars: self.max_tool_output_chars.max(1),
        }
    }

    /// Whether another tool round may run, given how many already have.
    ///
    /// Asked twice per round, and the pair is what makes the loop terminate:
    /// once to decide whether to offer tools to the model again, and once at
    /// the top of the next round to decide whether to run it at all. If only
    /// the first check existed, a model that requested tools it was never
    /// offered would keep the loop alive indefinitely.
    pub(crate) fn allows_round(&self, rounds: u8) -> bool {
        rounds < self.max_tool_rounds
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReply {
    pub text: String,
    pub duration_seconds: f64,
    pub duration: String,
    pub tool_rounds: u8,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AgentError {
    #[error("invalid tool: {reason}")]
    InvalidTool { reason: String },
    #[error("a tool named {name:?} is already registered")]
    DuplicateTool { name: String },
    #[error("the loaded model does not support tool calling")]
    UnsupportedModel,
    #[error("the agent turn was cancelled")]
    Cancelled,
    #[error("the model returned an empty final reply")]
    EmptyReply,
    #[error("the agent stopped after {rounds} tool rounds without a final reply")]
    ToolRoundsExhausted { rounds: u8 },
    #[error("inference failed: {reason}")]
    Inference { reason: String },
}

pub(crate) fn truncate_output(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut truncated: String = value.chars().take(max_chars).collect();
    truncated.push_str("\n[tool output truncated by Ed]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tool_names_and_object_schemas() {
        AgentToolDefinition::read_only("clock", "Read the clock", r#"{"type":"object"}"#)
            .validate()
            .unwrap();
        assert!(
            AgentToolDefinition::read_only("bad name", "Read", r#"{"type":"object"}"#)
                .validate()
                .is_err()
        );
        assert!(
            AgentToolDefinition::read_only("clock", "Read", r#"{"type":"array"}"#)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn a_validated_budget_always_allows_at_least_one_round() {
        // A zero budget would mean registering tools quietly disabled them:
        // the model would be offered tools it could never be allowed to use.
        let config = AgentConfig {
            max_tool_rounds: 0,
            max_tool_output_chars: 0,
        }
        .validated();

        assert!(config.allows_round(0));
        assert!(!config.allows_round(1));
        assert_eq!(config.max_tool_output_chars, 1);
    }

    #[test]
    fn the_round_budget_closes_at_the_limit_and_stays_closed() {
        let config = AgentConfig {
            max_tool_rounds: 3,
            max_tool_output_chars: 100,
        }
        .validated();

        assert!(config.allows_round(2));
        assert!(!config.allows_round(3));
        // Past the limit too: `rounds` is only ever compared, never trusted to
        // land exactly on the boundary.
        assert!(!config.allows_round(u8::MAX));
    }

    #[test]
    fn truncation_is_character_safe_and_explicit() {
        assert_eq!(
            truncate_output("héllo".into(), 2),
            "hé\n[tool output truncated by Ed]"
        );
    }
}
