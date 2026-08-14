use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("不支持 schema_version={actual}，当前仅支持 {expected}")]
    UnsupportedSchemaVersion { actual: u32, expected: u32 },

    #[error("不支持 Hook protocol_version={actual}，当前仅支持 {expected}")]
    UnsupportedHookProtocolVersion { actual: u16, expected: u16 },

    #[error("内部 Hook 事件无效：{0}")]
    InvalidHookEvent(String),

    #[error("project_key 无效：{0:?}")]
    InvalidProjectKey(String),

    #[error("项目语言 profile 无效：{0}")]
    InvalidLanguageProfile(String),

    #[error("语义 provider profile 无效：{0}")]
    InvalidSemanticProviderProfile(String),

    #[error("规则 {rule_id} 无效：{reason}")]
    InvalidRule { rule_id: String, reason: String },

    #[error("Finding effect 映射 {mapping_id} 无效：{reason}")]
    InvalidFindingEffectMapping { mapping_id: String, reason: String },

    #[error("Stop reconcile 配置无效：{0}")]
    InvalidStopReconcile(String),
}
