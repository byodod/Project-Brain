use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SYMBOL_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    Rust,
}

impl SourceLanguage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        (value == "rust").then_some(Self::Rust)
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
    Calls,
    Imports,
    Implements,
    Reads,
    Writes,
}

impl EdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Implements => "implements",
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

#[derive(Debug, Clone, Copy)]
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
    pub fn from_provider_key(provider: &ProviderDescriptor, input: SymbolNodeInput<'_>) -> Self {
        let id = symbol_id(&provider.id, input.provider_key);
        Self {
            id,
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
    pub provider: ProviderDescriptor,
    pub source_revision: String,
    pub sources: Vec<SourceFileState>,
    pub symbols: Vec<SymbolNode>,
    pub edges: Vec<SymbolEdge>,
}

impl SymbolSnapshot {
    pub fn for_worktree(
        provider: ProviderDescriptor,
        head_revision: &str,
        mut sources: Vec<SourceFileState>,
        mut symbols: Vec<SymbolNode>,
        mut edges: Vec<SymbolEdge>,
    ) -> Self {
        sources
            .sort_by(|left, right| (&left.path, left.language).cmp(&(&right.path, right.language)));
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
            append_digest_part(&mut revision_material, edge.provider_id.as_bytes());
            append_digest_part(&mut revision_material, edge.source_id.as_bytes());
            append_digest_part(&mut revision_material, edge.target_id.as_bytes());
            append_digest_part(&mut revision_material, edge.kind.as_str().as_bytes());
        }
        let source_revision = format!("worktree_v2_{}", stable_digest(&[&revision_material]));
        Self {
            protocol_version: SYMBOL_PROTOCOL_VERSION,
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

pub fn symbol_id(provider_id: &str, provider_key: &str) -> String {
    format!(
        "sym_v1_{}",
        stable_digest(&[provider_id.as_bytes(), provider_key.as_bytes()])
    )
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GraphDelta {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub removed: u64,
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
        IdentityQuality, ProviderDescriptor, SourceFileState, SourceLanguage, SymbolNode,
        SymbolNodeInput, SymbolStatus, encode_provider_key,
    };

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
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::Rust,
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
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::Rust,
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
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::Rust,
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
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::Rust,
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
                &provider,
                SymbolNodeInput {
                    language: SourceLanguage::Rust,
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
            provider.clone(),
            "head",
            vec![SourceFileState::from_source(
                "src/lib.rs",
                SourceLanguage::Rust,
                b"fn run() {}",
                false,
            )],
            vec![make(1)],
            Vec::new(),
        );
        let shifted = super::SymbolSnapshot::for_worktree(
            provider.clone(),
            "head",
            vec![SourceFileState::from_source(
                "src/lib.rs",
                SourceLanguage::Rust,
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
                provider(),
                "unborn:refs/heads/main",
                vec![SourceFileState::from_source(
                    "src/empty.rs",
                    SourceLanguage::Rust,
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
        let source =
            SourceFileState::from_source("src/lib.rs", SourceLanguage::Rust, b"fn run() {}", false);
        let make_symbol = |kind: &'static str, display_name: &'static str| {
            SymbolNode::from_provider_key(
                &provider(),
                SymbolNodeInput {
                    language: SourceLanguage::Rust,
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
}
