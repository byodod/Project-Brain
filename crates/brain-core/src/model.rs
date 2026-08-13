use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::CoreError;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrainConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub stop_reconcile: StopReconcileConfig,
}

impl BrainConfig {
    /// 验证配置 schema 和其中每一条规则的权限边界。
    ///
    /// # Errors
    ///
    /// 当 schema 版本不受支持，或任一规则无效时返回错误。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchemaVersion {
                actual: self.schema_version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }

        for rule in &self.rules {
            rule.validate()?;
        }
        if self.stop_reconcile.enabled {
            if self.stop_reconcile.base.trim().is_empty() {
                return Err(CoreError::InvalidStopReconcile("base 不能为空".to_owned()));
            }
            if self.stop_reconcile.envelope.trim().is_empty() {
                return Err(CoreError::InvalidStopReconcile(
                    "envelope 不能为空".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StopReconcileConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_reconcile_base")]
    pub base: String,
    #[serde(default = "default_envelope_path")]
    pub envelope: String,
}

impl Default for StopReconcileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base: default_reconcile_base(),
            envelope: default_envelope_path(),
        }
    }
}

fn default_reconcile_base() -> String {
    "HEAD".to_owned()
}

fn default_envelope_path() -> String {
    ".project-brain/envelope.json".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    #[serde(default = "default_active")]
    pub status: MemoryStatus,
    pub authority: Authority,
    pub strength: RuleStrength,
    pub effect: RuleEffect,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub actions: Vec<ActionKind>,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub operation_contains: Vec<String>,
    pub message: String,
    #[serde(default)]
    pub rationale: String,
}

impl Rule {
    /// 验证单条规则是否能安全进入确定性 Runtime。
    ///
    /// # Errors
    ///
    /// 当规则缺少标识或消息，或非法获得阻断权限时返回错误。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.id.trim().is_empty() {
            return Err(CoreError::InvalidRule {
                rule_id: "<empty>".to_owned(),
                reason: "id 不能为空".to_owned(),
            });
        }
        if self.message.trim().is_empty() {
            return Err(CoreError::InvalidRule {
                rule_id: self.id.clone(),
                reason: "message 不能为空".to_owned(),
            });
        }
        if self.effect == RuleEffect::Block
            && (self.strength != RuleStrength::Hard || !self.authority.can_block())
        {
            return Err(CoreError::InvalidRule {
                rule_id: self.id.clone(),
                reason: "Block 只允许 hard 且 authority 为 explicit_user、repository_rule 或 accepted_decision 的规则"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionDescriptor {
    pub schema_version: u32,
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    pub action: ActionKind,
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub target_files: Vec<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ActionDescriptor {
    /// 验证动作协议版本。
    ///
    /// # Errors
    ///
    /// 当 schema 版本不受支持时返回错误。
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchemaVersion {
                actual: self.schema_version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Read,
    Create,
    Modify,
    Delete,
    Execute,
    DependencyChange,
    GitOperation,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Proposed,
    Active,
    Challenged,
    Superseded,
    Retired,
}

const fn default_active() -> MemoryStatus {
    MemoryStatus::Active
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    ExplicitUser,
    RepositoryRule,
    AcceptedDecision,
    ObservedPattern,
    AgentInference,
}

impl Authority {
    const fn can_block(self) -> bool {
        matches!(
            self,
            Self::ExplicitUser | Self::RepositoryRule | Self::AcceptedDecision
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleStrength {
    Hard,
    Soft,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Block,
    InjectContext,
    Escalate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub rule_id: String,
    pub effect: RuleEffect,
    pub message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rationale: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    AllowWithContext,
    Block,
    Escalate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    pub schema_version: u32,
    pub decision: DecisionKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

#[cfg(test)]
mod tests {
    use super::{BrainConfig, CURRENT_SCHEMA_VERSION, StopReconcileConfig};
    use crate::CoreError;

    #[test]
    fn enabled_stop_reconcile_requires_a_base_and_envelope() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_name: "test".to_owned(),
            rules: Vec::new(),
            stop_reconcile: StopReconcileConfig {
                enabled: true,
                base: String::new(),
                envelope: ".project-brain/envelope.json".to_owned(),
            },
        };
        assert!(matches!(
            config.validate(),
            Err(CoreError::InvalidStopReconcile(_))
        ));
    }
}
