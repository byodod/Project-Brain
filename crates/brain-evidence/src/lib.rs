use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const EVIDENCE_PROTOCOL_VERSION: u32 = 1;
pub const INPUT_DEPENDENCY_CONTRACT_VERSION: u32 = 1;
pub const INPUT_PATH_MATCHER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidencePlane {
    Source,
    Semantic,
    Engine,
    Build,
    Test,
    Runtime,
}

impl EvidencePlane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Semantic => "semantic",
            Self::Engine => "engine",
            Self::Build => "build",
            Self::Test => "test",
            Self::Runtime => "runtime",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "semantic" => Some(Self::Semantic),
            "engine" => Some(Self::Engine),
            "build" => Some(Self::Build),
            "test" => Some(Self::Test),
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
            Self::Test => matches!(
                upstream,
                Self::Source | Self::Semantic | Self::Engine | Self::Build
            ),
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

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FindingAuthority {
    #[default]
    Advisory,
    DeterministicViolation,
}

impl FindingAuthority {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::DeterministicViolation => "deterministic_violation",
        }
    }
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
    /// Provider 开放 kind，例如 `engine_asset`、`dotnet_assembly` 或 `runtime_scenario`。
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
    #[serde(default, skip_serializing_if = "is_advisory_finding")]
    pub authority: FindingAuthority,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCoverage {
    Complete,
    Conservative,
    Incomplete,
}

impl DependencyCoverage {
    pub const fn hard_authority_eligible(self) -> bool {
        matches!(self, Self::Complete | Self::Conservative)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Conservative => "conservative",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InputRole {
    Source,
    Control,
    DependencyDeclaration,
    GeneratedInput,
}

impl InputRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Control => "control",
            Self::DependencyDeclaration => "dependency_declaration",
            Self::GeneratedInput => "generated_input",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InputPathUniverse {
    /// Git 已跟踪及未忽略的未跟踪文件，与 Source fingerprint v1 的边界一致。
    RepositoryVisible,
    /// selector 覆盖的项目文件系统；用于显式声明被 Provider 读取的 ignored/generated input。
    ProjectFilesystem,
}

impl InputPathUniverse {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RepositoryVisible => "repository_visible",
            Self::ProjectFilesystem => "project_filesystem",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathMatcherV1 {
    pub matcher_version: u32,
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl PathMatcherV1 {
    /// 建立使用 `/`、无 shell/环境变量/brace expansion 的有限 glob matcher。
    /// `**` 只允许作为完整 path segment；`*` 与 `?` 只匹配单个 segment。
    ///
    /// # Errors
    ///
    /// pattern 为空、越出 root、包含不支持的语法或重复时返回错误。
    pub fn new(mut include: Vec<String>, mut exclude: Vec<String>) -> Result<Self, EvidenceError> {
        include.sort();
        include.dedup();
        exclude.sort();
        exclude.dedup();
        let matcher = Self {
            matcher_version: INPUT_PATH_MATCHER_VERSION,
            include,
            exclude,
        };
        matcher.validate()?;
        Ok(matcher)
    }

    pub fn matches(&self, relative_path: &str) -> bool {
        valid_project_path(relative_path)
            && self
                .include
                .iter()
                .any(|pattern| glob_matches(pattern, relative_path))
            && !self
                .exclude
                .iter()
                .any(|pattern| glob_matches(pattern, relative_path))
    }

    fn validate(&self) -> Result<(), EvidenceError> {
        if self.matcher_version != INPUT_PATH_MATCHER_VERSION
            || self.include.is_empty()
            || self.include.windows(2).any(|pair| pair[0] >= pair[1])
            || self.exclude.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .include
                .iter()
                .chain(&self.exclude)
                .any(|pattern| !valid_glob_pattern(pattern))
        {
            return Err(EvidenceError::InvalidInputContract(
                "PathMatcherV1 非法或不是规范顺序".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputSelectorV1 {
    ExactPath {
        path: String,
        role: InputRole,
        presence_sensitive: bool,
    },
    Tree {
        /// 空字符串表示项目根；其它值必须是规范项目相对目录。
        root: String,
        universe: InputPathUniverse,
        matcher: PathMatcherV1,
        role: InputRole,
    },
}

impl InputSelectorV1 {
    pub fn matches_project_path(&self, path: &str) -> bool {
        if !valid_project_path(path) {
            return false;
        }
        match self {
            Self::ExactPath { path: exact, .. } => path == exact,
            Self::Tree { root, matcher, .. } => {
                tree_relative_path(root, path).is_some_and(|relative| matcher.matches(relative))
            }
        }
    }

    fn validate(&self) -> Result<(), EvidenceError> {
        match self {
            Self::ExactPath { path, .. } => {
                if !valid_project_path(path) {
                    return Err(EvidenceError::InvalidPath(path.clone()));
                }
            }
            Self::Tree { root, matcher, .. } => {
                if !root.is_empty() && !valid_project_path(root) {
                    return Err(EvidenceError::InvalidPath(root.clone()));
                }
                matcher.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputDependencyContractV1 {
    pub contract_version: u32,
    pub project_key: String,
    pub profile_id: String,
    pub provider_contract_id: String,
    pub provider_contract_version: u32,
    pub profile_contract_hash: String,
    pub dependency_contract_hash: String,
    pub selectors: Vec<InputSelectorV1>,
    pub coverage: DependencyCoverage,
}

impl InputDependencyContractV1 {
    /// 建立规范排序、不可由调用者伪造 hash 的 Provider/Profile 输入合同。
    ///
    /// # Errors
    ///
    /// 身份、hash 或 selector 非法时返回错误。
    pub fn new(
        project_key: &str,
        profile_id: &str,
        provider_contract_id: &str,
        provider_contract_version: u32,
        profile_contract_hash: &str,
        mut selectors: Vec<InputSelectorV1>,
        coverage: DependencyCoverage,
    ) -> Result<Self, EvidenceError> {
        selectors.sort();
        selectors.dedup();
        let mut contract = Self {
            contract_version: INPUT_DEPENDENCY_CONTRACT_VERSION,
            project_key: project_key.to_owned(),
            profile_id: profile_id.to_owned(),
            provider_contract_id: provider_contract_id.to_owned(),
            provider_contract_version,
            profile_contract_hash: profile_contract_hash.to_owned(),
            dependency_contract_hash: String::new(),
            selectors,
            coverage,
        };
        contract.validate_without_hash()?;
        contract.dependency_contract_hash = contract.computed_hash();
        Ok(contract)
    }

    /// 重新验证规范字段和内容派生 hash。
    ///
    /// # Errors
    ///
    /// 合同字段、selector、规范顺序或派生 hash 不匹配时返回错误。
    pub fn validate(&self) -> Result<(), EvidenceError> {
        self.validate_without_hash()?;
        let expected = self.computed_hash();
        if self.dependency_contract_hash != expected {
            return Err(EvidenceError::InputContractHashMismatch {
                actual: self.dependency_contract_hash.clone(),
                expected,
            });
        }
        Ok(())
    }

    pub fn matches_project_path(&self, path: &str) -> bool {
        self.selectors
            .iter()
            .any(|selector| selector.matches_project_path(path))
    }

    fn validate_without_hash(&self) -> Result<(), EvidenceError> {
        if self.contract_version != INPUT_DEPENDENCY_CONTRACT_VERSION
            || self.provider_contract_version == 0
            || self.selectors.is_empty()
        {
            return Err(EvidenceError::InvalidInputContract(
                "合同版本、Provider 合同版本或 selector 为空".to_owned(),
            ));
        }
        validate_identifier("input.project_key", &self.project_key, 128)?;
        validate_identifier("input.profile_id", &self.profile_id, 128)?;
        validate_identifier(
            "input.provider_contract_id",
            &self.provider_contract_id,
            128,
        )?;
        validate_fingerprint("input.profile_contract_hash", &self.profile_contract_hash)?;
        if self.selectors.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(EvidenceError::NonCanonicalOrder("input.selectors"));
        }
        for selector in &self.selectors {
            selector.validate()?;
        }
        Ok(())
    }

    fn computed_hash(&self) -> String {
        let mut bytes = Vec::new();
        append_part(&mut bytes, b"project-brain/input-dependency-contract/v1");
        append_part(&mut bytes, &u64::from(self.contract_version).to_be_bytes());
        append_part(&mut bytes, self.project_key.as_bytes());
        append_part(&mut bytes, self.profile_id.as_bytes());
        append_part(&mut bytes, self.provider_contract_id.as_bytes());
        append_part(
            &mut bytes,
            &u64::from(self.provider_contract_version).to_be_bytes(),
        );
        append_part(&mut bytes, self.profile_contract_hash.as_bytes());
        append_part(&mut bytes, self.coverage.as_str().as_bytes());
        for selector in &self.selectors {
            match selector {
                InputSelectorV1::ExactPath {
                    path,
                    role,
                    presence_sensitive,
                } => {
                    append_part(&mut bytes, b"exact_path");
                    append_part(&mut bytes, path.as_bytes());
                    append_part(&mut bytes, role.as_str().as_bytes());
                    append_part(&mut bytes, &[u8::from(*presence_sensitive)]);
                }
                InputSelectorV1::Tree {
                    root,
                    universe,
                    matcher,
                    role,
                } => {
                    append_part(&mut bytes, b"tree");
                    append_part(&mut bytes, root.as_bytes());
                    append_part(&mut bytes, universe.as_str().as_bytes());
                    append_part(&mut bytes, role.as_str().as_bytes());
                    append_part(
                        &mut bytes,
                        &u64::from(matcher.matcher_version).to_be_bytes(),
                    );
                    for pattern in &matcher.include {
                        append_part(&mut bytes, b"include");
                        append_part(&mut bytes, pattern.as_bytes());
                    }
                    for pattern in &matcher.exclude {
                        append_part(&mut bytes, b"exclude");
                        append_part(&mut bytes, pattern.as_bytes());
                    }
                }
            }
        }
        content_fingerprint(&bytes)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum InputPathState {
    PresentRegularFile,
    Absent,
}

impl InputPathState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PresentRegularFile => "present_regular_file",
            Self::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputManifestEntry {
    pub path: String,
    pub state: InputPathState,
    pub role: InputRole,
    pub content_sha256: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceInputManifestV1 {
    pub manifest_version: u32,
    pub contract: InputDependencyContractV1,
    pub source_fingerprint_at_creation: String,
    pub manifest_hash: String,
    pub entries: Vec<InputManifestEntry>,
}

impl EvidenceInputManifestV1 {
    /// 建立 selector 在一个稳定 Source state 上解析出的不可变输入清单。
    ///
    /// # Errors
    ///
    /// 合同、Source fingerprint、entry 状态或规范顺序非法时返回错误。
    pub fn new(
        contract: InputDependencyContractV1,
        source_fingerprint_at_creation: &str,
        mut entries: Vec<InputManifestEntry>,
    ) -> Result<Self, EvidenceError> {
        entries.sort();
        entries.dedup();
        let mut manifest = Self {
            manifest_version: 1,
            contract,
            source_fingerprint_at_creation: source_fingerprint_at_creation.to_owned(),
            manifest_hash: String::new(),
            entries,
        };
        manifest.validate_without_hash()?;
        manifest.manifest_hash = manifest.computed_hash();
        Ok(manifest)
    }

    /// 重新验证输入条目、合同和内容派生 hash。
    ///
    /// # Errors
    ///
    /// Manifest 版本、条目状态、规范顺序或派生 hash 不匹配时返回错误。
    pub fn validate(&self) -> Result<(), EvidenceError> {
        self.validate_without_hash()?;
        let expected = self.computed_hash();
        if self.manifest_hash != expected {
            return Err(EvidenceError::InputManifestHashMismatch {
                actual: self.manifest_hash.clone(),
                expected,
            });
        }
        Ok(())
    }

    fn validate_without_hash(&self) -> Result<(), EvidenceError> {
        if self.manifest_version != 1 {
            return Err(EvidenceError::InvalidInputManifest(
                "manifest_version 必须为 1".to_owned(),
            ));
        }
        self.contract.validate()?;
        validate_fingerprint(
            "input.source_fingerprint_at_creation",
            &self.source_fingerprint_at_creation,
        )?;
        if self.entries.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(EvidenceError::NonCanonicalOrder("input.entries"));
        }
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            if !valid_project_path(&entry.path) || !paths.insert(entry.path.as_str()) {
                return Err(EvidenceError::InvalidPath(entry.path.clone()));
            }
            match entry.state {
                InputPathState::PresentRegularFile => {
                    let hash = entry.content_sha256.as_deref().ok_or_else(|| {
                        EvidenceError::InvalidInputManifest(format!(
                            "present entry={:?} 缺少 content_sha256",
                            entry.path
                        ))
                    })?;
                    validate_fingerprint("input.entry.content_sha256", hash)?;
                    if entry.size.is_none() {
                        return Err(EvidenceError::InvalidInputManifest(format!(
                            "present entry={:?} 缺少 size",
                            entry.path
                        )));
                    }
                }
                InputPathState::Absent => {
                    if entry.content_sha256.is_some() || entry.size.is_some() {
                        return Err(EvidenceError::InvalidInputManifest(format!(
                            "absent entry={:?} 不得携带 hash/size",
                            entry.path
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn computed_hash(&self) -> String {
        let mut bytes = Vec::new();
        append_part(&mut bytes, b"project-brain/evidence-input-manifest/v1");
        append_part(
            &mut bytes,
            self.contract.dependency_contract_hash.as_bytes(),
        );
        append_part(&mut bytes, self.contract.profile_contract_hash.as_bytes());
        for entry in &self.entries {
            append_part(&mut bytes, entry.path.as_bytes());
            append_part(&mut bytes, entry.state.as_str().as_bytes());
            append_part(&mut bytes, entry.role.as_str().as_bytes());
            append_part(
                &mut bytes,
                entry
                    .content_sha256
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
            append_part(&mut bytes, &entry.size.unwrap_or_default().to_be_bytes());
        }
        content_fingerprint(&bytes)
    }
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
                left.authority,
                &left.artifact_id,
                &left.path,
                &left.message,
            )
                .cmp(&(
                    &right.code,
                    right.severity,
                    right.authority,
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
        explicitly_mapped: bool,
    ) -> bool {
        explicitly_mapped
            && self.provider.authority == EvidenceAuthority::Deterministic
            && self.coverage == EvidenceCoverage::Complete
            && freshness == EvidenceFreshness::Fresh
            && finding.severity == FindingSeverity::Error
            && finding.authority == FindingAuthority::DeterministicViolation
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
                pair[0].authority,
                &pair[0].artifact_id,
                &pair[0].path,
                &pair[0].message,
            ) > (
                &pair[1].code,
                pair[1].severity,
                pair[1].authority,
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
            if finding.authority != FindingAuthority::Advisory {
                append_part(&mut bytes, finding.authority.as_str().as_bytes());
            }
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

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a shared-reference predicate"
)]
fn is_advisory_finding(authority: &FindingAuthority) -> bool {
    *authority == FindingAuthority::Advisory
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
    #[error("Evidence 输入依赖合同无效：{0}")]
    InvalidInputContract(String),
    #[error("Evidence 输入依赖合同 hash 不匹配：actual={actual}, expected={expected}")]
    InputContractHashMismatch { actual: String, expected: String },
    #[error("Evidence 输入清单无效：{0}")]
    InvalidInputManifest(String),
    #[error("Evidence 输入清单 hash 不匹配：actual={actual}, expected={expected}")]
    InputManifestHashMismatch { actual: String, expected: String },
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

fn valid_glob_pattern(pattern: &str) -> bool {
    if pattern.is_empty()
        || pattern.contains(['\\', ':', '\0'])
        || pattern.starts_with('/')
        || pattern.contains('{')
        || pattern.contains('}')
        || pattern.contains('[')
        || pattern.contains(']')
        || pattern.contains('$')
    {
        return false;
    }
    pattern.split('/').all(|segment| {
        !segment.is_empty()
            && !matches!(segment, "." | "..")
            && (!segment.contains("**") || segment == "**")
            && segment.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'#' | b'@' | b'*' | b'?')
            })
    })
}

fn tree_relative_path<'a>(root: &str, path: &'a str) -> Option<&'a str> {
    if root.is_empty() {
        return Some(path);
    }
    if path == root {
        return None;
    }
    path.strip_prefix(root)?.strip_prefix('/')
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.split('/').collect::<Vec<_>>();
    let path = path.split('/').collect::<Vec<_>>();
    let mut memo = BTreeMap::new();
    glob_segments_match(&pattern, &path, 0, 0, &mut memo)
}

fn glob_segments_match(
    pattern: &[&str],
    path: &[&str],
    pattern_index: usize,
    path_index: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(cached) = memo.get(&(pattern_index, path_index)) {
        return *cached;
    }
    let matched = if pattern_index == pattern.len() {
        path_index == path.len()
    } else if pattern[pattern_index] == "**" {
        glob_segments_match(pattern, path, pattern_index + 1, path_index, memo)
            || (path_index < path.len()
                && glob_segments_match(pattern, path, pattern_index, path_index + 1, memo))
    } else {
        path_index < path.len()
            && glob_segment_matches(pattern[pattern_index], path[path_index])
            && glob_segments_match(pattern, path, pattern_index + 1, path_index + 1, memo)
    };
    memo.insert((pattern_index, path_index), matched);
    matched
}

fn glob_segment_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut current = vec![false; value.len() + 1];
        match token {
            b'*' => {
                current[0] = previous[0];
                for index in 1..=value.len() {
                    current[index] = previous[index] || current[index - 1];
                }
            }
            b'?' => {
                current[1..=value.len()].copy_from_slice(&previous[..value.len()]);
            }
            literal => {
                for index in 1..=value.len() {
                    current[index] = previous[index - 1] && value[index - 1] == *literal;
                }
            }
        }
        previous = current;
    }
    previous[value.len()]
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

    fn input_contract() -> InputDependencyContractV1 {
        InputDependencyContractV1::new(
            "project-a",
            "python-main",
            "python-compile",
            1,
            "sha256_profile",
            vec![
                InputSelectorV1::ExactPath {
                    path: "pyproject.toml".to_owned(),
                    role: InputRole::DependencyDeclaration,
                    presence_sensitive: true,
                },
                InputSelectorV1::Tree {
                    root: "src".to_owned(),
                    universe: InputPathUniverse::RepositoryVisible,
                    matcher: PathMatcherV1::new(
                        vec!["**/*.py".to_owned(), "*.py".to_owned()],
                        vec!["generated/**".to_owned()],
                    )
                    .unwrap(),
                    role: InputRole::Source,
                },
            ],
            DependencyCoverage::Complete,
        )
        .unwrap()
    }

    fn provider(authority: EvidenceAuthority) -> EvidenceProvider {
        EvidenceProvider {
            id: "engine-provider-v1".to_owned(),
            version: "4.6.0".to_owned(),
            contract_version: 1,
            authority,
        }
    }

    fn artifact(key: &str, path: &str) -> ArtifactNode {
        ArtifactNode::from_provider_key(
            "project-a",
            "engine-provider-v1",
            "engine_asset",
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
    fn input_contract_is_canonical_and_matches_added_paths() {
        let contract = input_contract();
        contract.validate().unwrap();
        assert!(contract.matches_project_path("pyproject.toml"));
        assert!(contract.matches_project_path("src/main.py"));
        assert!(contract.matches_project_path("src/pkg/module.py"));
        assert!(!contract.matches_project_path("src/generated/output.py"));
        assert!(!contract.matches_project_path("docs/readme.md"));

        let reordered = InputDependencyContractV1::new(
            "project-a",
            "python-main",
            "python-compile",
            1,
            "sha256_profile",
            contract.selectors.iter().cloned().rev().collect(),
            DependencyCoverage::Complete,
        )
        .unwrap();
        assert_eq!(contract, reordered);
    }

    #[test]
    fn input_manifest_hash_covers_absence_content_and_contract() {
        let contract = input_contract();
        let manifest = EvidenceInputManifestV1::new(
            contract.clone(),
            "sha256_source",
            vec![
                InputManifestEntry {
                    path: "src/main.py".to_owned(),
                    state: InputPathState::PresentRegularFile,
                    role: InputRole::Source,
                    content_sha256: Some("sha256_content".to_owned()),
                    size: Some(12),
                },
                InputManifestEntry {
                    path: "pyproject.toml".to_owned(),
                    state: InputPathState::Absent,
                    role: InputRole::DependencyDeclaration,
                    content_sha256: None,
                    size: None,
                },
            ],
        )
        .unwrap();
        manifest.validate().unwrap();

        let present = EvidenceInputManifestV1::new(
            contract,
            "sha256_source",
            vec![
                InputManifestEntry {
                    path: "pyproject.toml".to_owned(),
                    state: InputPathState::PresentRegularFile,
                    role: InputRole::DependencyDeclaration,
                    content_sha256: Some("sha256_config".to_owned()),
                    size: Some(1),
                },
                InputManifestEntry {
                    path: "src/main.py".to_owned(),
                    state: InputPathState::PresentRegularFile,
                    role: InputRole::Source,
                    content_sha256: Some("sha256_content".to_owned()),
                    size: Some(12),
                },
            ],
        )
        .unwrap();
        assert_ne!(manifest.manifest_hash, present.manifest_hash);
    }

    #[test]
    fn matcher_rejects_ambiguous_or_shell_like_patterns() {
        for invalid in ["", "../*.rs", "src/**x.rs", "src/{a,b}.rs", "$ROOT/**"] {
            assert!(PathMatcherV1::new(vec![invalid.to_owned()], Vec::new()).is_err());
        }
        assert!(PathMatcherV1::new(vec!["**/*.rs".to_owned()], Vec::new()).is_ok());
    }

    #[test]
    fn snapshot_fingerprint_is_independent_of_input_order() {
        let first = artifact("first", "assets/first.asset");
        let second = artifact("second", "assets/second.asset");
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
            code: "MISSING_RESOURCE".to_owned(),
            severity: FindingSeverity::Error,
            authority: FindingAuthority::DeterministicViolation,
            message: "scene references a missing resource".to_owned(),
            artifact_id: None,
            path: Some("assets/main.asset".to_owned()),
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
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh, true)
        );
        assert!(
            !make(EvidenceAuthority::Deterministic, EvidenceCoverage::Complete)
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh, false)
        );
        let mut advisory = finding.clone();
        advisory.authority = FindingAuthority::Advisory;
        let advisory_snapshot = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Engine,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![advisory.clone()],
        )
        .unwrap();
        assert!(!advisory_snapshot.finding_can_hard_block(
            &advisory,
            EvidenceFreshness::Fresh,
            true
        ));
        assert!(
            !make(EvidenceAuthority::Heuristic, EvidenceCoverage::Complete).finding_can_hard_block(
                &finding,
                EvidenceFreshness::Fresh,
                true
            )
        );
        assert!(
            !make(EvidenceAuthority::Deterministic, EvidenceCoverage::Partial)
                .finding_can_hard_block(&finding, EvidenceFreshness::Fresh, true)
        );
        assert!(
            !make(EvidenceAuthority::Deterministic, EvidenceCoverage::Complete)
                .finding_can_hard_block(&finding, EvidenceFreshness::Stale, true)
        );
    }

    #[test]
    fn test_is_an_independent_plane_with_explicit_upstream_contract() {
        let upstream = [
            EvidencePlane::Source,
            EvidencePlane::Semantic,
            EvidencePlane::Engine,
            EvidencePlane::Build,
        ]
        .into_iter()
        .map(|plane| EvidenceReference {
            plane,
            provider_id: format!("{}-provider", plane.as_str()),
            snapshot_fingerprint: format!("sha256_{}", plane.as_str()),
        })
        .collect();
        let snapshot = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Test,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            upstream,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(snapshot.plane, EvidencePlane::Test);

        let invalid = EvidenceSnapshot::new(
            "project-a",
            EvidencePlane::Build,
            provider(EvidenceAuthority::Deterministic),
            "sha256_worktree",
            EvidenceCoverage::Complete,
            vec![EvidenceReference {
                plane: EvidencePlane::Test,
                provider_id: "test-provider".to_owned(),
                snapshot_fingerprint: "sha256_test".to_owned(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(matches!(
            invalid,
            Err(EvidenceError::InvalidUpstreamPlane { .. })
        ));
    }

    #[test]
    fn artifact_graph_rejects_dangling_edges_and_cross_project_nodes() {
        let scene = artifact("main", "assets/main.asset");
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

        let mut foreign = artifact("foreign", "assets/foreign.asset");
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

        let windows_path = artifact("windows", "assets\\windows.asset");
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
