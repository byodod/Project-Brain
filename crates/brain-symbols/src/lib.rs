use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SYMBOL_PROTOCOL_VERSION: u32 = 2;
const HIGH_CONFIDENCE_RENAME_BASIS_POINTS: u16 = 5_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SourceLanguage(String);

impl SourceLanguage {
    pub fn rust() -> Self {
        Self("rust".to_owned())
    }

    pub fn csharp() -> Self {
        Self("csharp".to_owned())
    }

    pub fn python() -> Self {
        Self("python".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        (!normalized.is_empty()
            && normalized.len() <= 64
            && normalized.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'+' | b'#')
            }))
        .then_some(Self(normalized))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IdentityQuality {
    /// 仅由语法、路径和声明名推导；rename/move 后不得假定身份延续。
    SyntaxFallback,
    /// 来自语言语义 Provider；具体稳定性仍由 provider contract 声明。
    Semantic,
}

impl IdentityQuality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntaxFallback => "syntax_fallback",
            Self::Semantic => "semantic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "syntax_fallback" => Some(Self::SyntaxFallback),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SymbolStatus {
    Active,
    Removed,
}

impl SymbolStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Removed => "removed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    References,
    Calls,
    Imports,
    Implements,
    TypeDefinition,
    Reads,
    Writes,
}

impl EdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::References => "references",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Implements => "implements",
            Self::TypeDefinition => "type_definition",
            Self::Reads => "reads",
            Self::Writes => "writes",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// Provider key 语义发生破坏性变化时必须更换 ID，而不是只增加 version。
    pub id: String,
    /// 实现或工具链版本；不参与 Symbol ID，以允许兼容升级保持身份。
    pub version: String,
    pub identity_quality: IdentityQuality,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolNode {
    /// Provider key 的稳定摘要。它不是跨 Provider 的全局语义真相。
    pub id: String,
    /// 项目的稳定身份。符号身份不得跨项目隐式合并。
    pub project_key: String,
    pub provider_id: String,
    pub identity_quality: IdentityQuality,
    pub language: SourceLanguage,
    pub kind: String,
    pub provider_key: String,
    pub display_name: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content_fingerprint: String,
    pub status: SymbolStatus,
}

#[derive(Debug, Clone)]
pub struct SymbolNodeInput<'a> {
    pub language: SourceLanguage,
    pub kind: &'a str,
    pub provider_key: &'a str,
    pub display_name: &'a str,
    pub path: &'a str,
    pub start_line: usize,
    pub end_line: usize,
    pub content: &'a [u8],
}

impl SymbolNode {
    pub fn from_provider_key(
        project_key: &str,
        provider: &ProviderDescriptor,
        input: SymbolNodeInput<'_>,
    ) -> Self {
        let id = symbol_id(project_key, &provider.id, input.provider_key);
        Self {
            id,
            project_key: project_key.to_owned(),
            provider_id: provider.id.clone(),
            identity_quality: provider.identity_quality,
            language: input.language,
            kind: input.kind.to_owned(),
            provider_key: input.provider_key.to_owned(),
            display_name: input.display_name.to_owned(),
            path: input.path.to_owned(),
            start_line: input.start_line,
            end_line: input.end_line,
            content_fingerprint: format!("sha256_{}", stable_digest(&[input.content])),
            status: SymbolStatus::Active,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolEdge {
    pub project_key: String,
    pub provider_id: String,
    pub source_id: String,
    pub target_id: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFileState {
    pub path: String,
    pub language: SourceLanguage,
    pub content_fingerprint: String,
    pub has_syntax_errors: bool,
}

impl SourceFileState {
    pub fn from_source(
        path: &str,
        language: SourceLanguage,
        source: &[u8],
        has_syntax_errors: bool,
    ) -> Self {
        Self {
            path: path.to_owned(),
            language,
            content_fingerprint: format!("sha256_{}", stable_digest(&[source])),
            has_syntax_errors,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolSnapshot {
    pub protocol_version: u32,
    pub project_key: String,
    pub provider: ProviderDescriptor,
    pub source_revision: String,
    pub sources: Vec<SourceFileState>,
    pub symbols: Vec<SymbolNode>,
    pub edges: Vec<SymbolEdge>,
}

impl SymbolSnapshot {
    pub fn for_worktree(
        project_key: &str,
        provider: ProviderDescriptor,
        head_revision: &str,
        mut sources: Vec<SourceFileState>,
        mut symbols: Vec<SymbolNode>,
        mut edges: Vec<SymbolEdge>,
    ) -> Self {
        sources.sort_by(|left, right| {
            (&left.path, &left.language).cmp(&(&right.path, &right.language))
        });
        symbols.sort_by(|left, right| left.id.cmp(&right.id));
        edges.sort_by(|left, right| {
            (&left.source_id, &left.target_id, left.kind).cmp(&(
                &right.source_id,
                &right.target_id,
                right.kind,
            ))
        });
        let mut revision_material = Vec::new();
        append_digest_part(
            &mut revision_material,
            &u64::from(SYMBOL_PROTOCOL_VERSION).to_be_bytes(),
        );
        append_digest_part(&mut revision_material, project_key.as_bytes());
        append_digest_part(&mut revision_material, head_revision.as_bytes());
        append_digest_part(&mut revision_material, provider.id.as_bytes());
        append_digest_part(&mut revision_material, provider.version.as_bytes());
        append_digest_part(
            &mut revision_material,
            provider.identity_quality.as_str().as_bytes(),
        );
        for source in &sources {
            append_digest_part(&mut revision_material, source.path.as_bytes());
            append_digest_part(&mut revision_material, source.language.as_str().as_bytes());
            append_digest_part(
                &mut revision_material,
                source.content_fingerprint.as_bytes(),
            );
            append_digest_part(
                &mut revision_material,
                &[u8::from(source.has_syntax_errors)],
            );
        }
        for symbol in &symbols {
            append_digest_part(&mut revision_material, symbol.id.as_bytes());
            append_digest_part(&mut revision_material, symbol.project_key.as_bytes());
            append_digest_part(&mut revision_material, symbol.provider_id.as_bytes());
            append_digest_part(
                &mut revision_material,
                symbol.identity_quality.as_str().as_bytes(),
            );
            append_digest_part(&mut revision_material, symbol.language.as_str().as_bytes());
            append_digest_part(&mut revision_material, symbol.kind.as_bytes());
            append_digest_part(&mut revision_material, symbol.provider_key.as_bytes());
            append_digest_part(&mut revision_material, symbol.display_name.as_bytes());
            append_digest_part(
                &mut revision_material,
                symbol.content_fingerprint.as_bytes(),
            );
            append_digest_part(&mut revision_material, symbol.path.as_bytes());
            append_digest_part(
                &mut revision_material,
                &u64::try_from(symbol.start_line)
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            append_digest_part(
                &mut revision_material,
                &u64::try_from(symbol.end_line)
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            append_digest_part(&mut revision_material, symbol.status.as_str().as_bytes());
        }
        for edge in &edges {
            append_digest_part(&mut revision_material, edge.project_key.as_bytes());
            append_digest_part(&mut revision_material, edge.provider_id.as_bytes());
            append_digest_part(&mut revision_material, edge.source_id.as_bytes());
            append_digest_part(&mut revision_material, edge.target_id.as_bytes());
            append_digest_part(&mut revision_material, edge.kind.as_str().as_bytes());
        }
        let source_revision = format!("worktree_v3_{}", stable_digest(&[&revision_material]));
        Self {
            protocol_version: SYMBOL_PROTOCOL_VERSION,
            project_key: project_key.to_owned(),
            provider,
            source_revision,
            sources,
            symbols,
            edges,
        }
    }
}

pub fn encode_provider_key(parts: &[&str]) -> String {
    let mut encoded = String::new();
    for part in parts {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{}:{part}", part.len());
    }
    encoded
}

pub fn symbol_id(project_key: &str, provider_id: &str, provider_key: &str) -> String {
    format!(
        "sym_v2_{}",
        stable_digest(&[
            project_key.as_bytes(),
            provider_id.as_bytes(),
            provider_key.as_bytes(),
        ])
    )
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GraphDelta {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub removed: u64,
}

pub const LINEAGE_ALGORITHM_ID: &str = "project-brain-lineage";
pub const LINEAGE_ALGORITHM_VERSION: &str = "1";
pub const LINEAGE_EVIDENCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineageConfidence {
    Low,
    Medium,
    High,
}

impl LineageConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineageState {
    Proposed,
    Confirmed,
    Rejected,
    Superseded,
    Invalidated,
}

impl LineageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Invalidated => "invalidated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "confirmed" => Some(Self::Confirmed),
            "rejected" => Some(Self::Rejected),
            "superseded" => Some(Self::Superseded),
            "invalidated" => Some(Self::Invalidated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineageEvidence {
    ProviderSymbolEqual,
    KindEqual,
    NormalizedDefinitionEqual,
    DisplayNameChanged,
    PathChanged,
    GitPathRename {
        old_path: String,
        new_path: String,
        similarity_basis_points: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageSymbolObservation {
    pub project_key: String,
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language: SourceLanguage,
    pub snapshot_revision: String,
    pub symbol_id: String,
    pub provider_symbol: Option<String>,
    pub is_local: bool,
    pub kind: String,
    pub display_name: String,
    pub path: String,
    pub normalized_definition_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathRenameEvidence {
    pub old_path: String,
    pub new_path: String,
    pub similarity_basis_points: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageCandidateProposal {
    pub project_key: String,
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language: SourceLanguage,
    pub from_snapshot: String,
    pub from_symbol: String,
    pub to_snapshot: String,
    pub to_symbol: String,
    pub ambiguity_group_id: Option<String>,
    pub algorithm_id: String,
    pub algorithm_version: String,
    pub confidence: LineageConfidence,
    pub input_fingerprint: String,
    pub evidence_fingerprint: String,
    pub evidence: Vec<LineageEvidence>,
}

/// 对相邻快照的 removed/inserted 观察生成可审计 lineage 候选。
///
/// 该函数永远不会自动确认候选、复用 symbol ID 或改写 tombstone。
pub fn propose_lineage_candidates(
    previous: &[LineageSymbolObservation],
    current: &[LineageSymbolObservation],
    path_renames: &[PathRenameEvidence],
) -> Vec<LineageCandidateProposal> {
    let previous_ids = previous
        .iter()
        .map(|observation| observation.symbol_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let current_ids = current
        .iter()
        .map(|observation| observation.symbol_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut pairs = Vec::new();
    for (old_index, old) in previous.iter().enumerate() {
        if current_ids.contains(old.symbol_id.as_str()) {
            continue;
        }
        for (new_index, new) in current.iter().enumerate() {
            if !previous_ids.contains(new.symbol_id.as_str())
                && !old.is_local
                && !new.is_local
                && old.project_key == new.project_key
                && old.provider_profile_id == new.provider_profile_id
                && old.provider_contract_id == new.provider_contract_id
                && old.language == new.language
                && old.kind == new.kind
                && old.normalized_definition_fingerprint == new.normalized_definition_fingerprint
            {
                pairs.push((old_index, new_index));
            }
        }
    }
    let old_matches = match_counts(&pairs, true);
    let new_matches = match_counts(&pairs, false);
    let mut candidates = pairs
        .into_iter()
        .map(|(old_index, new_index)| {
            build_lineage_candidate_proposal(
                &previous[old_index],
                &current[new_index],
                path_renames,
                old_matches.get(&old_index).copied().unwrap_or_default() > 1
                    || new_matches.get(&new_index).copied().unwrap_or_default() > 1,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        (&left.from_symbol, &left.to_symbol).cmp(&(&right.from_symbol, &right.to_symbol))
    });
    candidates
}

fn match_counts(
    pairs: &[(usize, usize)],
    use_old_index: bool,
) -> std::collections::BTreeMap<usize, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for &(old_index, new_index) in pairs {
        *counts
            .entry(if use_old_index { old_index } else { new_index })
            .or_default() += 1;
    }
    counts
}

fn build_lineage_candidate_proposal(
    old: &LineageSymbolObservation,
    new: &LineageSymbolObservation,
    path_renames: &[PathRenameEvidence],
    ambiguous: bool,
) -> LineageCandidateProposal {
    let mut evidence = vec![
        LineageEvidence::KindEqual,
        LineageEvidence::NormalizedDefinitionEqual,
    ];
    if !old.is_local
        && !new.is_local
        && old.provider_symbol.is_some()
        && old.provider_symbol == new.provider_symbol
    {
        evidence.push(LineageEvidence::ProviderSymbolEqual);
    }
    if old.display_name != new.display_name {
        evidence.push(LineageEvidence::DisplayNameChanged);
    }
    let git_rename = path_renames
        .iter()
        .filter(|rename| {
            old.path != new.path
                && rename.similarity_basis_points <= 10_000
                && rename.old_path == old.path
                && rename.new_path == new.path
        })
        .max_by_key(|rename| rename.similarity_basis_points);
    if old.path != new.path {
        evidence.push(LineageEvidence::PathChanged);
    }
    if let Some(rename) = git_rename {
        evidence.push(LineageEvidence::GitPathRename {
            old_path: rename.old_path.clone(),
            new_path: rename.new_path.clone(),
            similarity_basis_points: rename.similarity_basis_points,
        });
    }
    let confidence = if ambiguous {
        LineageConfidence::Low
    } else if git_rename
        .is_some_and(|rename| rename.similarity_basis_points >= HIGH_CONFIDENCE_RENAME_BASIS_POINTS)
        || evidence
            .iter()
            .any(|item| item == &LineageEvidence::ProviderSymbolEqual)
        || (old.path == new.path && old.display_name != new.display_name)
    {
        LineageConfidence::High
    } else {
        LineageConfidence::Medium
    };
    let input_fingerprint = format!(
        "sha256_{}",
        stable_digest(&[
            old.project_key.as_bytes(),
            old.provider_profile_id.as_bytes(),
            old.provider_contract_id.as_bytes(),
            old.language.as_str().as_bytes(),
            old.snapshot_revision.as_bytes(),
            old.symbol_id.as_bytes(),
            new.snapshot_revision.as_bytes(),
            new.symbol_id.as_bytes(),
        ])
    );
    let evidence_json = serde_json::to_vec(&evidence).expect("lineage evidence is serializable");
    let evidence_fingerprint = format!("sha256_{}", stable_digest(&[&evidence_json]));
    let ambiguity_group_id = ambiguous.then(|| {
        format!(
            "ambiguity_v1_{}",
            stable_digest(&[
                old.project_key.as_bytes(),
                old.provider_profile_id.as_bytes(),
                old.provider_contract_id.as_bytes(),
                old.language.as_str().as_bytes(),
                old.snapshot_revision.as_bytes(),
                new.snapshot_revision.as_bytes(),
            ])
        )
    });
    LineageCandidateProposal {
        project_key: old.project_key.clone(),
        provider_profile_id: old.provider_profile_id.clone(),
        provider_contract_id: old.provider_contract_id.clone(),
        language: old.language.clone(),
        from_snapshot: old.snapshot_revision.clone(),
        from_symbol: old.symbol_id.clone(),
        to_snapshot: new.snapshot_revision.clone(),
        to_symbol: new.symbol_id.clone(),
        ambiguity_group_id,
        algorithm_id: LINEAGE_ALGORITHM_ID.to_owned(),
        algorithm_version: LINEAGE_ALGORITHM_VERSION.to_owned(),
        confidence,
        input_fingerprint,
        evidence_fingerprint,
        evidence,
    }
}

fn stable_digest(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        update_digest(&mut hasher, part);
    }
    format!("{:x}", hasher.finalize())
}

fn append_digest_part(target: &mut Vec<u8>, part: &[u8]) {
    target.extend_from_slice(&(part.len() as u64).to_be_bytes());
    target.extend_from_slice(part);
}

fn update_digest(hasher: &mut Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

#[cfg(test)]
mod tests {
    use super::{
        IdentityQuality, LineageConfidence, LineageEvidence, LineageSymbolObservation,
        PathRenameEvidence, ProviderDescriptor, SourceFileState, SourceLanguage, SymbolNode,
        SymbolNodeInput, SymbolStatus, encode_provider_key, propose_lineage_candidates,
    };

    const PROJECT_KEY: &str = "project_alpha";

    fn provider() -> ProviderDescriptor {
        ProviderDescriptor {
            id: "tree-sitter-rust-syntax".to_owned(),
            version: "1".to_owned(),
            identity_quality: IdentityQuality::SyntaxFallback,
        }
    }

    #[test]
    fn same_provider_key_has_a_repeatable_id() {
        let key = encode_provider_key(&["src/lib.rs", "function_item", "run"]);
        let left = SymbolNode::from_provider_key(
            PROJECT_KEY,
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function_item",
                provider_key: &key,
                display_name: "run",
                path: "src/lib.rs",
                start_line: 1,
                end_line: 1,
                content: b"fn run() {}",
            },
        );
        let right = SymbolNode::from_provider_key(
            PROJECT_KEY,
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function_item",
                provider_key: &key,
                display_name: "run",
                path: "src/lib.rs",
                start_line: 4,
                end_line: 4,
                content: b"fn run() {}",
            },
        );
        assert_eq!(left.id, right.id);
        assert_eq!(left.status, SymbolStatus::Active);
    }

    #[test]
    fn syntax_fallback_does_not_claim_rename_identity() {
        let before_key = encode_provider_key(&["src/lib.rs", "function_item", "before"]);
        let after_key = encode_provider_key(&["src/lib.rs", "function_item", "after"]);
        let old = SymbolNode::from_provider_key(
            PROJECT_KEY,
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function_item",
                provider_key: &before_key,
                display_name: "before",
                path: "src/lib.rs",
                start_line: 1,
                end_line: 1,
                content: b"fn body() {}",
            },
        );
        let renamed = SymbolNode::from_provider_key(
            PROJECT_KEY,
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function_item",
                provider_key: &after_key,
                display_name: "after",
                path: "src/lib.rs",
                start_line: 1,
                end_line: 1,
                content: b"fn body() {}",
            },
        );
        assert_ne!(old.id, renamed.id);
        assert_eq!(old.identity_quality, IdentityQuality::SyntaxFallback);
    }

    #[test]
    fn worktree_revision_changes_when_symbol_location_changes() {
        let provider = provider();
        let make = |line| {
            SymbolNode::from_provider_key(
                PROJECT_KEY,
                &provider,
                SymbolNodeInput {
                    language: SourceLanguage::rust(),
                    kind: "function_item",
                    provider_key: "stable-key",
                    display_name: "run",
                    path: "src/lib.rs",
                    start_line: line,
                    end_line: line,
                    content: b"fn run() {}",
                },
            )
        };
        let first = super::SymbolSnapshot::for_worktree(
            PROJECT_KEY,
            provider.clone(),
            "head",
            vec![SourceFileState::from_source(
                "src/lib.rs",
                SourceLanguage::rust(),
                b"fn run() {}",
                false,
            )],
            vec![make(1)],
            Vec::new(),
        );
        let shifted = super::SymbolSnapshot::for_worktree(
            PROJECT_KEY,
            provider.clone(),
            "head",
            vec![SourceFileState::from_source(
                "src/lib.rs",
                SourceLanguage::rust(),
                b"\nfn run() {}",
                false,
            )],
            vec![make(2)],
            Vec::new(),
        );
        assert_ne!(first.source_revision, shifted.source_revision);
    }

    #[test]
    fn worktree_revision_covers_symbol_free_source_and_syntax_state() {
        let make = |source: &[u8], has_syntax_errors| {
            super::SymbolSnapshot::for_worktree(
                PROJECT_KEY,
                provider(),
                "unborn:refs/heads/main",
                vec![SourceFileState::from_source(
                    "src/empty.rs",
                    SourceLanguage::rust(),
                    source,
                    has_syntax_errors,
                )],
                Vec::new(),
                Vec::new(),
            )
        };
        let empty = make(b"// comment\n", false);
        let invalid = make(b"fn {\n", true);
        let status_changed = make(b"// comment\n", true);

        assert_ne!(empty.source_revision, invalid.source_revision);
        assert_ne!(empty.source_revision, status_changed.source_revision);
    }

    #[test]
    fn worktree_revision_covers_provider_and_complete_symbol_observation() {
        let source = SourceFileState::from_source(
            "src/lib.rs",
            SourceLanguage::rust(),
            b"fn run() {}",
            false,
        );
        let make_symbol = |kind: &'static str, display_name: &'static str| {
            SymbolNode::from_provider_key(
                PROJECT_KEY,
                &provider(),
                SymbolNodeInput {
                    language: SourceLanguage::rust(),
                    kind,
                    provider_key: "stable-key",
                    display_name,
                    path: "src/lib.rs",
                    start_line: 1,
                    end_line: 1,
                    content: b"fn run() {}",
                },
            )
        };
        let snapshot = |provider, symbol| {
            super::SymbolSnapshot::for_worktree(
                PROJECT_KEY,
                provider,
                "head",
                vec![source.clone()],
                vec![symbol],
                Vec::new(),
            )
        };
        let baseline = snapshot(provider(), make_symbol("function_item", "run"));

        let mut semantic_provider = provider();
        semantic_provider.identity_quality = IdentityQuality::Semantic;
        let mut semantic_symbol = make_symbol("function_item", "run");
        semantic_symbol.identity_quality = IdentityQuality::Semantic;
        let quality_changed = snapshot(semantic_provider, semantic_symbol);
        let kind_changed = snapshot(provider(), make_symbol("method_item", "run"));
        let name_changed = snapshot(provider(), make_symbol("function_item", "renamed"));

        assert_ne!(baseline.source_revision, quality_changed.source_revision);
        assert_ne!(baseline.source_revision, kind_changed.source_revision);
        assert_ne!(baseline.source_revision, name_changed.source_revision);
    }

    #[test]
    fn identical_provider_symbols_are_isolated_by_project() {
        let make = |project_key| {
            SymbolNode::from_provider_key(
                project_key,
                &provider(),
                SymbolNodeInput {
                    language: SourceLanguage::rust(),
                    kind: "function_item",
                    provider_key: "stable-key",
                    display_name: "run",
                    path: "src/lib.rs",
                    start_line: 1,
                    end_line: 1,
                    content: b"fn run() {}",
                },
            )
        };
        let alpha = make("project_alpha");
        let beta = make("project_beta");

        assert_ne!(alpha.id, beta.id);
        assert_eq!(alpha.project_key, "project_alpha");
        assert_eq!(beta.project_key, "project_beta");
    }

    fn lineage_observation(
        snapshot: &str,
        symbol_id: &str,
        name: &str,
        path: &str,
    ) -> LineageSymbolObservation {
        LineageSymbolObservation {
            project_key: PROJECT_KEY.to_owned(),
            provider_profile_id: "rust-main".to_owned(),
            provider_contract_id: "scip-rust-main-rust-analyzer-contract-1".to_owned(),
            language: SourceLanguage::rust(),
            snapshot_revision: snapshot.to_owned(),
            symbol_id: symbol_id.to_owned(),
            provider_symbol: Some(format!("rust-analyzer cargo demo 0.1.0 {name}().")),
            is_local: false,
            kind: "function".to_owned(),
            display_name: name.to_owned(),
            path: path.to_owned(),
            normalized_definition_fingerprint: "sha256_same-body".to_owned(),
        }
    }

    #[test]
    fn rename_and_file_move_only_create_candidates() {
        let renamed = propose_lineage_candidates(
            &[lineage_observation("old", "old-id", "before", "src/lib.rs")],
            &[lineage_observation("new", "new-id", "after", "src/lib.rs")],
            &[],
        );
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].ambiguity_group_id, None);
        assert_eq!(renamed[0].confidence, LineageConfidence::High);
        assert_ne!(renamed[0].from_symbol, renamed[0].to_symbol);

        let moved = propose_lineage_candidates(
            &[lineage_observation("old", "old-id", "run", "src/a.rs")],
            &[lineage_observation("new", "new-id", "run", "src/b.rs")],
            &[PathRenameEvidence {
                old_path: "src/a.rs".to_owned(),
                new_path: "src/b.rs".to_owned(),
                similarity_basis_points: 10_000,
            }],
        );
        assert_eq!(moved[0].ambiguity_group_id, None);
        assert_eq!(moved[0].confidence, LineageConfidence::High);
    }

    #[test]
    fn ambiguous_matches_share_a_group_but_remain_proposals() {
        let old = lineage_observation("old", "old-id", "run", "src/lib.rs");
        let first = lineage_observation("new", "new-a", "run_a", "src/a.rs");
        let second = lineage_observation("new", "new-b", "run_b", "src/b.rs");
        let candidates = propose_lineage_candidates(&[old], &[first, second], &[]);

        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| {
            candidate.ambiguity_group_id.is_some() && candidate.confidence == LineageConfidence::Low
        }));
    }

    #[test]
    fn unchanged_symbol_ids_do_not_create_lineage_candidates() {
        let old = lineage_observation("old", "stable-id", "run", "src/lib.rs");
        let current = lineage_observation("new", "stable-id", "run", "src/lib.rs");
        assert!(propose_lineage_candidates(&[old], &[current], &[]).is_empty());
    }

    #[test]
    fn lineage_never_matches_symbols_across_projects() {
        let old = lineage_observation("old", "old-id", "run", "src/lib.rs");
        let mut other = lineage_observation("new", "new-id", "run", "src/lib.rs");
        other.project_key = "project_beta".to_owned();

        assert!(propose_lineage_candidates(&[old], &[other], &[]).is_empty());
    }

    #[test]
    fn lineage_never_matches_across_providers_or_languages() {
        let old = lineage_observation("old", "old-id", "run", "src/lib.rs");
        let mut other_provider = lineage_observation("new", "new-id", "run", "src/lib.rs");
        other_provider.provider_contract_id = "scip-secondary".to_owned();
        assert!(
            propose_lineage_candidates(std::slice::from_ref(&old), &[other_provider], &[])
                .is_empty()
        );

        let mut other_language = lineage_observation("new", "new-id", "run", "src/lib.rs");
        other_language.language = SourceLanguage::python();
        assert!(propose_lineage_candidates(&[old], &[other_language], &[]).is_empty());
    }

    #[test]
    fn rename_similarity_is_bounded_and_affects_confidence() {
        let old = lineage_observation("old", "old-id", "run", "src/a.rs");
        let mut current = lineage_observation("new", "new-id", "run", "src/b.rs");
        current.provider_symbol = Some("rust-analyzer cargo demo 0.1.0 moved().".to_owned());
        let low = propose_lineage_candidates(
            std::slice::from_ref(&old),
            std::slice::from_ref(&current),
            &[PathRenameEvidence {
                old_path: "src/a.rs".to_owned(),
                new_path: "src/b.rs".to_owned(),
                similarity_basis_points: 0,
            }],
        );
        assert_eq!(low[0].confidence, LineageConfidence::Medium);

        let invalid = propose_lineage_candidates(
            &[old],
            &[current],
            &[PathRenameEvidence {
                old_path: "src/a.rs".to_owned(),
                new_path: "src/b.rs".to_owned(),
                similarity_basis_points: 10_001,
            }],
        );
        assert_eq!(invalid[0].confidence, LineageConfidence::Medium);
        assert!(
            invalid[0]
                .evidence
                .iter()
                .all(|evidence| !matches!(evidence, LineageEvidence::GitPathRename { .. }))
        );
    }

    #[test]
    fn local_symbols_never_receive_cross_snapshot_lineage() {
        let mut old = lineage_observation("old", "old-id", "local", "src/lib.rs");
        old.is_local = true;
        let mut current = lineage_observation("new", "new-id", "local", "src/lib.rs");
        current.is_local = true;

        assert!(propose_lineage_candidates(&[old], &[current], &[]).is_empty());
    }
}
