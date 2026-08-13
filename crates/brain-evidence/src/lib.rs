use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const EVIDENCE_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePlane {
    Source,
    Semantic,
    Engine,
    Build,
    Runtime,
}

impl EvidencePlane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Semantic => "semantic",
            Self::Engine => "engine",
            Self::Build => "build",
            Self::Runtime => "runtime",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "semantic" => Some(Self::Semantic),
            "engine" => Some(Self::Engine),
            "build" => Some(Self::Build),
            "runtime" => Some(Self::Runtime),
            _ => None,
        }
    }

    const fn accepts_upstream(self, upstream: Self) -> bool {
        match self {
            Self::Source => false,
            Self::Semantic => matches!(upstream, Self::Source),
            Self::Engine => matches!(upstream, Self::Source | Self::Semantic),
            Self::Build => matches!(upstream, Self::Source | Self::Semantic | Self::Engine),
            Self::Runtime => !matches!(upstream, Self::Runtime),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAuthority {
    Deterministic,
    Heuristic,
}

impl EvidenceAuthority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Heuristic => "heuristic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "deterministic" => Some(Self::Deterministic),
            "heuristic" => Some(Self::Heuristic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCoverage {
    Complete,
    Partial,
}

impl EvidenceCoverage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFreshness {
    Fresh,
    Stale,
    Unknown,
}

impl EvidenceFreshness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fresh" => Some(Self::Fresh),
            "stale" => Some(Self::Stale),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Warning,
    Error,
}

impl FindingSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEdgeKind {
    Contains,
    References,
    Instances,
    AttachesScript,
    UsesResource,
    DeclaresAutoload,
    MainScene,
    Produces,
    LoadsAtRuntime,
}

impl ArtifactEdgeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::References => "references",
            Self::Instances => "instances",
            Self::AttachesScript => "attaches_script",
            Self::UsesResource => "uses_resource",
            Self::DeclaresAutoload => "declares_autoload",
            Self::MainScene => "main_scene",
            Self::Produces => "produces",
            Self::LoadsAtRuntime => "loads_at_runtime",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceProvider {
    pub id: String,
    pub version: String,
    pub contract_version: u16,
    pub authority: EvidenceAuthority,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EvidenceReference {
    pub plane: EvidencePlane,
    pub provider_id: String,
    pub snapshot_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactNode {
    pub id: String,
    pub project_key: String,
    pub provider_id: String,
    /// Provider 开放 kind，例如 `godot_scene`、`dotnet_assembly` 或 `runtime_scenario`。
    pub kind: String,
    pub provider_key: String,
    pub display_name: String,
    pub path: Option<String>,
    pub content_fingerprint: String,
}

impl ArtifactNode {
    pub fn from_provider_key(
        project_key: &str,
        provider_id: &str,
        kind: &str,
        provider_key: &str,
        display_name: &str,
        path: Option<&str>,
        content: &[u8],
    ) -> Self {
        Self {
            id: artifact_id(project_key, provider_id, provider_key),
            project_key: project_key.to_owned(),
            provider_id: provider_id.to_owned(),
            kind: kind.to_owned(),
            provider_key: provider_key.to_owned(),
            display_name: display_name.to_owned(),
            path: path.map(ToOwned::to_owned),
            content_fingerprint: fingerprint(&[content]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEdge {
    pub source_id: String,
    pub target_id: String,
    pub kind: ArtifactEdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceFinding {
    pub code: String,
    pub severity: FindingSeverity,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceSnapshot {
    pub protocol_version: u32,
    pub project_key: String,
    pub plane: EvidencePlane,
    pub provider: EvidenceProvider,
    /// 生成本快照时实际读取的工作树内容摘要，不复用其他 Evidence Plane 的快照摘要。
    pub source_fingerprint: String,
    pub snapshot_fingerprint: String,
    pub coverage: EvidenceCoverage,
    pub upstream: Vec<EvidenceReference>,
    pub artifacts: Vec<ArtifactNode>,
    pub edges: Vec<ArtifactEdge>,
    pub findings: Vec<EvidenceFinding>,
}

impl EvidenceSnapshot {
    /// 建立规范排序且具有确定性 fingerprint 的 Evidence Snapshot。
    ///
    /// # Errors
    ///
    /// 当 provider、project、upstream、ArtifactGraph 或 finding 违反协议边界时返回错误。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_key: &str,
        plane: EvidencePlane,
        provider: EvidenceProvider,
        source_fingerprint: &str,
        coverage: EvidenceCoverage,
        mut upstream: Vec<EvidenceReference>,
        mut artifacts: Vec<ArtifactNode>,
        mut edges: Vec<ArtifactEdge>,
        mut findings: Vec<EvidenceFinding>,
    ) -> Result<Self, EvidenceError> {
        upstream.sort();
        artifacts.sort_by(|left, right| left.id.cmp(&right.id));
        edges.sort_by(|left, right| {
            (&left.source_id, &left.target_id, left.kind).cmp(&(
                &right.source_id,
                &right.target_id,
                right.kind,
            ))
        });
        findings.sort_by(|left, right| {
            (
                &left.code,
                left.severity,
                &left.artifact_id,
                &left.path,
                &left.message,
            )
                .cmp(&(
                    &right.code,
                    right.severity,
                    &right.artifact_id,
                    &right.path,
                    &right.message,
                ))
        });
        let mut snapshot = Self {
            protocol_version: EVIDENCE_PROTOCOL_VERSION,
            project_key: project_key.to_owned(),
            plane,
            provider,
            source_fingerprint: source_fingerprint.to_owned(),
            snapshot_fingerprint: String::new(),
            coverage,
            upstream,
            artifacts,
            edges,
            findings,
        };
        snapshot.validate_without_fingerprint()?;
        snapshot.snapshot_fingerprint = snapshot.computed_fingerprint();
        Ok(snapshot)
    }

    /// 验证反序列化快照的协议版本、图边界与内容 fingerprint。
    ///
    /// # Errors
    ///
    /// 当快照字段非法、图不闭合或 fingerprint 与内容不一致时返回错误。
    pub fn validate(&self) -> Result<(), EvidenceError> {
        if self.protocol_version != EVIDENCE_PROTOCOL_VERSION {
            return Err(EvidenceError::UnsupportedProtocolVersion {
                actual: self.protocol_version,
                expected: EVIDENCE_PROTOCOL_VERSION,
            });
        }
        self.validate_without_fingerprint()?;
        let expected = self.computed_fingerprint();
        if self.snapshot_fingerprint != expected {
            return Err(EvidenceError::FingerprintMismatch {
                actual: self.snapshot_fingerprint.clone(),
                expected,
            });
        }
        Ok(())
    }

    pub fn freshness(
        &self,
        current_source_fingerprint: Option<&str>,
        current_upstream: &[EvidenceReference],
    ) -> EvidenceFreshness {
        let Some(current_source) = current_source_fingerprint else {
            return EvidenceFreshness::Unknown;
        };
        if current_source != self.source_fingerprint {
            return EvidenceFreshness::Stale;
        }
        let current = current_upstream
            .iter()
            .map(|item| {
                (
                    (item.plane, item.provider_id.as_str()),
                    item.snapshot_fingerprint.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for required in &self.upstream {
            let Some(observed) = current.get(&(required.plane, required.provider_id.as_str()))
            else {
                return EvidenceFreshness::Unknown;
            };
            if *observed != required.snapshot_fingerprint {
                return EvidenceFreshness::Stale;
            }
        }
        EvidenceFreshness::Fresh
    }

    pub fn finding_can_hard_block(
        &self,
        finding: &EvidenceFinding,
        freshness: EvidenceFreshness,
    ) -> bool {
        self.provider.authority == EvidenceAuthority::Deterministic
            && self.coverage == EvidenceCoverage::Complete
            && freshness == EvidenceFreshness::Fresh
            && finding.severity == FindingSeverity::Error
            && self.findings.contains(finding)
    }

    fn validate_without_fingerprint(&self) -> Result<(), EvidenceError> {
        validate_identifier("project_key", &self.project_key, 128)?;
        validate_identifier("provider.id", &self.provider.id, 128)?;
        if self.provider.version.trim().is_empty() || self.provider.version.len() > 128 {
            return Err(EvidenceError::InvalidField("provider.version"));
        }
        if self.provider.contract_version == 0 {
            return Err(EvidenceError::InvalidField("provider.contract_version"));
        }
        validate_fingerprint("source_fingerprint", &self.source_fingerprint)?;

        self.validate_canonical_order()?;

        let mut upstream_keys = BTreeSet::new();
        for item in &self.upstream {
            if !self.plane.accepts_upstream(item.plane) {
                return Err(EvidenceError::InvalidUpstreamPlane {
                    plane: self.plane,
                    upstream: item.plane,
                });
            }
            validate_identifier("upstream.provider_id", &item.provider_id, 128)?;
            validate_fingerprint("upstream.snapshot_fingerprint", &item.snapshot_fingerprint)?;
            if !upstream_keys.insert((item.plane, item.provider_id.as_str())) {
                return Err(EvidenceError::DuplicateUpstream);
            }
        }

        let mut artifact_ids = BTreeSet::new();
        for artifact in &self.artifacts {
            if artifact.project_key != self.project_key || artifact.provider_id != self.provider.id
            {
                return Err(EvidenceError::ArtifactBoundary(artifact.id.clone()));
            }
            validate_identifier("artifact.kind", &artifact.kind, 128)?;
            if artifact.provider_key.trim().is_empty() || artifact.provider_key.len() > 1_024 {
                return Err(EvidenceError::InvalidField("artifact.provider_key"));
            }
            validate_fingerprint(
                "artifact.content_fingerprint",
                &artifact.content_fingerprint,
            )?;
            if artifact
                .path
                .as_deref()
                .is_some_and(|path| !valid_project_path(path))
            {
                return Err(EvidenceError::InvalidPath(
                    artifact.path.clone().unwrap_or_default(),
                ));
            }
            if artifact.id
                != artifact_id(&self.project_key, &self.provider.id, &artifact.provider_key)
            {
                return Err(EvidenceError::ArtifactIdentity(artifact.id.clone()));
            }
            if !artifact_ids.insert(artifact.id.as_str()) {
                return Err(EvidenceError::DuplicateArtifact(artifact.id.clone()));
            }
        }

        let mut edge_keys = BTreeSet::new();
        for edge in &self.edges {
            if !artifact_ids.contains(edge.source_id.as_str())
                || !artifact_ids.contains(edge.target_id.as_str())
            {
                return Err(EvidenceError::DanglingEdge {
                    source_id: edge.source_id.clone(),
                    target_id: edge.target_id.clone(),
                });
            }
            if !edge_keys.insert((&edge.source_id, &edge.target_id, edge.kind)) {
                return Err(EvidenceError::DuplicateEdge);
            }
        }

        for finding in &self.findings {
            validate_identifier("finding.code", &finding.code, 128)?;
            if finding.message.trim().is_empty() || finding.message.len() > 8_192 {
                return Err(EvidenceError::InvalidField("finding.message"));
            }
            if finding
                .artifact_id
                .as_deref()
                .is_some_and(|id| !artifact_ids.contains(id))
            {
                return Err(EvidenceError::UnknownFindingArtifact(
                    finding.artifact_id.clone().unwrap_or_default(),
                ));
            }
            if finding
                .path
                .as_deref()
                .is_some_and(|path| !valid_project_path(path))
            {
                return Err(EvidenceError::InvalidPath(
                    finding.path.clone().unwrap_or_default(),
                ));
            }
        }
        Ok(())
    }

    fn validate_canonical_order(&self) -> Result<(), EvidenceError> {
        if self.upstream.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(EvidenceError::NonCanonicalOrder("upstream"));
        }
        if self
            .artifacts
            .windows(2)
            .any(|pair| pair[0].id > pair[1].id)
        {
            return Err(EvidenceError::NonCanonicalOrder("artifacts"));
        }
        if self.edges.windows(2).any(|pair| {
            (&pair[0].source_id, &pair[0].target_id, pair[0].kind)
                > (&pair[1].source_id, &pair[1].target_id, pair[1].kind)
        }) {
            return Err(EvidenceError::NonCanonicalOrder("edges"));
        }
        if self.findings.windows(2).any(|pair| {
            (
                &pair[0].code,
                pair[0].severity,
                &pair[0].artifact_id,
                &pair[0].path,
                &pair[0].message,
            ) > (
                &pair[1].code,
                pair[1].severity,
                &pair[1].artifact_id,
                &pair[1].path,
                &pair[1].message,
            )
        }) {
            return Err(EvidenceError::NonCanonicalOrder("findings"));
        }
        Ok(())
    }

    fn computed_fingerprint(&self) -> String {
        let mut bytes = Vec::new();
        append_part(&mut bytes, &u64::from(self.protocol_version).to_be_bytes());
        append_part(&mut bytes, self.project_key.as_bytes());
        append_part(&mut bytes, self.plane.as_str().as_bytes());
        append_part(&mut bytes, self.provider.id.as_bytes());
        append_part(&mut bytes, self.provider.version.as_bytes());
        append_part(
            &mut bytes,
            &u64::from(self.provider.contract_version).to_be_bytes(),
        );
        append_part(&mut bytes, self.provider.authority.as_str().as_bytes());
        append_part(&mut bytes, self.source_fingerprint.as_bytes());
        append_part(&mut bytes, self.coverage.as_str().as_bytes());
        for item in &self.upstream {
            append_part(&mut bytes, item.plane.as_str().as_bytes());
            append_part(&mut bytes, item.provider_id.as_bytes());
            append_part(&mut bytes, item.snapshot_fingerprint.as_bytes());
        }
        for artifact in &self.artifacts {
            append_part(&mut bytes, artifact.id.as_bytes());
            append_part(&mut bytes, artifact.project_key.as_bytes());
            append_part(&mut bytes, artifact.provider_id.as_bytes());
            append_part(&mut bytes, artifact.kind.as_bytes());
            append_part(&mut bytes, artifact.provider_key.as_bytes());
            append_part(&mut bytes, artifact.display_name.as_bytes());
            append_part(
                &mut bytes,
                artifact.path.as_deref().unwrap_or_default().as_bytes(),
            );
            append_part(&mut bytes, artifact.content_fingerprint.as_bytes());
        }
        for edge in &self.edges {
            append_part(&mut bytes, edge.source_id.as_bytes());
            append_part(&mut bytes, edge.target_id.as_bytes());
            append_part(&mut bytes, edge.kind.as_str().as_bytes());
        }
        for finding in &self.findings {
            append_part(&mut bytes, finding.code.as_bytes());
            append_part(&mut bytes, finding.severity.as_str().as_bytes());
            append_part(&mut bytes, finding.message.as_bytes());
            append_part(
                &mut bytes,
                finding
                    .artifact_id
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
            append_part(
                &mut bytes,
                finding.path.as_deref().unwrap_or_default().as_bytes(),
            );
        }
        fingerprint(&[&bytes])
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvidenceError {
    #[error("不支持 evidence protocol_version={actual}，当前版本为 {expected}")]
    UnsupportedProtocolVersion { actual: u32, expected: u32 },
    #[error("Evidence 字段无效：{0}")]
    InvalidField(&'static str),
    #[error("Evidence fingerprint 无效：{0}")]
    InvalidFingerprint(&'static str),
    #[error("Evidence snapshot fingerprint 不匹配：actual={actual}, expected={expected}")]
    FingerprintMismatch { actual: String, expected: String },
    #[error("{plane:?} plane 不允许依赖 {upstream:?} plane")]
    InvalidUpstreamPlane {
        plane: EvidencePlane,
        upstream: EvidencePlane,
    },
    #[error("Evidence upstream 重复")]
    DuplicateUpstream,
    #[error("Evidence collection 不是规范顺序：{0}")]
    NonCanonicalOrder(&'static str),
    #[error("Artifact 越过 project/provider 边界：{0}")]
    ArtifactBoundary(String),
    #[error("Artifact ID 不匹配 provider key：{0}")]
    ArtifactIdentity(String),
    #[error("Artifact ID 重复：{0}")]
    DuplicateArtifact(String),
    #[error("Artifact edge 存在悬空端点：{source_id} -> {target_id}")]
    DanglingEdge {
        source_id: String,
        target_id: String,
    },
    #[error("Artifact edge 重复")]
    DuplicateEdge,
    #[error("Finding 引用了未知 Artifact：{0}")]
    UnknownFindingArtifact(String),
    #[error("项目相对路径无效：{0:?}")]
    InvalidPath(String),
}

pub fn artifact_id(project_key: &str, provider_id: &str, provider_key: &str) -> String {
    format!(
        "artifact_v1_{}",
        stable_digest(&[
            project_key.as_bytes(),
            provider_id.as_bytes(),
            provider_key.as_bytes(),
        ])
    )
}

pub fn content_fingerprint(content: &[u8]) -> String {
    fingerprint(&[content])
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), EvidenceError> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(EvidenceError::InvalidField(field));
    }
    Ok(())
}

fn validate_fingerprint(field: &'static str, value: &str) -> Result<(), EvidenceError> {
    if value.len() < 8
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(EvidenceError::InvalidFingerprint(field));
    }
    Ok(())
}

fn valid_project_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    path == normalized
        && !normalized.is_empty()
        && !normalized.starts_with('/')
        && !normalized.contains(':')
        && !normalized
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}

fn fingerprint(parts: &[&[u8]]) -> String {
    format!("sha256_{}", stable_digest(parts))
}

fn stable_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

fn append_part(target: &mut Vec<u8>, part: &[u8]) {
    target.extend_from_slice(&(part.len() as u64).to_be_bytes());
    target.extend_from_slice(part);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(authority: EvidenceAuthority) -> EvidenceProvider {
        EvidenceProvider {
            id: "godot-engine-v1".to_owned(),
            version: "4.6.0".to_owned(),
            contract_version: 1,
            authority,
        }
    }

    fn artifact(key: &str, path: &str) -> ArtifactNode {
        ArtifactNode::from_provider_key(
            "project-a",
            "godot-engine-v1",
            "godot_scene",
            key,
            key,
            Some(path),
            path.as_bytes(),
        )
    }

    fn source_reference(fingerprint: &str) -> EvidenceReference {
        EvidenceReference {
            plane: EvidencePlane::Source,
            provider_id: "git-worktree".to_owned(),
            snapshot_fingerprint: fingerprint.to_owned(),
        }
    }

    #[test]
    fn snapshot_fingerprint_is_independent_of_input_order() {
        let first = artifact("first", "scenes/first.tscn");
        let second = artifact("second", "scenes/second.tscn");
        let edge = ArtifactEdge {
            source_id: first.id.clone(),
            target_id: second.id.clone(),
            kind: ArtifactEdgeKind::Instances,
        };
        let left = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_source",
            EvidenceCoverage::Complete,
            vec![source_reference("sha256_source-plane")],
            vec![first.clone(), second.clone()],
            vec![edge.clone()],
            Vec::new(),
        )
        .unwrap();
        let right = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_source",
            EvidenceCoverage::Complete,
            vec![source_reference("sha256_source-plane")],
            vec![second, first],
            vec![edge],
            Vec::new(),
        )
        .unwrap();

        assert_eq!(left.snapshot_fingerprint, right.snapshot_fingerprint);
        left.validate().unwrap();

        let mut non_canonical = left;
        non_canonical.artifacts.reverse();
        assert_eq!(
            non_canonical.validate(),
            Err(EvidenceError::NonCanonicalOrder("artifacts"))
        );
    }

    #[test]
    fn upstream_change_marks_downstream_evidence_stale() {
        let snapshot = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            vec![EvidenceReference {
                plane: EvidencePlane::Semantic,
                provider_id: "dotnet-scip".to_owned(),
                snapshot_fingerprint: "sha256_semantic-old".to_owned(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let current = [EvidenceReference {
            plane: EvidencePlane::Semantic,
            provider_id: "dotnet-scip".to_owned(),
            snapshot_fingerprint: "sha256_semantic-new".to_owned(),
        }];

        assert_eq!(
            snapshot.freshness(Some("sha256_worktree"), &current),
            EvidenceFreshness::Stale
        );
        assert_eq!(
            snapshot.freshness(None, &current),
            EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn only_fresh_complete_deterministic_errors_can_hard_block() {
        let finding = EvidenceFinding {
            code: "GODOT_MISSING_RESOURCE".to_owned(),
            severity: FindingSeverity::Error,
            message: "scene references a missing resource".to_owned(),
            artifact_id: None,
            path: Some("scenes/main.tscn".to_owned()),
        };
        let make = |authority, coverage| {
            EvidenceSnapshot::new(
                "project-a",
                EvidencePlane::Engine,
                provider(authority),
                "sha256_worktree",
                coverage,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                vec![finding.clone()],
            )
            .unwrap()
        };

        assert!(
            make(EvidenceAuthority::Deterministic, EvidenceCoverage::Complete)
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh)
        );
        assert!(
            !make(EvidenceAuthority::Heuristic, EvidenceCoverage::Complete)
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh)
        );
        assert!(
            !make(EvidenceAuthority::Deterministic, EvidenceCoverage::Partial)
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh)
        );
        assert!(
            !make(EvidenceAuthority::Deterministic, EvidenceCoverage::Complete)
                .finding_can_hard_block(&finding, EvidenceFreshness::Stale)
        );
    }

    #[test]
    fn artifact_graph_rejects_dangling_edges_and_cross_project_nodes() {
        let scene = artifact("main", "scenes/main.tscn");
        let dangling = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            Vec::new(),
            vec![scene.clone()],
            vec![ArtifactEdge {
                source_id: scene.id,
                target_id: "artifact_v1_missing".to_owned(),
                kind: ArtifactEdgeKind::References,
            }],
            Vec::new(),
        );
        assert!(matches!(dangling, Err(EvidenceError::DanglingEdge { .. })));

        let mut foreign = artifact("foreign", "scenes/foreign.tscn");
        foreign.project_key = "project-b".to_owned();
        let cross_project = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            Vec::new(),
            vec![foreign],
            Vec::new(),
            Vec::new(),
        );
        assert!(matches!(
            cross_project,
            Err(EvidenceError::ArtifactBoundary(_))
        ));

        let windows_path = artifact("windows", "scenes\\windows.tscn");
        assert!(matches!(
            EvidenceSnapshot::new(
                "project-a",
                EvidencePlane::Engine,
                provider(EvidenceAuthority::Deterministic),
                "sha256_worktree",
                EvidenceCoverage::Complete,
                Vec::new(),
                vec![windows_path],
                Vec::new(),
                Vec::new(),
            ),
            Err(EvidenceError::InvalidPath(_))
        ));
    }
}
