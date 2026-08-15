use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use brain_evidence::EvidencePlane;

use crate::CoreError;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrainConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub project_key: String,
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub language_profiles: Vec<ProjectLanguageProfile>,
    #[serde(default)]
    pub semantic_providers: Vec<SemanticProviderProfile>,
    #[serde(default)]
    pub finding_effect_mappings: Vec<FindingEffectMapping>,
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
        if self.project_key.trim().is_empty()
            || self.project_key.len() > 128
            || !self
                .project_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(CoreError::InvalidProjectKey(self.project_key.clone()));
        }
        validate_language_profiles(&self.language_profiles)?;
        validate_semantic_provider_profiles(&self.language_profiles, &self.semantic_providers)?;

        let mut mapping_ids = std::collections::BTreeSet::new();
        for mapping in &self.finding_effect_mappings {
            mapping.validate()?;
            if !mapping_ids.insert(mapping.id.clone()) {
                return Err(CoreError::InvalidFindingEffectMapping {
                    mapping_id: mapping.id.clone(),
                    reason: "id 重复".to_owned(),
                });
            }
        }

        for rule in &self.rules {
            rule.validate()?;
            validate_rule_symbol_scopes(rule, &self.semantic_providers)?;
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

/// 将一个精确 Evidence Finding 映射到治理 effect。
///
/// 未命中的 finding 永远保持 advisory；Block 仍受与普通规则相同的权限边界约束。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingEffectMapping {
    pub id: String,
    #[serde(default = "default_active")]
    pub status: MemoryStatus,
    pub authority: Authority,
    pub strength: RuleStrength,
    pub effect: RuleEffect,
    pub plane: EvidencePlane,
    pub provider_id: String,
    pub provider_contract_version: u16,
    pub finding_code: String,
    pub message: String,
}

impl FindingEffectMapping {
    /// 验证精确映射及其阻断权限。
    ///
    /// # Errors
    ///
    /// 当标识、provider、finding code、contract version 或权限边界无效时返回错误。
    pub fn validate(&self) -> Result<(), CoreError> {
        let invalid_identifier = |value: &str, max_len: usize| {
            value.trim().is_empty()
                || value.len() > max_len
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        if invalid_identifier(&self.id, 128) {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "id 为空或格式非法".to_owned(),
            });
        }
        if invalid_identifier(&self.provider_id, 128) {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "provider_id 为空或格式非法".to_owned(),
            });
        }
        if self.provider_contract_version == 0 {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "provider_contract_version 必须大于 0".to_owned(),
            });
        }
        if invalid_identifier(&self.finding_code, 128) {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "finding_code 为空或格式非法".to_owned(),
            });
        }
        if self.message.trim().is_empty() || self.message.len() > 4096 {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "message 为空或过长".to_owned(),
            });
        }
        if matches!(
            self.effect,
            RuleEffect::Block | RuleEffect::RequireReview | RuleEffect::Escalate
        ) && (self.strength != RuleStrength::Hard || !self.authority.can_block())
        {
            return Err(CoreError::InvalidFindingEffectMapping {
                mapping_id: self.id.clone(),
                reason: "Block/RequireReview/Escalate 只允许 hard 且 authority 为 explicit_user、repository_rule 或 accepted_decision 的映射".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectLanguageProfile {
    /// 开放的 SCIP/LSP language ID，例如 rust、csharp、python。
    pub language: String,
    /// 项目相对根；空数组表示整个项目。一个项目可声明多种语言。
    #[serde(default)]
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticProviderFormat {
    Scip,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticLanguageMapping {
    /// Producer 写入 `Document.language` 的原始值；`null` 只匹配缺失/空值。
    pub raw_language: Option<String>,
    /// 映射到项目声明的开放 language ID。
    pub language: String,
    #[serde(default)]
    pub allow_missing_language: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticProviderProfile {
    /// 项目内稳定且唯一的 provider 配置 ID，也是符号身份命名空间的一部分。
    pub id: String,
    pub format: SemanticProviderFormat,
    /// 允许导入的 SCIP `tool_info.name`。匹配时忽略 ASCII 大小写。
    pub producer: String,
    /// Project Brain 与该 producer 的解释契约版本，不等同于 producer 自身版本。
    pub contract_version: u16,
    pub language_mappings: Vec<SemanticLanguageMapping>,
}

fn validate_language_profiles(profiles: &[ProjectLanguageProfile]) -> Result<(), CoreError> {
    let mut languages = std::collections::BTreeSet::new();
    for profile in profiles {
        let language = profile.language.trim().to_ascii_lowercase();
        if language.is_empty()
            || language.len() > 64
            || !language.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'+' | b'#')
            })
            || !languages.insert(language.clone())
        {
            return Err(CoreError::InvalidLanguageProfile(format!(
                "language={:?} 为空、重复或格式非法",
                profile.language
            )));
        }
        let mut roots = std::collections::BTreeSet::new();
        for root in &profile.roots {
            let normalized = crate::normalize_project_path(root);
            let project_root_alias = matches!(root.trim(), "." | "./" | ".\\");
            if (normalized.is_empty() && !project_root_alias)
                || normalized.starts_with('/')
                || normalized.contains(':')
                || normalized.split('/').any(|part| part == "..")
                || !roots.insert(normalized)
            {
                return Err(CoreError::InvalidLanguageProfile(format!(
                    "language={language} 的 root={root:?} 非法或重复"
                )));
            }
        }
    }
    Ok(())
}

fn validate_semantic_provider_profiles(
    languages: &[ProjectLanguageProfile],
    profiles: &[SemanticProviderProfile],
) -> Result<(), CoreError> {
    let language_ids = languages
        .iter()
        .map(|profile| profile.language.trim().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let mut profile_ids = std::collections::BTreeSet::new();
    for profile in profiles {
        let id = profile.id.trim().to_ascii_lowercase();
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
            || !profile_ids.insert(id.clone())
        {
            return Err(CoreError::InvalidSemanticProviderProfile(format!(
                "id={:?} 为空、重复或格式非法",
                profile.id
            )));
        }
        if profile.producer.trim().is_empty() || profile.producer.len() > 128 {
            return Err(CoreError::InvalidSemanticProviderProfile(format!(
                "id={id} 的 producer 无效"
            )));
        }
        if profile.contract_version != 1 {
            return Err(CoreError::InvalidSemanticProviderProfile(format!(
                "id={id} 的 contract_version={} 不受支持",
                profile.contract_version
            )));
        }
        if profile.language_mappings.is_empty() {
            return Err(CoreError::InvalidSemanticProviderProfile(format!(
                "id={id} 缺少 language_mappings"
            )));
        }
        let mut raw_languages = std::collections::BTreeSet::new();
        for mapping in &profile.language_mappings {
            let language = mapping.language.trim().to_ascii_lowercase();
            if !language_ids.contains(&language) {
                return Err(CoreError::InvalidSemanticProviderProfile(format!(
                    "id={id} 映射到未声明 language={:?}",
                    mapping.language
                )));
            }
            let raw = mapping
                .raw_language
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if mapping
                .raw_language
                .as_deref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
            {
                return Err(CoreError::InvalidSemanticProviderProfile(format!(
                    "id={id} 的 raw language 无效"
                )));
            }
            if raw.is_none() != mapping.allow_missing_language {
                return Err(CoreError::InvalidSemanticProviderProfile(format!(
                    "id={id} 的缺失 language 映射必须且只能设置 allow_missing_language=true"
                )));
            }
            let raw_key = raw.map_or_else(|| "<missing>".to_owned(), ToOwned::to_owned);
            if !raw_languages.insert(raw_key.clone()) {
                return Err(CoreError::InvalidSemanticProviderProfile(format!(
                    "id={id} 的 raw language={raw_key:?} 重复"
                )));
            }
        }
    }
    Ok(())
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
    /// 绑定到已提交 semantic snapshot 的符号锚点。Runtime 只能沿人工确认的
    /// lineage 解析，绝不能为 rename/move 猜造稳定 ID。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_scopes: Vec<RuleSymbolScope>,
    pub message: String,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuleSymbolScope {
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language_id: String,
    pub anchor_snapshot_fingerprint: String,
    pub anchor_symbol_id: String,
    pub resolution_policy: SymbolResolutionPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SymbolResolutionPolicy {
    ConfirmedLineageOnly,
}

fn validate_rule_symbol_scopes(
    rule: &Rule,
    providers: &[SemanticProviderProfile],
) -> Result<(), CoreError> {
    let mut unique = std::collections::BTreeSet::new();
    for scope in &rule.symbol_scopes {
        let profile = providers
            .iter()
            .find(|profile| profile.id == scope.provider_profile_id)
            .ok_or_else(|| CoreError::InvalidRule {
                rule_id: rule.id.clone(),
                reason: format!(
                    "symbol scope 引用了未知 provider profile={:?}",
                    scope.provider_profile_id
                ),
            })?;
        if scope.provider_contract_id.trim().is_empty() || scope.provider_contract_id.len() > 192 {
            return Err(CoreError::InvalidRule {
                rule_id: rule.id.clone(),
                reason: format!(
                    "symbol scope 的 provider_contract_id={:?} 无效",
                    scope.provider_contract_id
                ),
            });
        }
        let language = scope.language_id.trim().to_ascii_lowercase();
        if !profile
            .language_mappings
            .iter()
            .any(|mapping| mapping.language.eq_ignore_ascii_case(&language))
        {
            return Err(CoreError::InvalidRule {
                rule_id: rule.id.clone(),
                reason: format!(
                    "symbol scope 的 language_id={:?} 不属于 provider profile={} 的映射",
                    scope.language_id, profile.id
                ),
            });
        }
        if scope.anchor_snapshot_fingerprint.trim().is_empty()
            || scope.anchor_snapshot_fingerprint.len() > 160
            || scope.anchor_symbol_id.trim().is_empty()
            || scope.anchor_symbol_id.len() > 160
        {
            return Err(CoreError::InvalidRule {
                rule_id: rule.id.clone(),
                reason: "symbol scope 缺少合法 snapshot/symbol 锚点".to_owned(),
            });
        }
        let key = (
            scope.provider_profile_id.clone(),
            scope.provider_contract_id.clone(),
            language,
            scope.anchor_snapshot_fingerprint.clone(),
            scope.anchor_symbol_id.clone(),
        );
        if !unique.insert(key) {
            return Err(CoreError::InvalidRule {
                rule_id: rule.id.clone(),
                reason: "存在重复 symbol scope".to_owned(),
            });
        }
    }
    Ok(())
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
        if matches!(self.effect, RuleEffect::Block | RuleEffect::RequireReview)
            && (self.strength != RuleStrength::Hard || !self.authority.can_block())
        {
            return Err(CoreError::InvalidRule {
                rule_id: self.id.clone(),
                reason: "Block/RequireReview 只允许 hard 且 authority 为 explicit_user、repository_rule 或 accepted_decision 的规则"
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
    RequireReview,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<EvidenceGrade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGrade {
    DeterministicPath,
    SemanticDirect,
    SemanticConfirmedLineage,
    SemanticBaselineDiff,
    AdvisorySyntax,
    Inferred,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    AllowWithContext,
    Block,
    RequireReview,
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
    use brain_evidence::EvidencePlane;

    use super::{
        Authority, BrainConfig, CURRENT_SCHEMA_VERSION, FindingEffectMapping, MemoryStatus,
        ProjectLanguageProfile, RuleEffect, RuleStrength, SemanticLanguageMapping,
        SemanticProviderFormat, SemanticProviderProfile, StopReconcileConfig,
    };
    use crate::CoreError;

    #[test]
    fn enabled_stop_reconcile_requires_a_base_and_envelope() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: Vec::new(),
            semantic_providers: Vec::new(),
            finding_effect_mappings: Vec::new(),
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

    #[test]
    fn language_profiles_are_open_but_unique_and_project_relative() {
        let mut config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: vec![ProjectLanguageProfile {
                language: "csharp".to_owned(),
                roots: vec!["src/App".to_owned()],
            }],
            semantic_providers: vec![SemanticProviderProfile {
                id: "dotnet-main".to_owned(),
                format: SemanticProviderFormat::Scip,
                producer: "scip-dotnet".to_owned(),
                contract_version: 1,
                language_mappings: vec![SemanticLanguageMapping {
                    raw_language: Some("C#".to_owned()),
                    language: "csharp".to_owned(),
                    allow_missing_language: false,
                }],
            }],
            finding_effect_mappings: Vec::new(),
            rules: Vec::new(),
            stop_reconcile: StopReconcileConfig::default(),
        };
        assert!(config.validate().is_ok());

        config.language_profiles.push(ProjectLanguageProfile {
            language: "CSharp".to_owned(),
            roots: Vec::new(),
        });
        assert!(matches!(
            config.validate(),
            Err(CoreError::InvalidLanguageProfile(_))
        ));

        config.language_profiles = vec![ProjectLanguageProfile {
            language: "csharp".to_owned(),
            roots: vec![".".to_owned()],
        }];
        assert!(config.validate().is_ok());

        for invalid_root in ["", "/", "C:/repo"] {
            config.language_profiles[0].roots = vec![invalid_root.to_owned()];
            assert!(matches!(
                config.validate(),
                Err(CoreError::InvalidLanguageProfile(_))
            ));
        }
    }

    #[test]
    fn missing_language_requires_an_explicit_mapping() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: vec![ProjectLanguageProfile {
                language: "python".to_owned(),
                roots: Vec::new(),
            }],
            semantic_providers: vec![SemanticProviderProfile {
                id: "python-main".to_owned(),
                format: SemanticProviderFormat::Scip,
                producer: "scip-python".to_owned(),
                contract_version: 1,
                language_mappings: vec![SemanticLanguageMapping {
                    raw_language: None,
                    language: "python".to_owned(),
                    allow_missing_language: true,
                }],
            }],
            finding_effect_mappings: Vec::new(),
            rules: Vec::new(),
            stop_reconcile: StopReconcileConfig::default(),
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn finding_block_mapping_requires_hard_repository_authority() {
        let mapping = FindingEffectMapping {
            id: "TEST-FAIL-001".to_owned(),
            status: MemoryStatus::Active,
            authority: Authority::AgentInference,
            strength: RuleStrength::Hard,
            effect: RuleEffect::Block,
            plane: EvidencePlane::Test,
            provider_id: "dotnet-test".to_owned(),
            provider_contract_version: 1,
            finding_code: "assertion_failed".to_owned(),
            message: "声明的测试断言失败，必须继续修复".to_owned(),
        };
        assert!(matches!(
            mapping.validate(),
            Err(CoreError::InvalidFindingEffectMapping { .. })
        ));

        let mut authorized = mapping;
        authorized.authority = Authority::RepositoryRule;
        authorized.strength = RuleStrength::Soft;
        assert!(matches!(
            authorized.validate(),
            Err(CoreError::InvalidFindingEffectMapping { .. })
        ));
        authorized.strength = RuleStrength::Hard;
        assert!(authorized.validate().is_ok());
        authorized.effect = RuleEffect::Escalate;
        authorized.authority = Authority::ObservedPattern;
        assert!(matches!(
            authorized.validate(),
            Err(CoreError::InvalidFindingEffectMapping { .. })
        ));
    }
}
