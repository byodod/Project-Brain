use brain_evidence::{
    ArtifactEdge, ArtifactNode, EvidenceCoverage, EvidenceInputManifestV1, EvidencePlane,
    EvidenceReference, FindingSeverity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const PROVIDER_PROCESS_PROTOCOL_VERSION: u32 = 1;
pub const PROVIDER_DESCRIPTOR_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_RUN_REQUEST_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_RUN_RESPONSE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderDescriptorV1 {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub provider_id: String,
    pub provider_version: String,
    pub provider_contract_version: u32,
    pub capabilities: Vec<EvidencePlane>,
}

impl ProviderDescriptorV1 {
    /// 验证 descriptor 的版本、身份和规范 capability 集合。
    ///
    /// # Errors
    ///
    /// 版本不受支持、身份非法或 capability 为空/重复/非规范排序时返回错误。
    pub fn validate(&self) -> Result<(), ProviderProtocolError> {
        if self.schema_version != PROVIDER_DESCRIPTOR_SCHEMA_VERSION {
            return Err(ProviderProtocolError::UnsupportedSchema {
                kind: "provider_descriptor",
                actual: self.schema_version,
                expected: PROVIDER_DESCRIPTOR_SCHEMA_VERSION,
            });
        }
        if self.protocol_version != PROVIDER_PROCESS_PROTOCOL_VERSION {
            return Err(ProviderProtocolError::UnsupportedProtocol {
                actual: self.protocol_version,
                expected: PROVIDER_PROCESS_PROTOCOL_VERSION,
            });
        }
        validate_id("provider_id", &self.provider_id, 128)?;
        validate_text("provider_version", &self.provider_version, 256)?;
        if self.provider_contract_version == 0 {
            return Err(ProviderProtocolError::Invalid(
                "provider_contract_version 必须大于 0".to_owned(),
            ));
        }
        if self.capabilities.is_empty() {
            return Err(ProviderProtocolError::Invalid(
                "provider capabilities 不能为空".to_owned(),
            ));
        }
        let mut capabilities = self.capabilities.clone();
        capabilities.sort();
        capabilities.dedup();
        if capabilities != self.capabilities {
            return Err(ProviderProtocolError::Invalid(
                "provider capabilities 必须规范排序且不重复".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderRunRequestV1 {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub request_id: String,
    pub provider_id: String,
    pub profile_id: String,
    pub project_key: String,
    pub plane: EvidencePlane,
    pub source_fingerprint: String,
    pub input_manifest: EvidenceInputManifestV1,
    pub staged_project_root: String,
    pub output_root: String,
    pub opaque_config: Value,
    pub opaque_config_hash: String,
    pub timeout_ms: u64,
}

impl ProviderRunRequestV1 {
    /// 验证 request 身份、staging 边界、输入清单和 opaque config hash。
    ///
    /// # Errors
    ///
    /// 版本、身份、路径、timeout、manifest 或 config hash 无效时返回错误。
    pub fn validate(&self) -> Result<(), ProviderProtocolError> {
        if self.schema_version != PROVIDER_RUN_REQUEST_SCHEMA_VERSION {
            return Err(ProviderProtocolError::UnsupportedSchema {
                kind: "provider_run_request",
                actual: self.schema_version,
                expected: PROVIDER_RUN_REQUEST_SCHEMA_VERSION,
            });
        }
        if self.protocol_version != PROVIDER_PROCESS_PROTOCOL_VERSION {
            return Err(ProviderProtocolError::UnsupportedProtocol {
                actual: self.protocol_version,
                expected: PROVIDER_PROCESS_PROTOCOL_VERSION,
            });
        }
        validate_text("request_id", &self.request_id, 256)?;
        validate_id("provider_id", &self.provider_id, 128)?;
        validate_id("profile_id", &self.profile_id, 128)?;
        validate_id("project_key", &self.project_key, 128)?;
        validate_text("source_fingerprint", &self.source_fingerprint, 256)?;
        validate_absolute_transport_path("staged_project_root", &self.staged_project_root)?;
        validate_absolute_transport_path("output_root", &self.output_root)?;
        if self.timeout_ms == 0 || self.timeout_ms > 86_400_000 {
            return Err(ProviderProtocolError::Invalid(
                "timeout_ms 必须位于 1..=86400000".to_owned(),
            ));
        }
        self.input_manifest.validate()?;
        if self.input_manifest.contract.project_key != self.project_key
            || self.input_manifest.contract.profile_id != self.profile_id
            || self.input_manifest.source_fingerprint_at_creation != self.source_fingerprint
        {
            return Err(ProviderProtocolError::Invalid(
                "input manifest 与 request project/profile/source 绑定不一致".to_owned(),
            ));
        }
        let config_hash =
            brain_evidence::content_fingerprint(&serde_json::to_vec(&self.opaque_config)?);
        if config_hash != self.opaque_config_hash {
            return Err(ProviderProtocolError::Invalid(
                "opaque provider config hash 不匹配".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRunStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderFindingCandidateV1 {
    pub code: String,
    pub severity: FindingSeverity,
    pub deterministic_violation_claim: bool,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderEvidenceCandidateV1 {
    pub plane: EvidencePlane,
    pub provider_version: String,
    pub provider_contract_version: u32,
    pub coverage: EvidenceCoverage,
    pub upstream: Vec<EvidenceReference>,
    pub artifacts: Vec<ArtifactNode>,
    pub edges: Vec<ArtifactEdge>,
    pub findings: Vec<ProviderFindingCandidateV1>,
    pub payload_schema: String,
    pub payload: Value,
    pub payload_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderRunResponseV1 {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub request_id: String,
    pub provider_id: String,
    pub profile_id: String,
    pub project_key: String,
    pub source_fingerprint: String,
    pub input_manifest_hash: String,
    pub status: ProviderRunStatus,
    pub candidate: Option<ProviderEvidenceCandidateV1>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

impl ProviderRunResponseV1 {
    /// 对照已发送 request 与机器绑定 descriptor 验证 Provider 响应。
    ///
    /// # Errors
    ///
    /// 版本、身份、状态、candidate 能力或 payload hash 不匹配时返回错误。
    pub fn validate_against(
        &self,
        request: &ProviderRunRequestV1,
        descriptor: &ProviderDescriptorV1,
    ) -> Result<(), ProviderProtocolError> {
        if self.schema_version != PROVIDER_RUN_RESPONSE_SCHEMA_VERSION {
            return Err(ProviderProtocolError::UnsupportedSchema {
                kind: "provider_run_response",
                actual: self.schema_version,
                expected: PROVIDER_RUN_RESPONSE_SCHEMA_VERSION,
            });
        }
        if self.protocol_version != PROVIDER_PROCESS_PROTOCOL_VERSION {
            return Err(ProviderProtocolError::UnsupportedProtocol {
                actual: self.protocol_version,
                expected: PROVIDER_PROCESS_PROTOCOL_VERSION,
            });
        }
        if self.request_id != request.request_id
            || self.provider_id != request.provider_id
            || self.profile_id != request.profile_id
            || self.project_key != request.project_key
            || self.source_fingerprint != request.source_fingerprint
            || self.input_manifest_hash != request.input_manifest.manifest_hash
            || descriptor.provider_id != request.provider_id
        {
            return Err(ProviderProtocolError::Invalid(
                "provider response identity/source/input binding 不匹配".to_owned(),
            ));
        }
        match self.status {
            ProviderRunStatus::Succeeded => {
                let candidate = self.candidate.as_ref().ok_or_else(|| {
                    ProviderProtocolError::Invalid(
                        "succeeded provider response 缺少 candidate".to_owned(),
                    )
                })?;
                if self.error_code.is_some() || self.error_message.is_some() {
                    return Err(ProviderProtocolError::Invalid(
                        "succeeded provider response 不得携带 error".to_owned(),
                    ));
                }
                if candidate.plane != request.plane
                    || !descriptor.capabilities.contains(&candidate.plane)
                    || candidate.provider_version != descriptor.provider_version
                    || candidate.provider_contract_version != descriptor.provider_contract_version
                {
                    return Err(ProviderProtocolError::Invalid(
                        "candidate plane/version/contract 超出已注册 descriptor".to_owned(),
                    ));
                }
                validate_id("payload_schema", &candidate.payload_schema, 128)?;
                validate_text("payload_hash", &candidate.payload_hash, 256)?;
                let actual_payload_hash =
                    brain_evidence::content_fingerprint(&serde_json::to_vec(&candidate.payload)?);
                if candidate.payload_hash != actual_payload_hash {
                    return Err(ProviderProtocolError::Invalid(
                        "candidate payload_hash 与 payload 不匹配".to_owned(),
                    ));
                }
                for finding in &candidate.findings {
                    validate_id("finding.code", &finding.code, 128)?;
                    validate_text("finding.message", &finding.message, 4_096)?;
                }
            }
            ProviderRunStatus::Failed => {
                if self.candidate.is_some() {
                    return Err(ProviderProtocolError::Invalid(
                        "failed provider response 不得携带 candidate".to_owned(),
                    ));
                }
                validate_id(
                    "error_code",
                    self.error_code.as_deref().ok_or_else(|| {
                        ProviderProtocolError::Invalid(
                            "failed provider response 缺少 error_code".to_owned(),
                        )
                    })?,
                    128,
                )?;
                validate_text(
                    "error_message",
                    self.error_message.as_deref().ok_or_else(|| {
                        ProviderProtocolError::Invalid(
                            "failed provider response 缺少 error_message".to_owned(),
                        )
                    })?,
                    4_096,
                )?;
            }
        }
        Ok(())
    }
}

fn validate_id(field: &'static str, value: &str, max: usize) -> Result<(), ProviderProtocolError> {
    if value.is_empty()
        || value.len() > max
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(ProviderProtocolError::Invalid(format!(
            "{field} 不是规范 provider identity"
        )));
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ProviderProtocolError> {
    if value.trim().is_empty() || value.len() > max || value.contains(['\0', '\n', '\r']) {
        return Err(ProviderProtocolError::Invalid(format!(
            "{field} 为空、超限或含控制字符"
        )));
    }
    Ok(())
}

fn validate_absolute_transport_path(
    field: &'static str,
    value: &str,
) -> Result<(), ProviderProtocolError> {
    validate_text(field, value, 4_096)?;
    if !std::path::Path::new(value).is_absolute() {
        return Err(ProviderProtocolError::Invalid(format!(
            "{field} 必须是 machine scratch 绝对路径"
        )));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProviderProtocolError {
    #[error("不支持的 {kind} schema_version={actual}，期望 {expected}")]
    UnsupportedSchema {
        kind: &'static str,
        actual: u32,
        expected: u32,
    },
    #[error("不支持的 provider protocol_version={actual}，期望 {expected}")]
    UnsupportedProtocol { actual: u32, expected: u32 },
    #[error("Provider Protocol 无效：{0}")]
    Invalid(String),
    #[error("Evidence input contract 无效：{0}")]
    Evidence(#[from] brain_evidence::EvidenceError),
    #[error("Provider Protocol JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_evidence::{
        DependencyCoverage, InputDependencyContractV1, InputManifestEntry, InputPathState,
        InputRole, InputSelectorV1,
    };

    fn request() -> ProviderRunRequestV1 {
        let contract = InputDependencyContractV1::new(
            "project-a",
            "main",
            "engine-provider",
            1,
            "profile-hash",
            vec![InputSelectorV1::ExactPath {
                path: "engine.project".to_owned(),
                role: InputRole::Control,
                presence_sensitive: true,
            }],
            DependencyCoverage::Complete,
        )
        .unwrap();
        let input_manifest = EvidenceInputManifestV1::new(
            contract,
            "source-a",
            vec![InputManifestEntry {
                path: "engine.project".to_owned(),
                state: InputPathState::PresentRegularFile,
                role: InputRole::Control,
                content_sha256: Some("a".repeat(64)),
                size: Some(1),
            }],
        )
        .unwrap();
        let opaque_config = serde_json::json!({"mode":"headless"});
        ProviderRunRequestV1 {
            schema_version: PROVIDER_RUN_REQUEST_SCHEMA_VERSION,
            protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            provider_id: "engine.provider.v1".to_owned(),
            profile_id: "main".to_owned(),
            project_key: "project-a".to_owned(),
            plane: EvidencePlane::Engine,
            source_fingerprint: "source-a".to_owned(),
            input_manifest,
            staged_project_root: if cfg!(windows) {
                "C:/scratch/project".to_owned()
            } else {
                "/scratch/project".to_owned()
            },
            output_root: if cfg!(windows) {
                "C:/scratch/output".to_owned()
            } else {
                "/scratch/output".to_owned()
            },
            opaque_config_hash: brain_evidence::content_fingerprint(
                &serde_json::to_vec(&opaque_config).unwrap(),
            ),
            opaque_config,
            timeout_ms: 300_000,
        }
    }

    #[test]
    fn request_binds_project_profile_source_inputs_and_config() {
        let mut request = request();
        request.validate().unwrap();
        request.opaque_config = serde_json::json!({"mode":"changed"});
        assert!(request.validate().is_err());
    }

    #[test]
    fn response_cannot_escape_registered_plane_or_identity() {
        let request = request();
        let descriptor = ProviderDescriptorV1 {
            schema_version: PROVIDER_DESCRIPTOR_SCHEMA_VERSION,
            protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
            provider_id: request.provider_id.clone(),
            provider_version: "1.0.0".to_owned(),
            provider_contract_version: 1,
            capabilities: vec![EvidencePlane::Engine],
        };
        descriptor.validate().unwrap();
        let mut response = ProviderRunResponseV1 {
            schema_version: PROVIDER_RUN_RESPONSE_SCHEMA_VERSION,
            protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            provider_id: request.provider_id.clone(),
            profile_id: request.profile_id.clone(),
            project_key: request.project_key.clone(),
            source_fingerprint: request.source_fingerprint.clone(),
            input_manifest_hash: request.input_manifest.manifest_hash.clone(),
            status: ProviderRunStatus::Succeeded,
            candidate: Some(ProviderEvidenceCandidateV1 {
                plane: EvidencePlane::Engine,
                provider_version: descriptor.provider_version.clone(),
                provider_contract_version: 1,
                coverage: EvidenceCoverage::Complete,
                upstream: Vec::new(),
                artifacts: Vec::new(),
                edges: Vec::new(),
                findings: Vec::new(),
                payload_schema: "engine.snapshot.v1".to_owned(),
                payload: serde_json::json!({"loaded": true}),
                payload_hash: brain_evidence::content_fingerprint(
                    &serde_json::to_vec(&serde_json::json!({"loaded": true})).unwrap(),
                ),
            }),
            error_code: None,
            error_message: None,
        };
        response.validate_against(&request, &descriptor).unwrap();
        response.candidate.as_mut().unwrap().plane = EvidencePlane::Runtime;
        assert!(response.validate_against(&request, &descriptor).is_err());
    }
}
