mod engine;
mod error;
mod hook;
mod model;
mod path;

pub use engine::RuleEngine;
pub use error::CoreError;
pub use hook::{
    AdapterCapabilities, AdapterIdentity, AdapterKind, CapabilitySupport, ContextItem,
    EventIdentityQuality, FeedbackItem, FeedbackSeverity, GateDecision, HOOK_PROTOCOL_VERSION,
    HookEventKind, HookEventPayload, HookOutcomePayload, IdempotencyMetadata, IntentDeclared,
    IntentOrigin, InternalHookEvent, InternalHookOutcome, SessionOpenReason, SessionOpened,
    StopDecision, TaskStopping, ToolAboutToRun, ToolAction, ToolFinished, ToolStatus,
};
pub use model::{
    ActionDescriptor, ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, Decision,
    DecisionKind, Evidence, MemoryStatus, Rule, RuleEffect, RuleStrength, StopReconcileConfig,
};
pub use path::{normalize_project_path, path_has_prefix};
