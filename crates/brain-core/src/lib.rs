mod engine;
mod error;
mod model;
mod path;

pub use engine::RuleEngine;
pub use error::CoreError;
pub use model::{
    ActionDescriptor, ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, Decision,
    DecisionKind, Evidence, MemoryStatus, Rule, RuleEffect, RuleStrength,
};
pub use path::{normalize_project_path, path_has_prefix};
