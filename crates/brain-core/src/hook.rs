use serde::{Deserialize, Serialize};

use crate::{ActionKind, CoreError};

pub const HOOK_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalHookEvent {
    pub protocol_version: u16,
    pub project_key: String,
    pub event_id: String,
    pub idempotency: IdempotencyMetadata,
    pub adapter: AdapterIdentity,
    pub session_key: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_key: Option<String>,
    pub payload: HookEventPayload,
}

impl InternalHookEvent {
    /// 验证内部 Hook 信封及事件特定的必填字段。
    ///
    /// # Errors
    ///
    /// 协议版本、项目/事件/会话身份或工具因果键无效时返回错误。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.protocol_version != HOOK_PROTOCOL_VERSION {
            return Err(CoreError::UnsupportedHookProtocolVersion {
                actual: self.protocol_version,
                expected: HOOK_PROTOCOL_VERSION,
            });
        }
        validate_identity("project_key", &self.project_key)?;
        validate_identity("event_id", &self.event_id)?;
        validate_identity("session_key", &self.session_key)?;
        if self.adapter.adapter_version == 0 {
            return Err(CoreError::InvalidHookEvent(
                "adapter_version 必须大于 0".to_owned(),
            ));
        }
        match &self.payload {
            HookEventPayload::ToolAboutToRun(tool) => tool.validate(),
            HookEventPayload::ToolFinished(tool) => {
                validate_identity("operation_id", &tool.operation_id)?;
                validate_identity("tool_name", &tool.tool_name)
            }
            HookEventPayload::SessionOpened(_)
            | HookEventPayload::IntentDeclared(_)
            | HookEventPayload::TaskStopping(_) => Ok(()),
        }
    }

    pub const fn kind(&self) -> HookEventKind {
        match self.payload {
            HookEventPayload::SessionOpened(_) => HookEventKind::SessionOpened,
            HookEventPayload::IntentDeclared(_) => HookEventKind::IntentDeclared,
            HookEventPayload::ToolAboutToRun(_) => HookEventKind::ToolAboutToRun,
            HookEventPayload::ToolFinished(_) => HookEventKind::ToolFinished,
            HookEventPayload::TaskStopping(_) => HookEventKind::TaskStopping,
        }
    }
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), CoreError> {
    if value.trim().is_empty() || value.len() > 256 || value.contains(['\0', '\n', '\r']) {
        return Err(CoreError::InvalidHookEvent(format!(
            "{field} 不能为空、超过 256 字节或包含控制换行"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookEventKind {
    SessionOpened,
    IntentDeclared,
    ToolAboutToRun,
    ToolFinished,
    TaskStopping,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdempotencyMetadata {
    pub identity_quality: EventIdentityQuality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventIdentityQuality {
    VendorStable,
    DerivedStable,
    PerDelivery,
}

impl HookEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionOpened => "session_opened",
            Self::IntentDeclared => "intent_declared",
            Self::ToolAboutToRun => "tool_about_to_run",
            Self::ToolFinished => "tool_finished",
            Self::TaskStopping => "task_stopping",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    Codex,
    ClaudeCode,
    PrimeAgent,
}

impl AdapterKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::PrimeAgent => "prime_agent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterIdentity {
    pub kind: AdapterKind,
    pub adapter_version: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub deny_intent: CapabilitySupport,
    pub deny_tool: CapabilitySupport,
    pub inject_context: CapabilitySupport,
    pub post_feedback: CapabilitySupport,
    pub continue_after_stop: CapabilitySupport,
}

impl AdapterCapabilities {
    pub const fn codex() -> Self {
        Self {
            deny_intent: CapabilitySupport::Unsupported,
            deny_tool: CapabilitySupport::Supported,
            inject_context: CapabilitySupport::Supported,
            post_feedback: CapabilitySupport::Supported,
            continue_after_stop: CapabilitySupport::Supported,
        }
    }

    pub const fn claude_code() -> Self {
        Self {
            deny_intent: CapabilitySupport::Supported,
            deny_tool: CapabilitySupport::Supported,
            inject_context: CapabilitySupport::Supported,
            post_feedback: CapabilitySupport::Supported,
            continue_after_stop: CapabilitySupport::Supported,
        }
    }

    pub const fn prime_agent() -> Self {
        Self {
            deny_intent: CapabilitySupport::Supported,
            deny_tool: CapabilitySupport::Supported,
            inject_context: CapabilitySupport::Supported,
            post_feedback: CapabilitySupport::Supported,
            continue_after_stop: CapabilitySupport::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum HookEventPayload {
    SessionOpened(SessionOpened),
    IntentDeclared(IntentDeclared),
    ToolAboutToRun(ToolAboutToRun),
    ToolFinished(ToolFinished),
    TaskStopping(TaskStopping),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOpened {
    pub reason: SessionOpenReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_session_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionOpenReason {
    Startup,
    New,
    Resume,
    Clear,
    Compact,
    Fork,
    Reload,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentDeclared {
    pub text: String,
    pub origin: IntentOrigin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOrigin {
    Interactive,
    Rpc,
    Extension,
    RuntimeContinuation,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAction {
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// 只有 adapter 能从结构化工具输入确定影响范围时才填写。shell 文本和无法
    /// 唯一定位的 patch/edit 保持为空，因此不得获得符号级硬门控资格。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deterministic_impacts: Vec<ToolImpact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolImpact {
    pub path: String,
    #[serde(default)]
    pub whole_file: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<ToolLineRange>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolLineRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAboutToRun {
    pub operation_id: String,
    pub tool_name: String,
    pub action: ToolAction,
}

impl ToolAboutToRun {
    fn validate(&self) -> Result<(), CoreError> {
        validate_identity("operation_id", &self.operation_id)?;
        validate_identity("tool_name", &self.tool_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolFinished {
    pub operation_id: String,
    pub tool_name: String,
    pub action: ToolAction,
    pub status: ToolStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskStopping {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub vendor_loop_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalHookOutcome {
    pub protocol_version: u16,
    pub event_id: String,
    pub payload: HookOutcomePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum HookOutcomePayload {
    SessionOpened {
        inject: Vec<ContextItem>,
    },
    IntentDeclared {
        gate: GateDecision,
        inject: Vec<ContextItem>,
    },
    ToolAboutToRun {
        gate: GateDecision,
        inject: Vec<ContextItem>,
    },
    ToolFinished {
        feedback: Vec<FeedbackItem>,
    },
    TaskStopping {
        stop: StopDecision,
        feedback: Vec<FeedbackItem>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateDecision {
    NoVeto,
    Deny { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum StopDecision {
    AllowStop,
    ContinueWork { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextItem {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackItem {
    pub severity: FeedbackSeverity,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSeverity {
    Info,
    Warning,
    Error,
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterCapabilities, AdapterIdentity, AdapterKind, CapabilitySupport,
        HOOK_PROTOCOL_VERSION, HookEventPayload, InternalHookEvent, SessionOpenReason,
        SessionOpened,
    };

    #[test]
    fn project_key_is_required_by_the_internal_protocol() {
        let event = InternalHookEvent {
            protocol_version: HOOK_PROTOCOL_VERSION,
            project_key: String::new(),
            event_id: "event-1".to_owned(),
            idempotency: super::IdempotencyMetadata {
                identity_quality: super::EventIdentityQuality::VendorStable,
            },
            adapter: AdapterIdentity {
                kind: AdapterKind::Codex,
                adapter_version: 1,
            },
            session_key: "session-1".to_owned(),
            cwd: "/repo".to_owned(),
            turn_key: None,
            payload: HookEventPayload::SessionOpened(SessionOpened {
                reason: SessionOpenReason::Startup,
                previous_session_key: None,
            }),
        };
        assert!(event.validate().is_err());
    }

    #[test]
    fn prime_does_not_claim_stop_continuation_support() {
        assert_eq!(
            AdapterCapabilities::prime_agent().continue_after_stop,
            CapabilitySupport::Unsupported
        );
        assert_eq!(
            AdapterCapabilities::codex().continue_after_stop,
            CapabilitySupport::Supported
        );
        assert_eq!(
            AdapterCapabilities::codex().deny_intent,
            CapabilitySupport::Unsupported
        );
    }
}
