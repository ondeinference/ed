//! UniFFI surface for Ed's Swift package.

use std::sync::Arc;

use async_trait::async_trait;
use ed_agent::{
    AgentConfig, AgentError, AgentReply, AgentToolDefinition, ApprovalDecision, ApprovalHandler,
    ApprovalRequest, ChatMessage, Ed, EngineInfo, EngineStatus, EventSink, GgufModelConfig,
    SamplingConfig, ToolCall, ToolExecutionResult, ToolExecutor, ToolRisk, UqffModelConfig,
};

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentConfig {
    pub max_tool_rounds: u8,
    pub max_tool_output_chars: u64,
}

impl Default for FfiAgentConfig {
    fn default() -> Self {
        let config = AgentConfig::default();
        Self {
            max_tool_rounds: config.max_tool_rounds,
            max_tool_output_chars: config.max_tool_output_chars as u64,
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiToolRisk {
    ReadOnly,
    Mutating,
}

impl From<FfiToolRisk> for ToolRisk {
    fn from(value: FfiToolRisk) -> Self {
        match value {
            FfiToolRisk::ReadOnly => Self::ReadOnly,
            FfiToolRisk::Mutating => Self::Mutating,
        }
    }
}

impl From<ToolRisk> for FfiToolRisk {
    fn from(value: ToolRisk) -> Self {
        match value {
            ToolRisk::ReadOnly => Self::ReadOnly,
            ToolRisk::Mutating => Self::Mutating,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: String,
    pub risk: FfiToolRisk,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl From<&ToolCall> for FfiToolCall {
    fn from(value: &ToolCall) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            arguments: value.arguments.clone(),
        }
    }
}

/// A tool call waiting on the user, with the risk that made it wait.
///
/// The risk travels with the call because it is the reason an approval sheet
/// is on screen at all: a host that only received the call would have to look
/// the tool up again to tell the user whether it is about to change anything.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiApprovalRequest {
    pub call: FfiToolCall,
    pub risk: FfiToolRisk,
}

impl From<&ApprovalRequest> for FfiApprovalRequest {
    fn from(value: &ApprovalRequest) -> Self {
        Self {
            call: (&value.call).into(),
            risk: value.risk.into(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiToolOutput {
    pub content: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiApprovalDecision {
    AllowOnce,
    AllowForSession,
    Deny,
}

impl From<FfiApprovalDecision> for ApprovalDecision {
    fn from(value: FfiApprovalDecision) -> Self {
        match value {
            FfiApprovalDecision::AllowOnce => Self::AllowOnce,
            FfiApprovalDecision::AllowForSession => Self::AllowForSession,
            FfiApprovalDecision::Deny => Self::Deny,
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiEngineStatus {
    Unloaded,
    Loading,
    Ready,
    Generating,
    Error,
}

impl From<EngineStatus> for FfiEngineStatus {
    fn from(value: EngineStatus) -> Self {
        match value {
            EngineStatus::Unloaded => Self::Unloaded,
            EngineStatus::Loading => Self::Loading,
            EngineStatus::Ready => Self::Ready,
            EngineStatus::Generating => Self::Generating,
            EngineStatus::Error => Self::Error,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStatusUpdate {
    pub status: FfiEngineStatus,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgentReply {
    pub text: String,
    pub duration_seconds: f64,
    pub duration: String,
    pub tool_rounds: u8,
}

impl From<&AgentReply> for FfiAgentReply {
    fn from(value: &AgentReply) -> Self {
        Self {
            text: value.text.clone(),
            duration_seconds: value.duration_seconds,
            duration: value.duration.clone(),
            tool_rounds: value.tool_rounds,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiReply {
    pub text: String,
    pub duration: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiEngineInfo {
    pub status: FfiEngineStatus,
    pub model_name: Option<String>,
    pub approx_memory: Option<String>,
    pub history_length: u64,
}

impl From<EngineInfo> for FfiEngineInfo {
    fn from(value: EngineInfo) -> Self {
        Self {
            status: value.status.into(),
            model_name: value.model_name,
            approx_memory: value.approx_memory,
            history_length: value.history_length,
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiChatMessage {
    pub role: FfiChatRole,
    pub content: String,
}

impl From<ChatMessage> for FfiChatMessage {
    fn from(value: ChatMessage) -> Self {
        let role = match value.role {
            ed_agent::ChatRole::System => FfiChatRole::System,
            ed_agent::ChatRole::User => FfiChatRole::User,
            ed_agent::ChatRole::Assistant => FfiChatRole::Assistant,
        };
        Self {
            role,
            content: value.content,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSamplingConfig {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub min_p: Option<f64>,
    pub max_tokens: Option<u64>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
}

impl From<FfiSamplingConfig> for SamplingConfig {
    fn from(value: FfiSamplingConfig) -> Self {
        Self {
            temperature: value.temperature,
            top_p: value.top_p,
            top_k: value.top_k,
            min_p: value.min_p,
            max_tokens: value.max_tokens,
            frequency_penalty: value.frequency_penalty,
            presence_penalty: value.presence_penalty,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiGgufModelConfig {
    pub model_id: String,
    pub files: Vec<String>,
    pub tok_model_id: Option<String>,
    pub display_name: String,
    pub approx_memory: String,
    pub chat_template: Option<String>,
}

impl From<FfiGgufModelConfig> for GgufModelConfig {
    fn from(value: FfiGgufModelConfig) -> Self {
        Self {
            model_id: value.model_id,
            files: value.files,
            tok_model_id: value.tok_model_id,
            display_name: value.display_name,
            approx_memory: value.approx_memory,
            chat_template: value.chat_template,
        }
    }
}

impl From<GgufModelConfig> for FfiGgufModelConfig {
    fn from(value: GgufModelConfig) -> Self {
        Self {
            model_id: value.model_id,
            files: value.files,
            tok_model_id: value.tok_model_id,
            display_name: value.display_name,
            approx_memory: value.approx_memory,
            chat_template: value.chat_template,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUqffModelConfig {
    pub model_id: String,
    pub files: Vec<String>,
    pub display_name: String,
    pub approx_memory: String,
    pub chat_template: Option<String>,
}

impl From<FfiUqffModelConfig> for UqffModelConfig {
    fn from(value: FfiUqffModelConfig) -> Self {
        Self {
            model_id: value.model_id,
            files: value.files,
            display_name: value.display_name,
            approx_memory: value.approx_memory,
            chat_template: value.chat_template,
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiEnvironment {
    Development,
    Production,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiEdError {
    #[error("{reason}")]
    Failure { reason: String },
}

impl From<AgentError> for FfiEdError {
    fn from(value: AgentError) -> Self {
        Self::Failure {
            reason: value.to_string(),
        }
    }
}

#[uniffi::export(callback_interface)]
#[async_trait]
pub trait FfiToolExecutor: Send + Sync {
    async fn execute(&self, tool_name: String, arguments: String) -> FfiToolOutput;
}

#[uniffi::export(callback_interface)]
#[async_trait]
pub trait FfiApprovalHandler: Send + Sync {
    async fn approve(&self, request: FfiApprovalRequest) -> FfiApprovalDecision;
}

#[uniffi::export(callback_interface)]
pub trait FfiEventListener: Send + Sync {
    fn status_changed(&self, update: FfiStatusUpdate);
    fn tool_requested(&self, call: FfiToolCall);
    fn approval_requested(&self, request: FfiApprovalRequest);
    fn tool_started(&self, call: FfiToolCall);
    fn tool_finished(&self, tool_call_id: String, content: String, is_error: bool);
    fn agent_replied(&self, reply: FfiAgentReply);
    fn warning(&self, message: String);
}

struct ForeignExecutor(Arc<dyn FfiToolExecutor>);

#[async_trait]
impl ToolExecutor for ForeignExecutor {
    async fn execute(&self, tool_name: &str, arguments: &str) -> Result<String, String> {
        let output = self
            .0
            .execute(tool_name.to_owned(), arguments.to_owned())
            .await;
        match output.error {
            Some(error) => Err(error),
            None => Ok(output.content),
        }
    }
}

struct ForeignApprovals(Arc<dyn FfiApprovalHandler>);

#[async_trait]
impl ApprovalHandler for ForeignApprovals {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalDecision {
        self.0.approve((&request).into()).await.into()
    }
}

#[derive(Clone)]
struct FfiSink(Arc<dyn FfiEventListener>);

impl EventSink for FfiSink {
    fn status_changed(&self, status: EngineStatus, model_name: Option<&str>, error: Option<&str>) {
        self.0.status_changed(FfiStatusUpdate {
            status: status.into(),
            model_name: model_name.map(str::to_owned),
            error: error.map(str::to_owned),
        });
    }

    fn tool_requested(&self, call: &ToolCall) {
        self.0.tool_requested(call.into());
    }

    fn approval_requested(&self, request: &ApprovalRequest) {
        self.0.approval_requested(request.into());
    }

    fn tool_started(&self, call: &ToolCall) {
        self.0.tool_started(call.into());
    }

    fn tool_finished(&self, result: &ToolExecutionResult) {
        self.0.tool_finished(
            result.tool_call_id.clone(),
            result.content.clone(),
            result.is_error,
        );
    }

    fn agent_replied(&self, reply: &AgentReply) {
        self.0.agent_replied(reply.into());
    }

    fn warning(&self, message: &str) {
        self.0.warning(message.to_owned());
    }
}

#[derive(uniffi::Object)]
pub struct FfiEdAgent {
    inner: Ed<FfiSink>,
}

#[uniffi::export(async_runtime = "tokio")]
impl FfiEdAgent {
    #[uniffi::constructor]
    pub fn new(
        executor: Box<dyn FfiToolExecutor>,
        approvals: Box<dyn FfiApprovalHandler>,
        events: Box<dyn FfiEventListener>,
        config: Option<FfiAgentConfig>,
    ) -> Arc<Self> {
        let config = config.unwrap_or_default();
        Arc::new(Self {
            inner: Ed::with_agent(
                FfiSink(Arc::from(events)),
                Arc::new(ForeignExecutor(Arc::from(executor))),
                Arc::new(ForeignApprovals(Arc::from(approvals))),
                AgentConfig {
                    max_tool_rounds: config.max_tool_rounds,
                    max_tool_output_chars: usize::try_from(config.max_tool_output_chars)
                        .unwrap_or(usize::MAX),
                },
            ),
        })
    }

    pub async fn register_tool(&self, tool: FfiToolDefinition) -> Result<(), FfiEdError> {
        self.inner
            .register_tool(AgentToolDefinition {
                name: tool.name,
                description: tool.description,
                parameters_schema: tool.parameters_schema,
                risk: tool.risk.into(),
            })
            .await
            .map_err(Into::into)
    }

    pub async fn unregister_tool(&self, name: String) -> bool {
        self.inner.unregister_tool(&name).await
    }

    pub async fn remove_all_tools(&self) {
        self.inner.remove_all_tools().await;
    }

    pub async fn load_default_agent_model(
        &self,
        system_prompt: Option<String>,
    ) -> Result<f64, FfiEdError> {
        let config: GgufModelConfig = default_agent_model_config().into();
        let sampling = SamplingConfig {
            max_tokens: Some(4096),
            ..SamplingConfig::default()
        };
        self.inner
            .load(config, system_prompt, Some(sampling))
            .await
            .map(|duration| duration.as_secs_f64())
            .map_err(|error| FfiEdError::Failure {
                reason: error.to_string(),
            })
    }

    pub async fn load_gguf_model(
        &self,
        config: FfiGgufModelConfig,
        system_prompt: Option<String>,
        sampling: Option<FfiSamplingConfig>,
    ) -> Result<f64, FfiEdError> {
        self.inner
            .load(config.into(), system_prompt, sampling.map(Into::into))
            .await
            .map(|duration| duration.as_secs_f64())
            .map_err(|error| FfiEdError::Failure {
                reason: error.to_string(),
            })
    }

    pub async fn load_uqff_model(
        &self,
        config: FfiUqffModelConfig,
        system_prompt: Option<String>,
        sampling: Option<FfiSamplingConfig>,
    ) -> Result<f64, FfiEdError> {
        let config: UqffModelConfig = config.into();
        let name = config.display_name.clone();
        self.inner
            .load_with(&name, |engine| {
                engine.load_uqff_model(config, system_prompt, sampling.map(Into::into))
            })
            .await
            .map(|duration| duration.as_secs_f64())
            .map_err(|error| FfiEdError::Failure {
                reason: error.to_string(),
            })
    }

    pub async fn load_assigned_model(
        &self,
        environment: FfiEnvironment,
        app_id: String,
        app_secret: String,
        system_prompt: Option<String>,
        sampling: Option<FfiSamplingConfig>,
    ) -> Result<f64, FfiEdError> {
        let environment = match environment {
            FfiEnvironment::Development => smbcloud_gresiq_sdk::Environment::Dev,
            FfiEnvironment::Production => smbcloud_gresiq_sdk::Environment::Production,
        };
        self.inner
            .load_with("Assigned model", |engine| {
                engine.load_assigned_model(
                    environment,
                    &app_id,
                    &app_secret,
                    system_prompt,
                    sampling.map(Into::into),
                )
            })
            .await
            .map(|duration| duration.as_secs_f64())
            .map_err(|error| FfiEdError::Failure {
                reason: error.to_string(),
            })
    }

    pub async fn run(&self, message: String) -> Result<FfiAgentReply, FfiEdError> {
        self.inner
            .run(message)
            .await
            .map(|reply| (&reply).into())
            .map_err(Into::into)
    }

    pub async fn send(&self, message: String) -> Result<FfiReply, FfiEdError> {
        let reply = self.inner.send(message).await;
        match (reply.reply, reply.duration, reply.error) {
            (Some(text), Some(duration), None) => Ok(FfiReply { text, duration }),
            (_, _, Some(reason)) => Err(FfiEdError::Failure { reason }),
            _ => Err(FfiEdError::Failure {
                reason: "the model returned an empty reply".to_string(),
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub async fn unload(&self) -> Option<String> {
        self.inner.unload().await
    }

    pub async fn info(&self) -> FfiEngineInfo {
        self.inner.info().await.into()
    }

    pub async fn is_loaded(&self) -> bool {
        self.inner.is_loaded().await
    }

    pub async fn history(&self) -> Vec<FfiChatMessage> {
        self.inner
            .history()
            .await
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub async fn clear_history(&self) -> u64 {
        self.inner.clear_history().await as u64
    }
}

#[uniffi::export]
pub fn configure_cache_dir(path: String) {
    onde::inference::ffi::configure_cache_dir(path);
}

#[uniffi::export]
pub fn default_agent_model_config() -> FfiGgufModelConfig {
    #[cfg(any(target_os = "tvos", target_os = "watchos"))]
    let config = GgufModelConfig::qwen3_0_6b();
    #[cfg(target_os = "ios")]
    let config = GgufModelConfig::qwen3_1_7b();
    #[cfg(any(target_os = "macos", target_os = "visionos"))]
    let config = GgufModelConfig::qwen3_4b();
    #[cfg(not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    )))]
    let config = GgufModelConfig::qwen3_1_7b();
    config.into()
}

uniffi::setup_scaffolding!();
