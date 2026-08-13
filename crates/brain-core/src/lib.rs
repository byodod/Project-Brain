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
    StopDecision, TaskStopping, ToolAboutToRun, ToolAction, ToolFinished, ToolImpact,
    ToolLineRange, ToolStatus,
};
pub use model::{
    ActionDescriptor, ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, Decision,
    DecisionKind, Evidence, EvidenceGrade, MemoryStatus, ProjectLanguageProfile, Rule, RuleEffect,
    RuleStrength, RuleSymbolScope, SemanticLanguageMapping, SemanticProviderFormat,
    SemanticProviderProfile, StopReconcileConfig, SymbolResolutionPolicy,
};
pub use path::{normalize_project_path, path_has_prefix};
