use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use brain_symbols::{
    EdgeKind, IdentityQuality, LineageSymbolObservation, ProviderDescriptor, SourceFileState,
    SourceLanguage, SymbolEdge, SymbolNode, SymbolNodeInput, SymbolSnapshot, encode_provider_key,
};
use protobuf::Message;
use scip::types::{
    Document, Index, Occurrence, PositionEncoding, Relationship, SymbolInformation, SymbolRole,
    occurrence,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_SCIP_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ScipError {
    #[error("读取 SCIP 或源码失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("SCIP protobuf 解码失败：{0}")]
    Decode(#[from] protobuf::Error),

    #[error("SCIP 文件过大：{actual} bytes，最大允许 {maximum}")]
    IndexTooLarge { actual: u64, maximum: u64 },

    #[error("SCIP metadata 缺失或无效：{0}")]
    InvalidMetadata(String),

    #[error("SCIP document 无效：{0}")]
    InvalidDocument(String),

    #[error("SCIP provider profile 不匹配：{0}")]
    ProfileMismatch(String),

    #[error("SCIP document 越出项目根目录：{0}")]
    DocumentOutsideProject(PathBuf),

    #[error("当前 SCIP 离线导入器不支持 position encoding：{0:?}")]
    UnsupportedPositionEncoding(PositionEncoding),

    #[error("SCIP occurrence range 无效：{0}")]
    InvalidRange(String),

    #[error("SCIP document 对应源码不是 UTF-8：{0}")]
    NonUtf8Source(String),
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Partial,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScipProviderCapabilities {
    pub definitions: CapabilitySupport,
    pub references: CapabilitySupport,
    pub symbol_kinds: CapabilitySupport,
    pub relationships: CapabilitySupport,
    pub calls: CapabilitySupport,
    pub implementations: CapabilitySupport,
    pub imports: CapabilitySupport,
    pub generated_code: CapabilitySupport,
    pub trait_symbols: CapabilitySupport,
    pub macro_symbols: CapabilitySupport,
    pub local_symbols: CapabilitySupport,
}

impl ScipProviderCapabilities {
    fn for_contract(producer_name: &str, language: &SourceLanguage) -> Self {
        let producer = producer_name.trim().to_ascii_lowercase();
        let language = language.as_str();
        if producer == "rust-analyzer" && language == "rust" {
            return Self {
                definitions: CapabilitySupport::Supported,
                references: CapabilitySupport::Supported,
                symbol_kinds: CapabilitySupport::Supported,
                relationships: CapabilitySupport::Unsupported,
                calls: CapabilitySupport::Unsupported,
                implementations: CapabilitySupport::Unsupported,
                imports: CapabilitySupport::Unsupported,
                generated_code: CapabilitySupport::Unknown,
                trait_symbols: CapabilitySupport::Supported,
                macro_symbols: CapabilitySupport::Supported,
                local_symbols: CapabilitySupport::Supported,
            };
        }
        if producer == "scip-dotnet" && matches!(language, "csharp" | "visual-basic") {
            return Self {
                definitions: CapabilitySupport::Supported,
                references: CapabilitySupport::Supported,
                symbol_kinds: CapabilitySupport::Unknown,
                relationships: CapabilitySupport::Supported,
                calls: CapabilitySupport::Unsupported,
                implementations: CapabilitySupport::Supported,
                imports: CapabilitySupport::Unsupported,
                generated_code: CapabilitySupport::Unknown,
                trait_symbols: CapabilitySupport::Unknown,
                macro_symbols: CapabilitySupport::Unsupported,
                local_symbols: CapabilitySupport::Unknown,
            };
        }
        if producer == "scip-python" && language == "python" {
            return Self {
                definitions: CapabilitySupport::Supported,
                references: CapabilitySupport::Supported,
                symbol_kinds: CapabilitySupport::Unknown,
                relationships: CapabilitySupport::Partial,
                calls: CapabilitySupport::Unsupported,
                implementations: CapabilitySupport::Partial,
                imports: CapabilitySupport::Partial,
                generated_code: CapabilitySupport::Unknown,
                trait_symbols: CapabilitySupport::Unsupported,
                macro_symbols: CapabilitySupport::Unsupported,
                local_symbols: CapabilitySupport::Supported,
            };
        }
        Self {
            definitions: CapabilitySupport::Supported,
            references: CapabilitySupport::Supported,
            symbol_kinds: CapabilitySupport::Unknown,
            relationships: CapabilitySupport::Unknown,
            calls: CapabilitySupport::Unsupported,
            implementations: CapabilitySupport::Unknown,
            imports: CapabilitySupport::Unknown,
            generated_code: CapabilitySupport::Unknown,
            trait_symbols: CapabilitySupport::Unknown,
            macro_symbols: CapabilitySupport::Unknown,
            local_symbols: CapabilitySupport::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScipLanguageCapabilities {
    pub language: SourceLanguage,
    pub capabilities: ScipProviderCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipLanguageMapping {
    pub raw_language: Option<String>,
    pub language: SourceLanguage,
    pub allow_missing_language: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScipImportProfile {
    pub id: String,
    pub producer: String,
    pub contract_version: u16,
    pub language_mappings: Vec<ScipLanguageMapping>,
}

#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq, Eq)]
pub struct ScipImportStats {
    pub documents: u64,
    pub definitions: u64,
    pub reference_edges: u64,
    pub contains_edges: u64,
    pub relationship_edges: u64,
    pub unresolved_references: u64,
    pub ambiguous_provider_symbols: u64,
    pub skipped_definitions_without_metadata: u64,
}

#[derive(Debug, Clone)]
pub struct ScipImport {
    pub snapshot: SymbolSnapshot,
    pub capabilities: Vec<ScipLanguageCapabilities>,
    pub lineage_observations: Vec<LineageSymbolObservation>,
    pub stats: ScipImportStats,
    pub producer_name: String,
    pub producer_version: String,
    pub languages: Vec<SourceLanguage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceRange {
    start_line: usize,
    start_character: usize,
    end_line: usize,
    end_character: usize,
}

impl SourceRange {
    fn contains(self, other: Self) -> bool {
        (self.start_line, self.start_character) <= (other.start_line, other.start_character)
            && (other.end_line, other.end_character) <= (self.end_line, self.end_character)
    }

    fn span_key(self) -> (usize, usize) {
        (
            self.end_line.saturating_sub(self.start_line),
            self.end_character.saturating_sub(self.start_character),
        )
    }

    fn brain_end_line(self) -> usize {
        let inclusive = if self.end_line > self.start_line && self.end_character == 0 {
            self.end_line - 1
        } else {
            self.end_line
        };
        inclusive + 1
    }
}

#[derive(Debug, Clone)]
struct DefinitionRecord {
    document_path: String,
    raw_symbol: String,
    range: SourceRange,
    node: SymbolNode,
    relationships: Vec<Relationship>,
    enclosing_symbol: String,
    normalized_definition_fingerprint: String,
    is_local: bool,
}

#[derive(Debug, Clone)]
struct LoadedSource {
    language: SourceLanguage,
    text: String,
}

struct ImportContext {
    project_key: String,
    head_revision: String,
    index_digest: String,
    provider: ProviderDescriptor,
    provider_profile_id: String,
    producer_name: String,
    producer_version: String,
}

/// 按项目 provider profile 从 `.scip` 文件离线导入语义快照。
///
/// # Errors
///
/// 文件过大、protobuf 无效、profile 不匹配，或任一 document 越出项目根目录时失败。
pub fn import_file(
    project_root: &Path,
    project_key: &str,
    head_revision: &str,
    input: &Path,
    profile: &ScipImportProfile,
) -> Result<ScipImport, ScipError> {
    let size = fs::metadata(input)?.len();
    if size > MAX_SCIP_BYTES {
        return Err(ScipError::IndexTooLarge {
            actual: size,
            maximum: MAX_SCIP_BYTES,
        });
    }
    let bytes = fs::read(input)?;
    import_bytes(project_root, project_key, head_revision, &bytes, profile)
}

/// 按显式语言映射从内存中的标准 SCIP protobuf 构造项目化 Provider-neutral 快照。
///
/// # Errors
///
/// protobuf、metadata、document path、源码或 occurrence range 无效时失败。
pub fn import_bytes(
    project_root: &Path,
    project_key: &str,
    head_revision: &str,
    bytes: &[u8],
    profile: &ScipImportProfile,
) -> Result<ScipImport, ScipError> {
    let (index, context) = decode_index(project_key, head_revision, bytes, profile)?;
    let root = project_root.canonicalize()?;
    let sources = load_sources(&root, &index.documents, profile)?;
    let mut stats = ScipImportStats {
        documents: u64::try_from(index.documents.len()).unwrap_or(u64::MAX),
        ..ScipImportStats::default()
    };
    let definitions = collect_definitions(
        &context.project_key,
        &context.provider,
        &context.index_digest,
        &index.documents,
        &sources,
        &mut stats,
    )?;
    let symbol_targets = definition_targets(&definitions, &mut stats);
    let mut edges = collect_reference_edges(
        &context.project_key,
        &context.provider,
        &index.documents,
        &definitions,
        &symbol_targets,
        &mut stats,
    )?;
    collect_relationship_edges(
        &context.project_key,
        &context.provider,
        &definitions,
        &symbol_targets,
        &mut edges,
        &mut stats,
    );
    edges.sort_by(|left, right| {
        (&left.source_id, &left.target_id, left.kind).cmp(&(
            &right.source_id,
            &right.target_id,
            right.kind,
        ))
    });
    edges.dedup_by(|left, right| {
        left.source_id == right.source_id
            && left.target_id == right.target_id
            && left.kind == right.kind
    });
    stats.reference_edges = u64::try_from(
        edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::References)
            .count(),
    )
    .unwrap_or(u64::MAX);
    Ok(finish_import(context, &sources, definitions, edges, stats))
}

fn finish_import(
    context: ImportContext,
    sources: &BTreeMap<String, LoadedSource>,
    mut definitions: Vec<DefinitionRecord>,
    edges: Vec<SymbolEdge>,
    mut stats: ScipImportStats,
) -> ScipImport {
    let source_states = sources
        .iter()
        .map(|(path, source)| {
            SourceFileState::from_source(
                path,
                source.language.clone(),
                source.text.as_bytes(),
                false,
            )
        })
        .collect();
    let nodes = definitions
        .iter()
        .map(|definition| definition.node.clone())
        .collect();
    let revision_hint = format!("{}:scip:{}", context.head_revision, context.index_digest);
    let snapshot = SymbolSnapshot::for_worktree(
        &context.project_key,
        context.provider,
        &revision_hint,
        source_states,
        nodes,
        edges,
    );
    let lineage_observations = definitions
        .drain(..)
        .map(|definition| LineageSymbolObservation {
            project_key: context.project_key.clone(),
            provider_profile_id: context.provider_profile_id.clone(),
            provider_contract_id: snapshot.provider.id.clone(),
            language: definition.node.language.clone(),
            snapshot_revision: snapshot.source_revision.clone(),
            symbol_id: definition.node.id,
            provider_symbol: Some(definition.raw_symbol),
            is_local: definition.is_local,
            kind: definition.node.kind,
            display_name: definition.node.display_name,
            path: definition.document_path,
            normalized_definition_fingerprint: definition.normalized_definition_fingerprint,
        })
        .collect();
    stats.definitions = u64::try_from(snapshot.symbols.len()).unwrap_or(u64::MAX);
    let languages: Vec<SourceLanguage> = snapshot
        .sources
        .iter()
        .map(|source| source.language.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ScipImport {
        snapshot,
        capabilities: languages
            .iter()
            .cloned()
            .map(|language| ScipLanguageCapabilities {
                capabilities: ScipProviderCapabilities::for_contract(
                    &context.producer_name,
                    &language,
                ),
                language,
            })
            .collect(),
        lineage_observations,
        stats,
        producer_name: context.producer_name,
        producer_version: context.producer_version,
        languages,
    }
}

fn decode_index(
    project_key: &str,
    head_revision: &str,
    bytes: &[u8],
    profile: &ScipImportProfile,
) -> Result<(Index, ImportContext), ScipError> {
    let mut index = Index::parse_from_bytes(bytes)?;
    for document in &mut index.documents {
        document.relative_path = normalize_document_path(&document.relative_path)?;
    }
    let metadata = index
        .metadata
        .as_ref()
        .ok_or_else(|| ScipError::InvalidMetadata("metadata 缺失".to_owned()))?;
    let tool = metadata
        .tool_info
        .as_ref()
        .ok_or_else(|| ScipError::InvalidMetadata("tool_info 缺失".to_owned()))?;
    if tool.name.trim().is_empty() || tool.version.trim().is_empty() {
        return Err(ScipError::InvalidMetadata(
            "tool_info.name/version 不能为空".to_owned(),
        ));
    }
    if !tool
        .name
        .trim()
        .eq_ignore_ascii_case(profile.producer.trim())
    {
        return Err(ScipError::ProfileMismatch(format!(
            "profile={} 要求 producer={:?}，index 实际为 {:?}",
            profile.id, profile.producer, tool.name
        )));
    }
    validate_import_profile(profile)?;
    let context = ImportContext {
        project_key: project_key.to_owned(),
        head_revision: head_revision.to_owned(),
        index_digest: sha256(bytes),
        provider: ProviderDescriptor {
            id: provider_contract_id(profile),
            version: format!("contract-{}", profile.contract_version),
            identity_quality: IdentityQuality::Semantic,
        },
        provider_profile_id: profile.id.clone(),
        producer_name: tool.name.clone(),
        producer_version: tool.version.clone(),
    };
    Ok((index, context))
}

fn load_sources(
    root: &Path,
    documents: &[Document],
    profile: &ScipImportProfile,
) -> Result<BTreeMap<String, LoadedSource>, ScipError> {
    let mut sources = BTreeMap::new();
    for document in documents {
        let language = validate_document(document, profile)?;
        if sources.contains_key(&document.relative_path) {
            return Err(ScipError::InvalidDocument(format!(
                "重复 relative_path={:?}",
                document.relative_path
            )));
        }
        let relative = Path::new(&document.relative_path);
        let canonical = root.join(relative).canonicalize()?;
        if !canonical.starts_with(root) {
            return Err(ScipError::DocumentOutsideProject(canonical));
        }
        if !canonical.is_file() {
            return Err(ScipError::InvalidDocument(format!(
                "不是普通文件：{}",
                document.relative_path
            )));
        }
        let bytes = fs::read(canonical)?;
        let source = String::from_utf8(bytes)
            .map_err(|_| ScipError::NonUtf8Source(document.relative_path.clone()))?;
        sources.insert(
            document.relative_path.clone(),
            LoadedSource {
                language,
                text: source,
            },
        );
    }
    Ok(sources)
}

fn validate_document(
    document: &Document,
    profile: &ScipImportProfile,
) -> Result<SourceLanguage, ScipError> {
    let raw_language = document.language.trim();
    let mapping = profile.language_mappings.iter().find(|mapping| {
        if raw_language.is_empty() {
            mapping.raw_language.is_none() && mapping.allow_missing_language
        } else {
            mapping.raw_language.as_deref() == Some(raw_language)
        }
    });
    let language = mapping
        .map(|mapping| mapping.language.clone())
        .ok_or_else(|| {
            ScipError::ProfileMismatch(format!(
                "profile={} 未显式映射 Document.language={:?}",
                profile.id, document.language
            ))
        })?;
    let path = Path::new(&document.relative_path);
    if document.relative_path.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(ScipError::InvalidDocument(format!(
            "relative_path 非规范：{:?}",
            document.relative_path
        )));
    }
    let encoding = document.position_encoding.enum_value().map_err(|unknown| {
        ScipError::InvalidDocument(format!("未知 position encoding={unknown}"))
    })?;
    if !matches!(
        encoding,
        PositionEncoding::UnspecifiedPositionEncoding
            | PositionEncoding::UTF8CodeUnitOffsetFromLineStart
    ) {
        return Err(ScipError::UnsupportedPositionEncoding(encoding));
    }
    Ok(language)
}

fn normalize_document_path(path: &str) -> Result<String, ScipError> {
    let normalized = path.trim().replace('\\', "/");
    let candidate = Path::new(&normalized);
    if normalized.is_empty()
        || normalized.starts_with('/')
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(ScipError::InvalidDocument(format!(
            "relative_path 非规范：{path:?}"
        )));
    }
    Ok(normalized)
}

fn collect_definitions(
    project_key: &str,
    provider: &ProviderDescriptor,
    index_digest: &str,
    documents: &[Document],
    sources: &BTreeMap<String, LoadedSource>,
    stats: &mut ScipImportStats,
) -> Result<Vec<DefinitionRecord>, ScipError> {
    let mut definitions = Vec::new();
    for document in documents {
        let loaded = &sources[&document.relative_path];
        let source = &loaded.text;
        let information = document.symbols.iter().fold(
            BTreeMap::<&str, Vec<&SymbolInformation>>::new(),
            |mut map, info| {
                map.entry(&info.symbol).or_default().push(info);
                map
            },
        );
        for occurrence in document
            .occurrences
            .iter()
            .filter(|item| has_role(item, SymbolRole::Definition))
        {
            let range = occurrence_range(occurrence)?;
            let Some(info) = information
                .get(occurrence.symbol.as_str())
                .and_then(|items| items.first())
            else {
                stats.skipped_definitions_without_metadata += 1;
                continue;
            };
            let content = extract_range(source, range)?;
            let display_name = if info.display_name.trim().is_empty() {
                occurrence.symbol.clone()
            } else {
                info.display_name.clone()
            };
            let is_local = scip::symbol::is_local_symbol(&occurrence.symbol);
            let range_key = format!(
                "{}:{}:{}:{}",
                range.start_line, range.start_character, range.end_line, range.end_character
            );
            let provider_key = if is_local {
                encode_provider_key(&[
                    "local",
                    index_digest,
                    loaded.language.as_str(),
                    &document.relative_path,
                    &occurrence.symbol,
                    &range_key,
                ])
            } else {
                encode_provider_key(&[
                    "global",
                    loaded.language.as_str(),
                    &occurrence.symbol,
                    &document.relative_path,
                    &range_key,
                ])
            };
            let kind = symbol_kind(info);
            let node = SymbolNode::from_provider_key(
                project_key,
                provider,
                SymbolNodeInput {
                    language: loaded.language.clone(),
                    kind: &kind,
                    provider_key: &provider_key,
                    display_name: &display_name,
                    path: &document.relative_path,
                    start_line: range.start_line + 1,
                    end_line: range.brain_end_line(),
                    content: content.as_bytes(),
                },
            );
            let normalized_definition_fingerprint =
                semantic_definition_fingerprint(info, &display_name).unwrap_or_else(|| {
                    // SCIP definition occurrence 常常只覆盖名称 token。缺少 producer 签名时，
                    // 不能把该 token 冒充定义正文；使用节点身份做不可跨快照匹配的占位指纹。
                    format!(
                        "sha256_{}",
                        sha256(format!("no-lineage:{}", node.id).as_bytes())
                    )
                });
            definitions.push(DefinitionRecord {
                document_path: document.relative_path.clone(),
                raw_symbol: occurrence.symbol.clone(),
                range,
                node,
                relationships: info.relationships.clone(),
                enclosing_symbol: info.enclosing_symbol.clone(),
                normalized_definition_fingerprint,
                is_local,
            });
        }
    }
    definitions.sort_by(|left, right| left.node.id.cmp(&right.node.id));
    Ok(definitions)
}

fn definition_targets(
    definitions: &[DefinitionRecord],
    stats: &mut ScipImportStats,
) -> BTreeMap<String, Vec<String>> {
    let mut targets = BTreeMap::<String, Vec<String>>::new();
    for definition in definitions {
        targets
            .entry(definition.raw_symbol.clone())
            .or_default()
            .push(definition.node.id.clone());
    }
    stats.ambiguous_provider_symbols = u64::try_from(
        targets
            .values()
            .filter(|definition_ids| definition_ids.len() > 1)
            .count(),
    )
    .unwrap_or(u64::MAX);
    targets
}

fn collect_reference_edges(
    project_key: &str,
    provider: &ProviderDescriptor,
    documents: &[Document],
    definitions: &[DefinitionRecord],
    targets: &BTreeMap<String, Vec<String>>,
    stats: &mut ScipImportStats,
) -> Result<Vec<SymbolEdge>, ScipError> {
    let mut edges = Vec::new();
    for document in documents {
        let owners = definitions
            .iter()
            .filter(|definition| definition.document_path == document.relative_path)
            .collect::<Vec<_>>();
        for occurrence in document
            .occurrences
            .iter()
            .filter(|item| !has_role(item, SymbolRole::Definition) && !item.symbol.is_empty())
        {
            let range = occurrence_range(occurrence)?;
            let source = owners
                .iter()
                .filter(|definition| definition.range.contains(range))
                .min_by_key(|definition| definition.range.span_key());
            let target = targets
                .get(&occurrence.symbol)
                .filter(|definition_ids| definition_ids.len() == 1)
                .and_then(|definition_ids| definition_ids.first());
            match (source, target) {
                (Some(source), Some(target)) if source.node.id != *target => {
                    edges.push(SymbolEdge {
                        project_key: project_key.to_owned(),
                        provider_id: provider.id.clone(),
                        source_id: source.node.id.clone(),
                        target_id: target.clone(),
                        kind: EdgeKind::References,
                    });
                }
                _ => stats.unresolved_references += 1,
            }
        }
    }
    Ok(edges)
}

fn collect_relationship_edges(
    project_key: &str,
    provider: &ProviderDescriptor,
    definitions: &[DefinitionRecord],
    targets: &BTreeMap<String, Vec<String>>,
    edges: &mut Vec<SymbolEdge>,
    stats: &mut ScipImportStats,
) {
    for definition in definitions {
        if !definition.enclosing_symbol.is_empty() {
            if let Some(parent_id) = unique_target(targets, &definition.enclosing_symbol) {
                if parent_id != &definition.node.id {
                    edges.push(SymbolEdge {
                        project_key: project_key.to_owned(),
                        provider_id: provider.id.clone(),
                        source_id: parent_id.clone(),
                        target_id: definition.node.id.clone(),
                        kind: EdgeKind::Contains,
                    });
                    stats.contains_edges += 1;
                }
            } else {
                stats.unresolved_references += 1;
            }
        }
        for relationship in &definition.relationships {
            for (enabled, kind) in [
                (relationship.is_reference, EdgeKind::References),
                (relationship.is_implementation, EdgeKind::Implements),
                (relationship.is_type_definition, EdgeKind::TypeDefinition),
            ] {
                if enabled
                    && push_unique_target_edge(
                        project_key,
                        provider,
                        targets,
                        &relationship.symbol,
                        &definition.node.id,
                        kind,
                        edges,
                        stats,
                    )
                {
                    stats.relationship_edges += 1;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_unique_target_edge(
    project_key: &str,
    provider: &ProviderDescriptor,
    targets: &BTreeMap<String, Vec<String>>,
    raw_target: &str,
    source_id: &str,
    kind: EdgeKind,
    edges: &mut Vec<SymbolEdge>,
    stats: &mut ScipImportStats,
) -> bool {
    let Some(target) = unique_target(targets, raw_target) else {
        stats.unresolved_references += 1;
        return false;
    };
    if source_id != target {
        edges.push(SymbolEdge {
            project_key: project_key.to_owned(),
            provider_id: provider.id.clone(),
            source_id: source_id.to_owned(),
            target_id: target.clone(),
            kind,
        });
        return true;
    }
    false
}

fn unique_target<'a>(
    targets: &'a BTreeMap<String, Vec<String>>,
    raw_target: &str,
) -> Option<&'a String> {
    targets
        .get(raw_target)
        .filter(|definition_ids| definition_ids.len() == 1)
        .and_then(|definition_ids| definition_ids.first())
}

fn has_role(occurrence: &Occurrence, role: SymbolRole) -> bool {
    occurrence.symbol_roles & role as i32 != 0
}

fn occurrence_range(occurrence: &Occurrence) -> Result<SourceRange, ScipError> {
    let raw = match occurrence.typed_range.as_ref() {
        Some(occurrence::Typed_range::SingleLineRange(range)) => [
            range.line,
            range.start_character,
            range.line,
            range.end_character,
        ],
        Some(occurrence::Typed_range::MultiLineRange(range)) => [
            range.start_line,
            range.start_character,
            range.end_line,
            range.end_character,
        ],
        Some(_) => {
            return Err(ScipError::InvalidRange(format!(
                "symbol={:?} 使用未知 typed range",
                occurrence.symbol
            )));
        }
        None => match occurrence.range.as_slice() {
            [line, start_character, end_character] => {
                [*line, *start_character, *line, *end_character]
            }
            [start_line, start_character, end_line, end_character] => {
                [*start_line, *start_character, *end_line, *end_character]
            }
            _ => {
                return Err(ScipError::InvalidRange(format!(
                    "symbol={:?} 缺少 3/4 元素 range",
                    occurrence.symbol
                )));
            }
        },
    };
    if raw.iter().any(|value| *value < 0) || (raw[0], raw[1]) > (raw[2], raw[3]) {
        return Err(ScipError::InvalidRange(format!(
            "symbol={:?} range={raw:?}",
            occurrence.symbol
        )));
    }
    Ok(SourceRange {
        start_line: usize::try_from(raw[0]).unwrap_or(usize::MAX),
        start_character: usize::try_from(raw[1]).unwrap_or(usize::MAX),
        end_line: usize::try_from(raw[2]).unwrap_or(usize::MAX),
        end_character: usize::try_from(raw[3]).unwrap_or(usize::MAX),
    })
}

fn extract_range(source: &str, range: SourceRange) -> Result<String, ScipError> {
    let starts = line_starts(source);
    let start = position_offset(source, &starts, range.start_line, range.start_character)?;
    let end = position_offset(source, &starts, range.end_line, range.end_character)?;
    if start > end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Err(ScipError::InvalidRange(format!(
            "range 不是有效 UTF-8 边界：{range:?}"
        )));
    }
    Ok(source[start..end].to_owned())
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

fn position_offset(
    source: &str,
    starts: &[usize],
    line: usize,
    character: usize,
) -> Result<usize, ScipError> {
    let start = *starts
        .get(line)
        .ok_or_else(|| ScipError::InvalidRange(format!("line={line} 越出源码")))?;
    let next = starts.get(line + 1).copied().unwrap_or(source.len());
    let logical_end = source.as_bytes()[start..next]
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n'))
        .map_or(start, |offset| start + offset + 1);
    let offset = start.saturating_add(character);
    if offset > logical_end {
        return Err(ScipError::InvalidRange(format!(
            "line={line} character={character} 越出行尾"
        )));
    }
    Ok(offset)
}

fn symbol_kind(info: &SymbolInformation) -> String {
    match info.kind.enum_value() {
        Ok(kind) => camel_to_snake(&format!("{kind:?}")),
        Err(value) => format!("unknown_kind_{value}"),
    }
}

fn camel_to_snake(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            result.push('_');
        }
        result.push(character.to_ascii_lowercase());
    }
    result
}

fn semantic_definition_fingerprint(
    information: &SymbolInformation,
    display_name: &str,
) -> Option<String> {
    let signature = information.signature_documentation.as_ref()?;
    let text = signature.text.trim();
    if text.is_empty() {
        return None;
    }
    let without_name = if display_name.is_empty() {
        text.to_owned()
    } else {
        text.replace(display_name, "<symbol>")
    };
    let normalized = without_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("sha256_{}", sha256(normalized.as_bytes())))
}

fn sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn validate_import_profile(profile: &ScipImportProfile) -> Result<(), ScipError> {
    if profile.id.trim().is_empty()
        || profile.producer.trim().is_empty()
        || profile.contract_version != 1
        || profile.language_mappings.is_empty()
    {
        return Err(ScipError::ProfileMismatch(format!(
            "profile={:?} 缺少 ID/producer/mapping 或 contract_version 不受支持",
            profile.id
        )));
    }
    let mut raw_languages = BTreeSet::new();
    for mapping in &profile.language_mappings {
        if SourceLanguage::parse(mapping.language.as_str()).as_ref() != Some(&mapping.language)
            || mapping
                .raw_language
                .as_deref()
                .is_some_and(|raw| raw.trim().is_empty())
            || (mapping.raw_language.is_none() != mapping.allow_missing_language)
        {
            return Err(ScipError::ProfileMismatch(format!(
                "profile={} 包含无效 language mapping",
                profile.id
            )));
        }
        let key = mapping
            .raw_language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| "<missing>".to_owned(), ToOwned::to_owned);
        if !raw_languages.insert(key) {
            return Err(ScipError::ProfileMismatch(format!(
                "profile={} 包含重复 raw language mapping",
                profile.id
            )));
        }
    }
    Ok(())
}

/// 返回配置契约对应的稳定 Provider ID。
///
/// 该 ID 同时用于快照命名空间与存储查询；调用方不得根据 producer 版本临时拼接。
pub fn provider_contract_id(profile: &ScipImportProfile) -> String {
    let raw = format!(
        "{}-{}-contract-{}",
        profile.id, profile.producer, profile.contract_version
    );
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in raw.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
            previous_separator = false;
        } else if !previous_separator && !normalized.is_empty() {
            normalized.push('-');
            previous_separator = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if normalized.is_empty() {
        normalized = format!("provider-{}", &sha256(raw.as_bytes())[..16]);
    }
    format!("scip-{normalized}-h{}", sha256(raw.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_symbols::{EdgeKind, propose_lineage_candidates};
    use protobuf::{EnumOrUnknown, Message, MessageField};
    use scip::types::{
        Document, Index, Metadata, Occurrence, PositionEncoding, Relationship, Signature,
        SingleLineRange, SymbolInformation, SymbolRole, ToolInfo, symbol_information,
    };

    use super::{
        CapabilitySupport, ScipError, ScipImport, ScipImportProfile, ScipLanguageMapping,
        extract_range, import_bytes, occurrence_range, semantic_definition_fingerprint,
    };

    const PROJECT_KEY: &str = "project_fixture";
    const CALLER: &str = "rust-analyzer cargo fixture 0.1.0 caller().";
    const TARGET: &str = "rust-analyzer cargo fixture 0.1.0 target().";
    const TRAIT: &str = "rust-analyzer cargo fixture 0.1.0 Worker#";
    const MACRO: &str = "rust-analyzer cargo fixture 0.1.0 make!";

    fn profile(
        id: &str,
        producer: &str,
        mappings: &[(Option<&str>, &str, bool)],
    ) -> ScipImportProfile {
        ScipImportProfile {
            id: id.to_owned(),
            producer: producer.to_owned(),
            contract_version: 1,
            language_mappings: mappings
                .iter()
                .map(
                    |(raw, language, allow_missing_language)| ScipLanguageMapping {
                        raw_language: raw.map(ToOwned::to_owned),
                        language: brain_symbols::SourceLanguage::parse(language).unwrap(),
                        allow_missing_language: *allow_missing_language,
                    },
                )
                .collect(),
        }
    }

    fn rust_profile() -> ScipImportProfile {
        profile(
            "rust-main",
            "rust-analyzer",
            &[(Some("rust"), "rust", false)],
        )
    }

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "project-brain-scip-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn info(symbol: &str, display_name: &str, kind: symbol_information::Kind) -> SymbolInformation {
        SymbolInformation {
            symbol: symbol.to_owned(),
            display_name: display_name.to_owned(),
            kind: EnumOrUnknown::new(kind),
            signature_documentation: MessageField::some(Signature {
                language: "rust".to_owned(),
                text: format!("fn {display_name}()"),
                ..Signature::default()
            }),
            ..SymbolInformation::default()
        }
    }

    fn occurrence(
        symbol: &str,
        line: i32,
        start_character: i32,
        end_character: i32,
        definition: bool,
    ) -> Occurrence {
        let mut occurrence = Occurrence {
            symbol: symbol.to_owned(),
            symbol_roles: if definition {
                SymbolRole::Definition as i32
            } else {
                0
            },
            ..Occurrence::default()
        };
        occurrence.set_single_line_range(SingleLineRange {
            line,
            start_character,
            end_character,
            ..SingleLineRange::default()
        });
        occurrence
    }

    #[test]
    fn empty_occurrence_ranges_are_valid_scip_ranges() {
        let range = occurrence("module", 0, 0, 0, true);
        let parsed = occurrence_range(&range).unwrap();
        assert_eq!(parsed.start_line, 0);
        assert_eq!(parsed.start_character, 0);
        assert_eq!(parsed.end_line, 0);
        assert_eq!(parsed.end_character, 0);
        assert_eq!(extract_range("", parsed).unwrap(), "");
    }

    #[test]
    fn lineage_fingerprint_requires_a_nonempty_producer_signature() {
        let with_signature = info(
            "rust-analyzer cargo demo 0.1.0 run().",
            "run",
            symbol_information::Kind::Function,
        );
        assert!(semantic_definition_fingerprint(&with_signature, "run").is_some());

        let without_signature = SymbolInformation {
            symbol: "rust-analyzer cargo demo 0.1.0 run().".to_owned(),
            display_name: "run".to_owned(),
            kind: EnumOrUnknown::new(symbol_information::Kind::Function),
            ..SymbolInformation::default()
        };
        assert_eq!(
            semantic_definition_fingerprint(&without_signature, "run"),
            None
        );
    }

    fn fixture(root: &Path) -> Vec<u8> {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn caller() { target(); }\ntrait Worker {}\nmacro_rules! make { () => {} }\nlet local = 1;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/other.rs"),
            "pub fn target() {}\nlet other = 2;\n",
        )
        .unwrap();
        let lib = Document {
            language: "rust".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            position_encoding: EnumOrUnknown::new(
                PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
            ),
            occurrences: vec![
                occurrence(CALLER, 0, 0, 25, true),
                occurrence(TARGET, 0, 14, 20, false),
                occurrence(TRAIT, 1, 0, 15, true),
                occurrence(MACRO, 2, 0, 30, true),
                occurrence("local 0", 3, 4, 9, true),
            ],
            symbols: vec![
                info(CALLER, "caller", symbol_information::Kind::Function),
                info(TRAIT, "Worker", symbol_information::Kind::Trait),
                info(MACRO, "make", symbol_information::Kind::Macro),
                info("local 0", "local", symbol_information::Kind::Variable),
            ],
            ..Document::default()
        };
        let other = Document {
            language: "rust".to_owned(),
            relative_path: "src/other.rs".to_owned(),
            position_encoding: EnumOrUnknown::new(
                PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
            ),
            occurrences: vec![
                occurrence(TARGET, 0, 0, 18, true),
                occurrence("local 0", 1, 4, 9, true),
            ],
            symbols: vec![
                info(TARGET, "target", symbol_information::Kind::Function),
                info("local 0", "other", symbol_information::Kind::Variable),
            ],
            ..Document::default()
        };
        Index {
            metadata: MessageField::some(Metadata {
                tool_info: MessageField::some(ToolInfo {
                    name: "rust-analyzer".to_owned(),
                    version: "fixture-1".to_owned(),
                    ..ToolInfo::default()
                }),
                project_root: root.to_string_lossy().into_owned(),
                ..Metadata::default()
            }),
            documents: vec![lib, other],
            ..Index::default()
        }
        .write_to_bytes()
        .unwrap()
    }

    #[derive(Clone, Copy)]
    struct LanguageFixtureSpec<'a> {
        relative_path: &'a str,
        language: &'a str,
        producer: &'a str,
        source: &'a str,
        symbol: &'a str,
        display_name: &'a str,
        kind: symbol_information::Kind,
    }

    fn single_language_fixture(root: &Path, spec: LanguageFixtureSpec<'_>) -> Vec<u8> {
        let path = root.join(spec.relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, spec.source).unwrap();
        Index {
            metadata: MessageField::some(Metadata {
                tool_info: MessageField::some(ToolInfo {
                    name: spec.producer.to_owned(),
                    version: "fixture-1".to_owned(),
                    ..ToolInfo::default()
                }),
                ..Metadata::default()
            }),
            documents: vec![Document {
                language: spec.language.to_owned(),
                relative_path: spec.relative_path.to_owned(),
                position_encoding: EnumOrUnknown::new(
                    PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
                ),
                occurrences: vec![occurrence(
                    spec.symbol,
                    0,
                    0,
                    i32::try_from(spec.source.trim_end_matches(['\r', '\n']).len()).unwrap(),
                    true,
                )],
                symbols: vec![info(spec.symbol, spec.display_name, spec.kind)],
                ..Document::default()
            }],
            ..Index::default()
        }
        .write_to_bytes()
        .unwrap()
    }

    fn dotnet_fixture(root: &Path) -> Vec<u8> {
        let interface_symbol = "scip-dotnet nuget fixture 0.1.0 IWorker#";
        let class_symbol = "scip-dotnet nuget fixture 0.1.0 Worker#";
        let csharp_source = "public interface IWorker {}\n";
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/IWorker.cs"), csharp_source).unwrap();
        fs::write(
            root.join("src/Worker.vb"),
            "Public Class Worker\nEnd Class\n",
        )
        .unwrap();
        let mut worker = info(
            class_symbol,
            "Worker",
            symbol_information::Kind::UnspecifiedKind,
        );
        worker.relationships.push(Relationship {
            symbol: interface_symbol.to_owned(),
            is_implementation: true,
            ..Relationship::default()
        });
        Index {
            metadata: MessageField::some(Metadata {
                tool_info: MessageField::some(ToolInfo {
                    name: "scip-dotnet".to_owned(),
                    version: "0.1.0-SNAPSHOT".to_owned(),
                    ..ToolInfo::default()
                }),
                ..Metadata::default()
            }),
            documents: vec![
                Document {
                    language: "C#".to_owned(),
                    relative_path: "src/IWorker.cs".to_owned(),
                    position_encoding: EnumOrUnknown::new(
                        PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
                    ),
                    occurrences: vec![occurrence(
                        interface_symbol,
                        0,
                        0,
                        i32::try_from(csharp_source.trim_end().len()).unwrap(),
                        true,
                    )],
                    symbols: vec![info(
                        interface_symbol,
                        "IWorker",
                        symbol_information::Kind::UnspecifiedKind,
                    )],
                    ..Document::default()
                },
                Document {
                    language: "Visual Basic".to_owned(),
                    relative_path: "src/Worker.vb".to_owned(),
                    position_encoding: EnumOrUnknown::new(
                        PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
                    ),
                    occurrences: vec![occurrence(class_symbol, 0, 0, 19, true)],
                    symbols: vec![worker],
                    ..Document::default()
                },
            ],
            ..Index::default()
        }
        .write_to_bytes()
        .unwrap()
    }

    #[test]
    fn fixture_decodes_definitions_references_and_kinds_offline() {
        let root = temp_root("offline");
        let bytes = fixture(&root);
        let imported = import_bytes(&root, PROJECT_KEY, "head", &bytes, &rust_profile()).unwrap();

        assert_eq!(imported.stats.documents, 2);
        assert_eq!(imported.stats.definitions, 6);
        assert_eq!(imported.stats.reference_edges, 1);
        assert!(
            imported
                .snapshot
                .symbols
                .iter()
                .any(|symbol| symbol.kind == "trait" && symbol.display_name == "Worker")
        );
        assert!(
            imported
                .snapshot
                .symbols
                .iter()
                .any(|symbol| symbol.kind == "macro" && symbol.display_name == "make")
        );
        assert!(
            imported
                .snapshot
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::References)
        );
        assert_eq!(
            imported.capabilities[0].capabilities.implementations,
            CapabilitySupport::Unsupported
        );
        assert_eq!(
            imported.capabilities[0].capabilities.generated_code,
            CapabilitySupport::Unknown
        );
        assert!(
            imported
                .snapshot
                .edges
                .iter()
                .all(|edge| edge.kind != EdgeKind::Implements && edge.kind != EdgeKind::Imports)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_symbols_are_document_and_snapshot_scoped() {
        let root = temp_root("locals");
        let bytes = fixture(&root);
        let first = import_bytes(&root, PROJECT_KEY, "head", &bytes, &rust_profile()).unwrap();
        let locals = first
            .lineage_observations
            .iter()
            .filter(|observation| observation.is_local)
            .collect::<Vec<_>>();
        assert_eq!(locals.len(), 2);
        assert_ne!(locals[0].symbol_id, locals[1].symbol_id);

        let mut changed_bytes = bytes.clone();
        changed_bytes.extend_from_slice(&[0xA0, 0x06, 0x00]);
        let second =
            import_bytes(&root, PROJECT_KEY, "head", &changed_bytes, &rust_profile()).unwrap();
        let second_locals = second
            .lineage_observations
            .iter()
            .filter(|observation| observation.is_local)
            .collect::<Vec<_>>();
        assert_ne!(locals[0].symbol_id, second_locals[0].symbol_id);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_symbol_is_evidence_not_lineage_identity() {
        let root = temp_root("lineage");
        let bytes = fixture(&root);
        let first = import_bytes(&root, PROJECT_KEY, "head-a", &bytes, &rust_profile()).unwrap();
        let second = import_bytes(&root, PROJECT_KEY, "head-b", &bytes, &rust_profile()).unwrap();
        let old = first
            .lineage_observations
            .into_iter()
            .filter(|item| !item.is_local)
            .collect::<Vec<_>>();
        let mut current = second
            .lineage_observations
            .into_iter()
            .filter(|item| !item.is_local)
            .collect::<Vec<_>>();
        for observation in &mut current {
            observation.symbol_id.push_str("-new-provider-identity");
        }
        assert!(
            old.iter()
                .zip(&current)
                .all(|(before, after)| before.symbol_id != after.symbol_id)
        );
        let candidates = propose_lineage_candidates(&old, &current, &[]).unwrap();

        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.ambiguity_group_id.is_none())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_provider_symbol_does_not_collapse_nodes_or_invent_edges() {
        let root = temp_root("duplicates");
        let bytes = fixture(&root);
        let imported = import_bytes(&root, PROJECT_KEY, "head", &bytes, &rust_profile()).unwrap();
        let locals = imported
            .snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.display_name == "local" || symbol.display_name == "other")
            .collect::<Vec<_>>();

        assert_eq!(locals.len(), 2);
        assert_ne!(locals[0].id, locals[1].id);
        assert_eq!(imported.stats.ambiguous_provider_symbols, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_or_escaping_document_paths_are_rejected() {
        let root = temp_root("escape");
        let bytes = fixture(&root);
        let mut index = Index::parse_from_bytes(&bytes).unwrap();
        index.documents[0].relative_path = "../outside.rs".to_owned();
        let escaped = index.write_to_bytes().unwrap();

        assert!(matches!(
            import_bytes(&root, PROJECT_KEY, "head", &escaped, &rust_profile()),
            Err(ScipError::InvalidDocument(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dotnet_maps_csharp_and_visual_basic_per_document() {
        let root = temp_root("dotnet");
        let bytes = dotnet_fixture(&root);
        let dotnet_profile = profile(
            "dotnet-main",
            "scip-dotnet",
            &[
                (Some("C#"), "csharp", false),
                (Some("Visual Basic"), "visual-basic", false),
            ],
        );
        let imported = import_bytes(&root, PROJECT_KEY, "head", &bytes, &dotnet_profile).unwrap();

        assert_eq!(
            imported.languages,
            vec![
                brain_symbols::SourceLanguage::csharp(),
                brain_symbols::SourceLanguage::parse("visual-basic").unwrap(),
            ]
        );
        assert_eq!(imported.snapshot.provider.version, "contract-1");
        assert!(imported.snapshot.provider.id.contains("dotnet-main"));
        assert!(
            imported
                .snapshot
                .symbols
                .iter()
                .all(|symbol| symbol.kind == "unspecified_kind")
        );
        assert!(
            imported
                .snapshot
                .edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Implements)
        );
        assert_eq!(imported.capabilities.len(), 2);
        assert!(
            imported.capabilities.iter().all(|entry| {
                entry.capabilities.implementations == CapabilitySupport::Supported
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn python_missing_language_requires_explicit_opt_in() {
        let root = temp_root("python");
        let python = single_language_fixture(
            &root,
            LanguageFixtureSpec {
                relative_path: "src/main.py",
                language: "",
                producer: "scip-python",
                source: "def run(): pass\n",
                symbol: "scip-python pypi fixture 0.1.0 run().",
                display_name: "run",
                kind: symbol_information::Kind::UnspecifiedKind,
            },
        );
        assert!(matches!(
            import_bytes(
                &root,
                PROJECT_KEY,
                "head",
                &python,
                &profile(
                    "python-main",
                    "scip-python",
                    &[(Some("python"), "python", false)]
                ),
            ),
            Err(ScipError::ProfileMismatch(_))
        ));
        let python_import = import_bytes(
            &root,
            PROJECT_KEY,
            "head",
            &python,
            &profile("python-main", "scip-python", &[(None, "python", true)]),
        )
        .unwrap();
        assert_eq!(
            python_import.languages,
            vec![brain_symbols::SourceLanguage::python()]
        );
        assert_eq!(python_import.snapshot.symbols[0].kind, "unspecified_kind");
        assert_eq!(
            python_import.capabilities[0].capabilities.implementations,
            CapabilitySupport::Partial
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_language_is_supported_only_through_profile_mapping() {
        let root = temp_root("custom");
        let bytes = single_language_fixture(
            &root,
            LanguageFixtureSpec {
                relative_path: "src/module.custom",
                language: "AcmeLang",
                producer: "acme-scip",
                source: "module Demo\n",
                symbol: "acme package fixture 1 Demo#",
                display_name: "Demo",
                kind: symbol_information::Kind::UnspecifiedKind,
            },
        );
        let imported = import_bytes(
            &root,
            PROJECT_KEY,
            "head",
            &bytes,
            &profile(
                "acme-main",
                "acme-scip",
                &[(Some("AcmeLang"), "acme", false)],
            ),
        )
        .unwrap();
        assert_eq!(imported.languages[0].as_str(), "acme");
        assert_eq!(
            imported.capabilities[0].capabilities.relationships,
            CapabilitySupport::Unknown
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rust_profile_rejects_the_wrapper_name_as_producer() {
        let root = temp_root("rust-wrapper");
        let bytes = fixture(&root);
        let mut index = Index::parse_from_bytes(&bytes).unwrap();
        index
            .metadata
            .as_mut()
            .unwrap()
            .tool_info
            .as_mut()
            .unwrap()
            .name = "scip-rust".to_owned();
        assert!(matches!(
            import_bytes(
                &root,
                PROJECT_KEY,
                "head",
                &index.write_to_bytes().unwrap(),
                &rust_profile(),
            ),
            Err(ScipError::ProfileMismatch(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_contract_namespaces_identity_but_producer_version_is_provenance() {
        let root = temp_root("provider-identity");
        let bytes = fixture(&root);
        let first = import_bytes(&root, PROJECT_KEY, "head", &bytes, &rust_profile()).unwrap();

        let mut version_changed = Index::parse_from_bytes(&bytes).unwrap();
        version_changed
            .metadata
            .as_mut()
            .unwrap()
            .tool_info
            .as_mut()
            .unwrap()
            .version = "fixture-2".to_owned();
        let second = import_bytes(
            &root,
            PROJECT_KEY,
            "head",
            &version_changed.write_to_bytes().unwrap(),
            &rust_profile(),
        )
        .unwrap();
        assert_eq!(first.snapshot.provider, second.snapshot.provider);
        let global_id = |import: &ScipImport| {
            import
                .snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.display_name == "target")
                .unwrap()
                .id
                .clone()
        };
        assert_eq!(global_id(&first), global_id(&second));
        assert_ne!(first.producer_version, second.producer_version);

        let other_profile = profile(
            "rust_main",
            "rust-analyzer",
            &[(Some("rust"), "rust", false)],
        );
        let third = import_bytes(&root, PROJECT_KEY, "head", &bytes, &other_profile).unwrap();
        assert_ne!(first.snapshot.provider.id, third.snapshot.provider.id);
        assert_ne!(global_id(&first), global_id(&third));

        let punctuation_profile = profile(
            "rust.main",
            "rust-analyzer",
            &[(Some("rust"), "rust", false)],
        );
        let punctuation =
            import_bytes(&root, PROJECT_KEY, "head", &bytes, &punctuation_profile).unwrap();
        assert_ne!(third.snapshot.provider.id, punctuation.snapshot.provider.id);
        fs::remove_dir_all(root).unwrap();
    }
}
