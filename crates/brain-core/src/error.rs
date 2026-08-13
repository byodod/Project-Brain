use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("不支持 schema_version={actual}，当前仅支持 {expected}")]
    UnsupportedSchemaVersion { actual: u32, expected: u32 },

    #[error("规则 {rule_id} 无效：{reason}")]
    InvalidRule { rule_id: String, reason: String },
}
