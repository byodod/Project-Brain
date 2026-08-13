use std::{
    path::Path,
    time::{SystemTime, SystemTimeError, UNIX_EPOCH},
};

use brain_core::{
    ActionDescriptor, Decision, HOOK_PROTOCOL_VERSION, InternalHookEvent, InternalHookOutcome,
};
use brain_evidence::{
    EvidenceAuthority, EvidenceCoverage, EvidenceFreshness, EvidencePlane, EvidenceSnapshot,
};
use brain_symbols::{
    GraphDelta, IdentityQuality, LINEAGE_EVIDENCE_SCHEMA_VERSION, LineageCandidateProposal,
    LineageConfidence, LineageEvidence, LineageGroupProposal, LineageGroupReviewClass,
    LineageGroupStorageMode, LineageProposalSet, LineageState, LineageSymbolObservation,
    MAX_LINEAGE_GROUP_MEMBERS_PER_SIDE, PathRenameEvidence, SYMBOL_PROTOCOL_VERSION,
    SourceFileState, SourceLanguage, SymbolNode, SymbolSnapshot, SymbolStatus, propose_lineage,
    symbol_id,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DATABASE_SCHEMA_VERSION: i64 = 12;
const LEGACY_LINEAGE_ALGORITHM_ID: &str = "project-brain-lineage";
const LEGACY_LINEAGE_ALGORITHM_VERSION: &str = "1";
const LEGACY_COMPACTION_ALGORITHM_ID: &str = "project-brain-lineage-legacy-compaction";
const LEGACY_COMPACTION_ALGORITHM_VERSION: &str = "1";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite 操作失败：{0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON 序列化失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error("系统时间无效：{0}")]
    Clock(#[from] SystemTimeError),

    #[error("不支持数据库 schema_version={actual}，当前最高支持 {expected}")]
    UnsupportedSchemaVersion { actual: i64, expected: i64 },

    #[error("符号快照无效：{0}")]
    InvalidSnapshot(String),

    #[error("内部 Hook 事件无效：{0}")]
    InvalidHookEvent(String),

    #[error("数据库完整性检查失败：{0}")]
    Integrity(String),

    #[error("数据库中存在无法识别的符号字段：{field}={value:?}")]
    InvalidSymbolField { field: &'static str, value: String },

    #[error("lineage 输入或状态无效：{0}")]
    InvalidLineage(String),

    #[error("lineage 裁决发生冲突：{0}")]
    LineageConflict(String),

    #[error("lineage request_id 已用于不同请求：{0}")]
    LineageIdempotencyConflict(String),

    #[error("Provider 资格状态无效：{0}")]
    InvalidProviderQualification(String),

    #[error("Evidence Snapshot 或状态无效：{0}")]
    InvalidEvidence(String),

    #[error("Evidence staleness event_id 已用于不同事件：{0}")]
    EvidenceIdempotencyConflict(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub id: i64,
    pub event_id: String,
    pub session_id: String,
    pub hook_event: String,
    pub action_json: String,
    pub decision_json: String,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterAuditRecord {
    pub id: i64,
    pub project_key: String,
    pub adapter_kind: String,
    pub adapter_version: u16,
    pub event_id: String,
    pub session_key: String,
    pub event_kind: String,
    pub event_json: String,
    pub outcome_json: Option<String>,
    pub latency_ms: u64,
    pub failure: Option<String>,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterRecordResult {
    Inserted(i64),
    Duplicate(InternalHookOutcome),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticApplyResult {
    pub graph: GraphDelta,
    pub snapshot_inserted: bool,
    pub candidates_inserted: u64,
    pub evidence_inserted: u64,
    pub lineage_groups_inserted: u64,
    pub lineage_group_members_inserted: u64,
    pub potential_lineage_pairs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceApplyResult {
    pub snapshot_fingerprint: String,
    pub snapshot_inserted: bool,
    pub attestation_sequence: u64,
    pub freshness: EvidenceFreshness,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceHeadRecord {
    pub project_key: String,
    pub plane: EvidencePlane,
    pub provider_id: String,
    pub snapshot_fingerprint: String,
    pub freshness: EvidenceFreshness,
    pub stale_event_id: Option<String>,
    pub stale_reason: Option<String>,
    pub updated_at_unix_seconds: i64,
    pub last_attestation_sequence: u64,
    pub snapshot: EvidenceSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceHeadSummary {
    pub project_key: String,
    pub plane: EvidencePlane,
    pub provider_id: String,
    pub provider_version: String,
    pub provider_contract_version: u16,
    pub snapshot_fingerprint: String,
    pub source_fingerprint: String,
    pub coverage: EvidenceCoverage,
    pub authority: EvidenceAuthority,
    pub freshness: EvidenceFreshness,
    pub stale_event_id: Option<String>,
    pub stale_reason: Option<String>,
    pub updated_at_unix_seconds: i64,
    pub last_attestation_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceStaleResult {
    pub event_id: String,
    pub heads_marked: u64,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticSnapshotSource {
    pub worktree_fingerprint: String,
    pub head_revision: String,
    pub worktree_clean: bool,
    pub trust: SemanticSourceTrust,
    pub provider_registration_id: Option<String>,
    pub executable_sha256: Option<String>,
    pub artifact_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticSourceManifest {
    pub snapshot_fingerprint: String,
    pub source: SemanticSnapshotSource,
    /// `false` 表示快照来自 v7 之前，存储层不会凭空补造当时的文档清单。
    pub recorded: bool,
    pub sources: Vec<SourceFileState>,
}

impl SemanticSnapshotSource {
    pub fn offline(
        worktree_fingerprint: String,
        head_revision: String,
        worktree_clean: bool,
    ) -> Self {
        Self {
            worktree_fingerprint,
            head_revision,
            worktree_clean,
            trust: SemanticSourceTrust::OfflineImport,
            provider_registration_id: None,
            executable_sha256: None,
            artifact_sha256: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn trusted_provider(
        worktree_fingerprint: String,
        head_revision: String,
        worktree_clean: bool,
        provider_registration_id: String,
        executable_sha256: String,
        artifact_sha256: String,
    ) -> Self {
        Self {
            worktree_fingerprint,
            head_revision,
            worktree_clean,
            trust: SemanticSourceTrust::TrustedProvider,
            provider_registration_id: Some(provider_registration_id),
            executable_sha256: Some(executable_sha256),
            artifact_sha256: Some(artifact_sha256),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSourceTrust {
    OfflineImport,
    TrustedProvider,
}

impl SemanticSourceTrust {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OfflineImport => "offline_import",
            Self::TrustedProvider => "trusted_provider",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "offline_import" => Some(Self::OfflineImport),
            "trusted_provider" => Some(Self::TrustedProvider),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticResolutionKind {
    DirectSemantic,
    ConfirmedLineage,
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticScopeResolution {
    pub kind: SemanticResolutionKind,
    pub anchor_snapshot_fingerprint: String,
    pub anchor_symbol_id: String,
    pub latest_snapshot_fingerprint: Option<String>,
    pub resolved_symbol: Option<SymbolNode>,
    pub source: Option<SemanticSnapshotSource>,
    pub lineage_decision_ids: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageCandidateRecord {
    pub candidate_id: String,
    pub project_key: String,
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language: SourceLanguage,
    pub from_snapshot_fingerprint: String,
    pub from_symbol_id: String,
    pub to_snapshot_fingerprint: String,
    pub to_symbol_id: String,
    pub state: LineageState,
    pub ambiguity_group_id: Option<String>,
    pub revision: u64,
    pub evidence_count: u64,
    pub created_at_unix_seconds: i64,
    pub updated_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageGroupRecord {
    pub group_id: String,
    pub project_key: String,
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language_id: String,
    pub from_snapshot_fingerprint: String,
    pub to_snapshot_fingerprint: String,
    pub symbol_kind: String,
    pub definition_fingerprint: String,
    pub algorithm_id: String,
    pub algorithm_version: String,
    pub from_count: u64,
    pub to_count: u64,
    pub potential_pair_count: u64,
    pub review_class: String,
    pub storage_mode: String,
    pub from_members_hash: String,
    pub to_members_hash: String,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageGroupDetail {
    pub group: LineageGroupRecord,
    pub from_members: Vec<String>,
    pub to_members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyLineageCompactionGroup {
    pub group_id: String,
    pub legacy_ambiguity_group_id: String,
    pub provider_profile_id: String,
    pub provider_contract_id: String,
    pub language_id: String,
    pub from_snapshot_fingerprint: String,
    pub to_snapshot_fingerprint: String,
    pub symbol_kind: String,
    pub definition_fingerprint: String,
    pub from_count: u64,
    pub to_count: u64,
    pub potential_pair_count: u64,
    pub candidate_count: u64,
    pub evidence_count: u64,
    pub storage_mode: String,
    pub from_members_hash: String,
    pub to_members_hash: String,
    pub candidate_manifest_hash: String,
    pub evidence_manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyLineageCompactionReport {
    pub project_key: String,
    pub operation_version: u32,
    pub mode: String,
    pub applied: bool,
    pub replayed: bool,
    pub request_id: Option<String>,
    pub legacy_ambiguous_candidate_count: u64,
    pub compactable_group_count: u64,
    pub compactable_candidate_count: u64,
    pub compactable_evidence_count: u64,
    pub protected_candidate_count: u64,
    pub group_member_count: u64,
    pub oversized_group_count: u64,
    pub compaction_manifest_hash: String,
    pub groups: Vec<LegacyLineageCompactionGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderQualificationRecord {
    pub sequence: u64,
    pub project_key: String,
    pub provider_profile_id: String,
    pub status: String,
    pub runs: u64,
    pub registration_id: String,
    pub registration_revision: u64,
    pub executable_sha256: String,
    pub source_fingerprint: String,
    pub evidence_manifest_hash: String,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineageDecisionAction {
    Confirm,
    Reject,
    Supersede,
    Invalidate,
}

impl LineageDecisionAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Reject => "reject",
            Self::Supersede => "supersede",
            Self::Invalidate => "invalidate",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "confirm" => Some(Self::Confirm),
            "reject" => Some(Self::Reject),
            "supersede" => Some(Self::Supersede),
            "invalidate" => Some(Self::Invalidate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageDecisionRecord {
    pub decision_id: String,
    pub project_key: String,
    pub request_id: String,
    pub candidate_id: String,
    pub action: LineageDecisionAction,
    pub from_state: LineageState,
    pub to_state: LineageState,
    pub related_candidate_id: Option<String>,
    pub actor_kind: String,
    pub actor_ref: Option<String>,
    pub reason: Option<String>,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineageAdjudicationResult {
    pub decision: LineageDecisionRecord,
    pub candidate: LineageCandidateRecord,
    pub superseded_candidate: Option<LineageCandidateRecord>,
    pub replayed: bool,
}

pub struct BrainStore {
    connection: Connection,
}

impl BrainStore {
    /// 打开或创建 `SQLite` 数据库，并执行幂等 schema 初始化。
    ///
    /// # Errors
    ///
    /// 当数据库无法打开或 schema 初始化失败时返回错误。
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        let store = Self { connection };
        store.initialize()?;
        Ok(store)
    }

    /// 创建仅用于测试或临时计算的内存数据库。
    ///
    /// # Errors
    ///
    /// 当 `SQLite` 初始化失败时返回错误。
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        let store = Self { connection };
        store.initialize()?;
        Ok(store)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "schema 初始化按版本顺序集中执行，避免迁移步骤次序漂移"
    )]
    fn initialize(&self) -> Result<(), StoreError> {
        let metadata_table_existed: bool = self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'metadata'
             )",
            [],
            |row| row.get(0),
        )?;
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS metadata (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS audit_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 hook_event TEXT NOT NULL,
                 action_json TEXT NOT NULL,
                 decision_json TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_audit_events_session
                 ON audit_events(session_id, id);",
        )?;
        let schema_version = self.read_schema_version(metadata_table_existed)?;
        if !(1..=DATABASE_SCHEMA_VERSION).contains(&schema_version) {
            return Err(StoreError::UnsupportedSchemaVersion {
                actual: schema_version,
                expected: DATABASE_SCHEMA_VERSION,
            });
        }
        if schema_version < 4 {
            // V1-V3 的符号图没有项目维度，无法在多项目存储中安全归属。
            // 图是可重建缓存，因此迁移时只清除此缓存，保留动作与 adapter 审计。
            self.connection.execute_batch(
                "DROP TABLE IF EXISTS symbol_edges;
                 DROP TABLE IF EXISTS symbol_nodes;",
            )?;
        }
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbol_nodes (
                 project_key TEXT NOT NULL,
                 id TEXT NOT NULL,
                 provider_id TEXT NOT NULL,
                 identity_quality TEXT NOT NULL,
                 language TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 provider_key TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 path TEXT NOT NULL,
                 start_line INTEGER NOT NULL,
                 end_line INTEGER NOT NULL,
                 content_fingerprint TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('active', 'removed')),
                 first_seen_revision TEXT NOT NULL,
                 last_seen_revision TEXT NOT NULL,
                 PRIMARY KEY(project_key, id)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_symbol_provider_key
                 ON symbol_nodes(project_key, provider_id, provider_key);
             CREATE INDEX IF NOT EXISTS idx_symbol_path_status
                 ON symbol_nodes(project_key, path, status, start_line);
             CREATE TABLE IF NOT EXISTS symbol_edges (
                 project_key TEXT NOT NULL,
                 provider_id TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 target_id TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('active', 'removed')),
                 first_seen_revision TEXT NOT NULL,
                 last_seen_revision TEXT NOT NULL,
                 PRIMARY KEY(project_key, provider_id, source_id, target_id, kind),
                 FOREIGN KEY(project_key, source_id) REFERENCES symbol_nodes(project_key, id),
                 FOREIGN KEY(project_key, target_id) REFERENCES symbol_nodes(project_key, id)
             );
             CREATE INDEX IF NOT EXISTS idx_symbol_edges_source
                 ON symbol_edges(project_key, source_id, status, kind);
             CREATE INDEX IF NOT EXISTS idx_symbol_edges_target
                 ON symbol_edges(project_key, target_id, status, kind);",
        )?;
        self.initialize_lineage_schema()?;
        self.ensure_lineage_v8_schema()?;
        self.ensure_lineage_v9_schema()?;
        self.ensure_provider_qualification_v10_schema()?;
        self.ensure_semantic_attestation_v11_schema(metadata_table_existed, schema_version)?;
        self.ensure_semantic_snapshot_source_columns()?;
        self.initialize_evidence_v12_schema()?;
        self.initialize_adapter_audit_schema()?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [DATABASE_SCHEMA_VERSION.to_string()],
        )?;
        let integrity: String = self
            .connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Integrity(integrity));
        }
        Ok(())
    }

    fn read_schema_version(&self, metadata_table_existed: bool) -> Result<i64, StoreError> {
        let stored = self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored {
            Some(value) => value
                .parse::<i64>()
                .map_err(|_| StoreError::Integrity("schema_version 不是整数".to_owned())),
            None if metadata_table_existed => Err(StoreError::Integrity(
                "已有 metadata 表缺少 schema_version".to_owned(),
            )),
            None => Ok(1),
        }
    }

    fn initialize_adapter_audit_schema(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS adapter_audit_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 adapter_kind TEXT NOT NULL,
                 adapter_version INTEGER NOT NULL,
                 event_id TEXT NOT NULL,
                 session_key TEXT NOT NULL,
                 event_kind TEXT NOT NULL,
                 event_json TEXT NOT NULL,
                 outcome_json TEXT,
                 latency_ms INTEGER NOT NULL,
                 failure TEXT,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, adapter_kind, event_id)
             );
             CREATE INDEX IF NOT EXISTS idx_adapter_audit_project_session
                 ON adapter_audit_events(project_key, adapter_kind, session_key, id);",
        )?;
        Ok(())
    }

    fn initialize_evidence_v12_schema(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS evidence_snapshots (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 plane TEXT NOT NULL CHECK(plane IN ('source', 'semantic', 'engine', 'build', 'runtime')),
                 provider_id TEXT NOT NULL,
                 provider_version TEXT NOT NULL,
                 provider_contract_version INTEGER NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 source_fingerprint TEXT NOT NULL,
                 coverage TEXT NOT NULL CHECK(coverage IN ('complete', 'partial')),
                 authority TEXT NOT NULL CHECK(authority IN ('deterministic', 'heuristic')),
                 snapshot_json TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, plane, provider_id, snapshot_fingerprint)
             );
             CREATE INDEX IF NOT EXISTS idx_evidence_snapshot_latest
                 ON evidence_snapshots(project_key, plane, provider_id, sequence DESC);
             CREATE TABLE IF NOT EXISTS evidence_attestations (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 plane TEXT NOT NULL,
                 provider_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 observed_at_unix_seconds INTEGER NOT NULL,
                 FOREIGN KEY(project_key, plane, provider_id, snapshot_fingerprint)
                     REFERENCES evidence_snapshots(project_key, plane, provider_id, snapshot_fingerprint)
                     ON DELETE RESTRICT
             );
             CREATE INDEX IF NOT EXISTS idx_evidence_attestation_latest
                 ON evidence_attestations(project_key, plane, provider_id, sequence DESC);
             CREATE TABLE IF NOT EXISTS evidence_heads (
                 project_key TEXT NOT NULL,
                 plane TEXT NOT NULL,
                 provider_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 freshness TEXT NOT NULL CHECK(freshness IN ('fresh', 'stale', 'unknown')),
                 stale_event_id TEXT,
                 stale_reason TEXT,
                 updated_at_unix_seconds INTEGER NOT NULL,
                 last_attestation_sequence INTEGER NOT NULL,
                 PRIMARY KEY(project_key, plane, provider_id),
                 FOREIGN KEY(project_key, plane, provider_id, snapshot_fingerprint)
                     REFERENCES evidence_snapshots(project_key, plane, provider_id, snapshot_fingerprint)
                     ON DELETE RESTRICT,
                 FOREIGN KEY(last_attestation_sequence)
                     REFERENCES evidence_attestations(sequence) ON DELETE RESTRICT
             );
             CREATE TABLE IF NOT EXISTS evidence_staleness_events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 plane TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 changed_paths_json TEXT NOT NULL,
                 event_hash TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, event_id)
             );
             CREATE INDEX IF NOT EXISTS idx_evidence_staleness_project
                 ON evidence_staleness_events(project_key, sequence DESC);",
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn initialize_lineage_schema(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_snapshots (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 worktree_fingerprint TEXT NOT NULL DEFAULT '',
                 head_revision TEXT NOT NULL DEFAULT '',
                 worktree_clean INTEGER NOT NULL DEFAULT 0 CHECK(worktree_clean IN (0, 1)),
                 source_trust TEXT NOT NULL DEFAULT 'offline_import'
                     CHECK(source_trust IN ('offline_import', 'trusted_provider')),
                 provider_registration_id TEXT,
                 executable_sha256 TEXT,
                 artifact_sha256 TEXT,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, provider_profile_id, provider_contract_id, snapshot_fingerprint)
             );
             CREATE INDEX IF NOT EXISTS idx_semantic_snapshot_latest
                 ON semantic_snapshots(project_key, provider_profile_id, provider_contract_id, sequence DESC);
             CREATE TABLE IF NOT EXISTS semantic_snapshot_attestations (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 worktree_fingerprint TEXT NOT NULL,
                 head_revision TEXT NOT NULL,
                 worktree_clean INTEGER NOT NULL CHECK(worktree_clean IN (0, 1)),
                 source_trust TEXT NOT NULL
                     CHECK(source_trust IN ('offline_import', 'trusted_provider')),
                 provider_registration_id TEXT,
                 executable_sha256 TEXT,
                 artifact_sha256 TEXT,
                 created_at_unix_seconds INTEGER NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_attestation_identity
                 ON semantic_snapshot_attestations(
                    project_key, provider_profile_id, provider_contract_id,
                    snapshot_fingerprint, worktree_fingerprint, head_revision,
                    worktree_clean, source_trust,
                    IFNULL(provider_registration_id, ''),
                    IFNULL(executable_sha256, ''), IFNULL(artifact_sha256, '')
                 );
             CREATE INDEX IF NOT EXISTS idx_semantic_attestation_latest
                 ON semantic_snapshot_attestations(
                    project_key, provider_profile_id, provider_contract_id,
                    snapshot_fingerprint, sequence DESC
                 );
             CREATE TABLE IF NOT EXISTS semantic_source_manifests (
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 source_count INTEGER NOT NULL CHECK(source_count >= 0),
                 manifest_sha256 TEXT NOT NULL,
                 PRIMARY KEY(project_key, provider_profile_id, provider_contract_id,
                             snapshot_fingerprint),
                 FOREIGN KEY(project_key, provider_profile_id, provider_contract_id,
                             snapshot_fingerprint)
                     REFERENCES semantic_snapshots(project_key, provider_profile_id,
                                                   provider_contract_id, snapshot_fingerprint)
                     ON DELETE RESTRICT
             );
             CREATE TABLE IF NOT EXISTS semantic_source_observations (
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 path TEXT NOT NULL,
                 language_id TEXT NOT NULL,
                 content_fingerprint TEXT NOT NULL,
                 has_syntax_errors INTEGER NOT NULL CHECK(has_syntax_errors IN (0, 1)),
                 PRIMARY KEY(project_key, provider_profile_id, provider_contract_id,
                             snapshot_fingerprint, path),
                 FOREIGN KEY(project_key, provider_profile_id, provider_contract_id,
                             snapshot_fingerprint)
                     REFERENCES semantic_source_manifests(project_key, provider_profile_id,
                                                          provider_contract_id,
                                                          snapshot_fingerprint)
                     ON DELETE RESTRICT
             );
             CREATE INDEX IF NOT EXISTS idx_semantic_source_observation_path
                 ON semantic_source_observations(project_key, path, snapshot_fingerprint);
             CREATE TABLE IF NOT EXISTS semantic_symbol_observations (
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 language_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 symbol_id TEXT NOT NULL,
                 provider_symbol TEXT,
                 is_local INTEGER NOT NULL CHECK(is_local IN (0, 1)),
                 kind TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 path TEXT NOT NULL,
                 normalized_definition_fingerprint TEXT NOT NULL,
                 PRIMARY KEY(project_key, provider_profile_id, provider_contract_id,
                             snapshot_fingerprint, symbol_id),
                 FOREIGN KEY(project_key, symbol_id)
                     REFERENCES symbol_nodes(project_key, id) ON DELETE RESTRICT
             );
             CREATE TABLE IF NOT EXISTS semantic_lineage_candidates (
                 candidate_id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 language_id TEXT NOT NULL,
                 from_snapshot_fingerprint TEXT NOT NULL,
                 from_symbol_id TEXT NOT NULL,
                 to_snapshot_fingerprint TEXT NOT NULL,
                 to_symbol_id TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN
                     ('proposed', 'confirmed', 'rejected', 'superseded', 'invalidated')),
                 ambiguity_group_id TEXT,
                 revision INTEGER NOT NULL DEFAULT 0,
                 created_at_unix_seconds INTEGER NOT NULL,
                 updated_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, provider_profile_id, provider_contract_id, language_id,
                        from_snapshot_fingerprint, from_symbol_id,
                        to_snapshot_fingerprint, to_symbol_id),
                 FOREIGN KEY(project_key, from_symbol_id)
                     REFERENCES symbol_nodes(project_key, id) ON DELETE RESTRICT,
                 FOREIGN KEY(project_key, to_symbol_id)
                     REFERENCES symbol_nodes(project_key, id) ON DELETE RESTRICT
             );
             CREATE UNIQUE INDEX IF NOT EXISTS ux_lineage_confirmed_from
                 ON semantic_lineage_candidates(
                     project_key, provider_profile_id, provider_contract_id, language_id,
                     from_snapshot_fingerprint, from_symbol_id, to_snapshot_fingerprint)
                 WHERE state = 'confirmed';
             CREATE UNIQUE INDEX IF NOT EXISTS ux_lineage_confirmed_to
                 ON semantic_lineage_candidates(
                     project_key, provider_profile_id, provider_contract_id, language_id,
                     from_snapshot_fingerprint, to_snapshot_fingerprint, to_symbol_id)
                 WHERE state = 'confirmed';
             CREATE INDEX IF NOT EXISTS idx_lineage_candidate_query
                 ON semantic_lineage_candidates(project_key, state, updated_at_unix_seconds DESC);
             CREATE TABLE IF NOT EXISTS semantic_lineage_evidence (
                 evidence_id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
                 candidate_id TEXT NOT NULL,
                 algorithm_id TEXT NOT NULL,
                 algorithm_version TEXT NOT NULL,
                 evidence_schema_version INTEGER NOT NULL,
                 input_fingerprint TEXT NOT NULL,
                 confidence_band TEXT NOT NULL CHECK(confidence_band IN ('low', 'medium', 'high')),
                 evidence_json TEXT NOT NULL,
                 evidence_hash TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 FOREIGN KEY(candidate_id)
                     REFERENCES semantic_lineage_candidates(candidate_id) ON DELETE RESTRICT,
                 UNIQUE(candidate_id, algorithm_id, algorithm_version,
                        input_fingerprint, evidence_hash)
             );
             CREATE TABLE IF NOT EXISTS semantic_lineage_decisions (
                 decision_id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
                 project_key TEXT NOT NULL,
                 request_id TEXT NOT NULL,
                 request_hash TEXT NOT NULL,
                 candidate_id TEXT NOT NULL,
                 action TEXT NOT NULL CHECK(action IN
                     ('confirm', 'reject', 'supersede', 'invalidate')),
                 from_state TEXT NOT NULL,
                 to_state TEXT NOT NULL,
                 related_candidate_id TEXT,
                 actor_kind TEXT NOT NULL,
                 actor_ref TEXT,
                 reason TEXT,
                 created_at_unix_seconds INTEGER NOT NULL,
                 FOREIGN KEY(candidate_id)
                     REFERENCES semantic_lineage_candidates(candidate_id) ON DELETE RESTRICT,
                 UNIQUE(project_key, request_id)
             );",
        )?;
        Ok(())
    }

    fn ensure_lineage_v8_schema(&self) -> Result<(), StoreError> {
        let columns = self
            .connection
            .prepare("PRAGMA table_info(semantic_lineage_candidates)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        if !columns.contains("origin_group_id") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_lineage_candidates ADD COLUMN origin_group_id TEXT;",
            )?;
        }
        if !columns.contains("proposal_origin") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_lineage_candidates ADD COLUMN proposal_origin TEXT NOT NULL DEFAULT 'legacy_v7' CHECK(proposal_origin IN ('auto_unique', 'human_group_pair', 'legacy_v7'));",
            )?;
        }
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_lineage_groups (
                 group_id TEXT PRIMARY KEY,
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 language_id TEXT NOT NULL,
                 from_snapshot_fingerprint TEXT NOT NULL,
                 to_snapshot_fingerprint TEXT NOT NULL,
                 symbol_kind TEXT NOT NULL,
                 definition_fingerprint TEXT NOT NULL,
                 algorithm_id TEXT NOT NULL,
                 algorithm_version TEXT NOT NULL,
                 from_count INTEGER NOT NULL CHECK(from_count > 0),
                 to_count INTEGER NOT NULL CHECK(to_count > 0),
                 potential_pair_count INTEGER NOT NULL CHECK(potential_pair_count > 0),
                 review_class TEXT NOT NULL CHECK(review_class IN ('unique', 'ambiguous', 'oversized')),
                 storage_mode TEXT NOT NULL CHECK(storage_mode IN ('members', 'summary_only')),
                 from_members_hash TEXT NOT NULL,
                 to_members_hash TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, provider_profile_id, provider_contract_id, language_id,
                        from_snapshot_fingerprint, to_snapshot_fingerprint, symbol_kind,
                        definition_fingerprint, algorithm_id, algorithm_version)
             );
             CREATE INDEX IF NOT EXISTS idx_lineage_group_query
                 ON semantic_lineage_groups(project_key, review_class, created_at_unix_seconds DESC);
             CREATE TABLE IF NOT EXISTS semantic_lineage_group_members (
                 group_id TEXT NOT NULL,
                 side TEXT NOT NULL CHECK(side IN ('from', 'to')),
                 symbol_id TEXT NOT NULL,
                 PRIMARY KEY(group_id, side, symbol_id),
                 FOREIGN KEY(group_id) REFERENCES semantic_lineage_groups(group_id) ON DELETE RESTRICT
             );
             CREATE TABLE IF NOT EXISTS semantic_lineage_generation_runs (
                 run_id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 language_id TEXT NOT NULL,
                 from_snapshot_fingerprint TEXT NOT NULL,
                 to_snapshot_fingerprint TEXT NOT NULL,
                 algorithm_id TEXT NOT NULL,
                 algorithm_version TEXT NOT NULL,
                 group_count INTEGER NOT NULL,
                 unique_group_count INTEGER NOT NULL,
                 ambiguous_group_count INTEGER NOT NULL,
                 oversized_group_count INTEGER NOT NULL,
                 potential_pair_count INTEGER NOT NULL,
                 materialized_candidate_count INTEGER NOT NULL,
                 group_manifest_hash TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, provider_profile_id, provider_contract_id, language_id,
                        from_snapshot_fingerprint, to_snapshot_fingerprint,
                        algorithm_id, algorithm_version)
             );",
        )?;
        Ok(())
    }

    fn ensure_lineage_v9_schema(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_lineage_compaction_runs (
                 run_id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
                 project_key TEXT NOT NULL,
                 request_id TEXT NOT NULL,
                 request_hash TEXT NOT NULL,
                 operation_version INTEGER NOT NULL,
                 compacted_group_count INTEGER NOT NULL,
                 compacted_candidate_count INTEGER NOT NULL,
                 compacted_evidence_count INTEGER NOT NULL,
                 protected_candidate_count INTEGER NOT NULL,
                 compaction_manifest_hash TEXT NOT NULL,
                 report_json TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL,
                 UNIQUE(project_key, request_id)
             );
             CREATE TABLE IF NOT EXISTS semantic_lineage_compaction_groups (
                 run_id TEXT NOT NULL,
                 group_id TEXT NOT NULL,
                 legacy_ambiguity_group_id TEXT NOT NULL,
                 candidate_count INTEGER NOT NULL CHECK(candidate_count > 0),
                 evidence_count INTEGER NOT NULL CHECK(evidence_count > 0),
                 candidate_manifest_hash TEXT NOT NULL,
                 evidence_manifest_hash TEXT NOT NULL,
                 PRIMARY KEY(run_id, group_id),
                 FOREIGN KEY(run_id) REFERENCES semantic_lineage_compaction_runs(run_id)
                     ON DELETE RESTRICT,
                 FOREIGN KEY(group_id) REFERENCES semantic_lineage_groups(group_id)
                     ON DELETE RESTRICT
             );
             CREATE INDEX IF NOT EXISTS idx_lineage_compaction_project
                 ON semantic_lineage_compaction_runs(project_key, created_at_unix_seconds DESC);",
        )?;
        Ok(())
    }

    fn ensure_provider_qualification_v10_schema(&self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_provider_qualification_events (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN
                     ('stable_complete', 'stable_incomplete', 'nondeterministic')),
                 runs INTEGER NOT NULL CHECK(runs >= 2),
                 registration_id TEXT NOT NULL,
                 registration_revision INTEGER NOT NULL CHECK(registration_revision > 0),
                 executable_sha256 TEXT NOT NULL,
                 source_fingerprint TEXT NOT NULL,
                 evidence_manifest_hash TEXT NOT NULL,
                 created_at_unix_seconds INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_provider_qualification_latest
                 ON semantic_provider_qualification_events(
                    project_key, provider_profile_id, sequence DESC
                 );",
        )?;
        Ok(())
    }

    fn ensure_semantic_attestation_v11_schema(
        &self,
        metadata_table_existed: bool,
        schema_version: i64,
    ) -> Result<(), StoreError> {
        if !metadata_table_existed || schema_version >= 11 {
            return Ok(());
        }
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute_batch(
            "CREATE TABLE semantic_snapshot_attestations_v11 (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_key TEXT NOT NULL,
                 provider_profile_id TEXT NOT NULL,
                 provider_contract_id TEXT NOT NULL,
                 snapshot_fingerprint TEXT NOT NULL,
                 worktree_fingerprint TEXT NOT NULL,
                 head_revision TEXT NOT NULL,
                 worktree_clean INTEGER NOT NULL CHECK(worktree_clean IN (0, 1)),
                 source_trust TEXT NOT NULL
                     CHECK(source_trust IN ('offline_import', 'trusted_provider')),
                 provider_registration_id TEXT,
                 executable_sha256 TEXT,
                 artifact_sha256 TEXT,
                 created_at_unix_seconds INTEGER NOT NULL
             );
             INSERT INTO semantic_snapshot_attestations_v11(
                 sequence, project_key, provider_profile_id, provider_contract_id,
                 snapshot_fingerprint, worktree_fingerprint, head_revision,
                 worktree_clean, source_trust, provider_registration_id,
                 executable_sha256, artifact_sha256, created_at_unix_seconds
             )
             SELECT sequence, project_key, provider_profile_id, provider_contract_id,
                    snapshot_fingerprint, worktree_fingerprint, head_revision,
                    worktree_clean, source_trust, provider_registration_id,
                    executable_sha256, artifact_sha256, created_at_unix_seconds
             FROM semantic_snapshot_attestations ORDER BY sequence;
             DROP TABLE semantic_snapshot_attestations;
             ALTER TABLE semantic_snapshot_attestations_v11
                 RENAME TO semantic_snapshot_attestations;
             CREATE UNIQUE INDEX idx_semantic_attestation_identity
                 ON semantic_snapshot_attestations(
                    project_key, provider_profile_id, provider_contract_id,
                    snapshot_fingerprint, worktree_fingerprint, head_revision,
                    worktree_clean, source_trust,
                    IFNULL(provider_registration_id, ''),
                    IFNULL(executable_sha256, ''), IFNULL(artifact_sha256, '')
                 );
             CREATE INDEX idx_semantic_attestation_latest
                 ON semantic_snapshot_attestations(
                    project_key, provider_profile_id, provider_contract_id,
                    snapshot_fingerprint, sequence DESC
                 );",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn ensure_semantic_snapshot_source_columns(&self) -> Result<(), StoreError> {
        let mut statement = self
            .connection
            .prepare("PRAGMA table_info(semantic_snapshots)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        if !columns.contains("worktree_fingerprint") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_snapshots ADD COLUMN worktree_fingerprint TEXT NOT NULL DEFAULT '';",
            )?;
        }
        if !columns.contains("head_revision") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_snapshots ADD COLUMN head_revision TEXT NOT NULL DEFAULT '';",
            )?;
        }
        if !columns.contains("worktree_clean") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_snapshots ADD COLUMN worktree_clean INTEGER NOT NULL DEFAULT 0 CHECK(worktree_clean IN (0, 1));",
            )?;
        }
        if !columns.contains("source_trust") {
            self.connection.execute_batch(
                "ALTER TABLE semantic_snapshots ADD COLUMN source_trust TEXT NOT NULL DEFAULT 'offline_import' CHECK(source_trust IN ('offline_import', 'trusted_provider'));",
            )?;
        }
        for (column, definition) in [
            ("provider_registration_id", "TEXT"),
            ("executable_sha256", "TEXT"),
            ("artifact_sha256", "TEXT"),
        ] {
            if !columns.contains(column) {
                self.connection.execute_batch(&format!(
                    "ALTER TABLE semantic_snapshots ADD COLUMN {column} {definition};"
                ))?;
            }
        }
        Ok(())
    }

    /// 原子保存一份不可变 Evidence Snapshot，追加轻量运行证明，并把该 provider 的当前 head 恢复为 fresh。
    /// 相同 fingerprint 的完整 JSON 只保存一次；每次真实运行只追加一行 attestation。
    ///
    /// # Errors
    ///
    /// 当快照协议无效、数据库中的同 fingerprint 内容不一致，或事务失败时返回错误。
    pub fn apply_evidence_snapshot(
        &self,
        snapshot: &EvidenceSnapshot,
    ) -> Result<EvidenceApplyResult, StoreError> {
        snapshot
            .validate()
            .map_err(|error| StoreError::InvalidEvidence(error.to_string()))?;
        let snapshot_json = serde_json::to_string(snapshot)?;
        let now = unix_seconds()?;
        let transaction = self.connection.unchecked_transaction()?;
        let existing_json = transaction
            .query_row(
                "SELECT snapshot_json FROM evidence_snapshots
                 WHERE project_key = ?1 AND plane = ?2 AND provider_id = ?3
                   AND snapshot_fingerprint = ?4",
                params![
                    snapshot.project_key,
                    snapshot.plane.as_str(),
                    snapshot.provider.id,
                    snapshot.snapshot_fingerprint
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing_json) = existing_json.as_deref()
            && existing_json != snapshot_json
        {
            return Err(StoreError::InvalidEvidence(format!(
                "snapshot fingerprint={} 对应的不可变内容发生冲突",
                snapshot.snapshot_fingerprint
            )));
        }
        let snapshot_inserted = existing_json.is_none();
        if snapshot_inserted {
            transaction.execute(
                "INSERT INTO evidence_snapshots(
                     project_key, plane, provider_id, provider_version,
                     provider_contract_version, snapshot_fingerprint, source_fingerprint,
                     coverage, authority, snapshot_json, created_at_unix_seconds
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    snapshot.project_key,
                    snapshot.plane.as_str(),
                    snapshot.provider.id,
                    snapshot.provider.version,
                    snapshot.provider.contract_version,
                    snapshot.snapshot_fingerprint,
                    snapshot.source_fingerprint,
                    snapshot.coverage.as_str(),
                    snapshot.provider.authority.as_str(),
                    snapshot_json,
                    now,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO evidence_attestations(
                 project_key, plane, provider_id, snapshot_fingerprint, observed_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                snapshot.project_key,
                snapshot.plane.as_str(),
                snapshot.provider.id,
                snapshot.snapshot_fingerprint,
                now,
            ],
        )?;
        let attestation_rowid = transaction.last_insert_rowid();
        let attestation_sequence = u64::try_from(attestation_rowid).map_err(|_| {
            StoreError::Integrity("Evidence attestation sequence 超出 u64".to_owned())
        })?;
        transaction.execute(
            "INSERT INTO evidence_heads(
                 project_key, plane, provider_id, snapshot_fingerprint, freshness,
                 stale_event_id, stale_reason, updated_at_unix_seconds, last_attestation_sequence
             ) VALUES (?1, ?2, ?3, ?4, 'fresh', NULL, NULL, ?5, ?6)
             ON CONFLICT(project_key, plane, provider_id) DO UPDATE SET
                 snapshot_fingerprint = excluded.snapshot_fingerprint,
                 freshness = 'fresh',
                 stale_event_id = NULL,
                 stale_reason = NULL,
                 updated_at_unix_seconds = excluded.updated_at_unix_seconds,
                 last_attestation_sequence = excluded.last_attestation_sequence",
            params![
                snapshot.project_key,
                snapshot.plane.as_str(),
                snapshot.provider.id,
                snapshot.snapshot_fingerprint,
                now,
                attestation_rowid,
            ],
        )?;
        transaction.commit()?;
        Ok(EvidenceApplyResult {
            snapshot_fingerprint: snapshot.snapshot_fingerprint.clone(),
            snapshot_inserted,
            attestation_sequence,
            freshness: EvidenceFreshness::Fresh,
        })
    }

    /// 查询项目当前的 Evidence heads。返回的不可变快照会再次执行协议校验。
    ///
    /// # Errors
    ///
    /// 当数据库记录无法反序列化、违反 Evidence Protocol，或查询失败时返回错误。
    pub fn list_evidence_heads(
        &self,
        project_key: &str,
    ) -> Result<Vec<EvidenceHeadRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT h.project_key, h.plane, h.provider_id, h.snapshot_fingerprint,
                    h.freshness, h.stale_event_id, h.stale_reason,
                    h.updated_at_unix_seconds, h.last_attestation_sequence, s.snapshot_json
             FROM evidence_heads h
             JOIN evidence_snapshots s
               ON s.project_key = h.project_key AND s.plane = h.plane
              AND s.provider_id = h.provider_id
              AND s.snapshot_fingerprint = h.snapshot_fingerprint
             WHERE h.project_key = ?1
             ORDER BY h.plane, h.provider_id",
        )?;
        let rows = statement.query_map([project_key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                stored_project_key,
                plane,
                provider_id,
                snapshot_fingerprint,
                freshness,
                stale_event_id,
                stale_reason,
                updated_at_unix_seconds,
                last_attestation_sequence,
                snapshot_json,
            ) = row?;
            let plane = EvidencePlane::parse(&plane).ok_or_else(|| {
                StoreError::InvalidEvidence(format!("无法识别 evidence plane={plane:?}"))
            })?;
            let freshness = EvidenceFreshness::parse(&freshness).ok_or_else(|| {
                StoreError::InvalidEvidence(format!("无法识别 evidence freshness={freshness:?}"))
            })?;
            let snapshot: EvidenceSnapshot = serde_json::from_str(&snapshot_json)?;
            snapshot
                .validate()
                .map_err(|error| StoreError::InvalidEvidence(error.to_string()))?;
            if snapshot.project_key != stored_project_key
                || snapshot.plane != plane
                || snapshot.provider.id != provider_id
                || snapshot.snapshot_fingerprint != snapshot_fingerprint
            {
                return Err(StoreError::InvalidEvidence(
                    "Evidence head 与不可变 snapshot 的身份字段不一致".to_owned(),
                ));
            }
            records.push(EvidenceHeadRecord {
                project_key: stored_project_key,
                plane,
                provider_id,
                snapshot_fingerprint,
                freshness,
                stale_event_id,
                stale_reason,
                updated_at_unix_seconds,
                last_attestation_sequence: u64::try_from(last_attestation_sequence).map_err(
                    |_| StoreError::Integrity("Evidence attestation sequence 为负数".to_owned()),
                )?,
                snapshot,
            });
        }
        Ok(records)
    }

    /// 查询当前 Evidence heads 的轻量状态，不加载完整 `ArtifactGraph` JSON。
    ///
    /// # Errors
    ///
    /// 当数据库包含未知枚举、负数序列，或查询失败时返回错误。
    pub fn list_evidence_head_summaries(
        &self,
        project_key: &str,
    ) -> Result<Vec<EvidenceHeadSummary>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT h.project_key, h.plane, h.provider_id, s.provider_version,
                    s.provider_contract_version, h.snapshot_fingerprint,
                    s.source_fingerprint, s.coverage, s.authority, h.freshness,
                    h.stale_event_id, h.stale_reason, h.updated_at_unix_seconds,
                    h.last_attestation_sequence
             FROM evidence_heads h
             JOIN evidence_snapshots s
               ON s.project_key = h.project_key AND s.plane = h.plane
              AND s.provider_id = h.provider_id
              AND s.snapshot_fingerprint = h.snapshot_fingerprint
             WHERE h.project_key = ?1
             ORDER BY h.plane, h.provider_id",
        )?;
        let rows = statement.query_map([project_key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                stored_project_key,
                plane,
                provider_id,
                provider_version,
                provider_contract_version,
                snapshot_fingerprint,
                source_fingerprint,
                coverage,
                authority,
                freshness,
                stale_event_id,
                stale_reason,
                updated_at_unix_seconds,
                last_attestation_sequence,
            ) = row?;
            records.push(EvidenceHeadSummary {
                project_key: stored_project_key,
                plane: EvidencePlane::parse(&plane).ok_or_else(|| {
                    StoreError::InvalidEvidence(format!("无法识别 evidence plane={plane:?}"))
                })?,
                provider_id,
                provider_version,
                provider_contract_version: u16::try_from(provider_contract_version).map_err(
                    |_| {
                        StoreError::InvalidEvidence("provider contract version 超出 u16".to_owned())
                    },
                )?,
                snapshot_fingerprint,
                source_fingerprint,
                coverage: EvidenceCoverage::parse(&coverage).ok_or_else(|| {
                    StoreError::InvalidEvidence(format!("无法识别 evidence coverage={coverage:?}"))
                })?,
                authority: EvidenceAuthority::parse(&authority).ok_or_else(|| {
                    StoreError::InvalidEvidence(format!(
                        "无法识别 evidence authority={authority:?}"
                    ))
                })?,
                freshness: EvidenceFreshness::parse(&freshness).ok_or_else(|| {
                    StoreError::InvalidEvidence(format!(
                        "无法识别 evidence freshness={freshness:?}"
                    ))
                })?,
                stale_event_id,
                stale_reason,
                updated_at_unix_seconds,
                last_attestation_sequence: u64::try_from(last_attestation_sequence).map_err(
                    |_| StoreError::Integrity("Evidence attestation sequence 为负数".to_owned()),
                )?,
            });
        }
        Ok(records)
    }

    /// 以幂等事件把项目某个 Evidence Plane 的所有当前 heads 标记为 stale。
    ///
    /// # Errors
    ///
    /// 当事件标识/原因非法、同一 `event_id` 被复用于不同内容，或事务失败时返回错误。
    pub fn mark_evidence_plane_stale(
        &self,
        project_key: &str,
        plane: EvidencePlane,
        event_id: &str,
        reason: &str,
        changed_paths: &[String],
    ) -> Result<EvidenceStaleResult, StoreError> {
        if !is_valid_project_key(project_key)
            || event_id.trim().is_empty()
            || reason.trim().is_empty()
            || event_id.len() > 256
            || reason.len() > 2048
            || event_id.contains(['\0', '\n', '\r'])
            || reason.contains('\0')
        {
            return Err(StoreError::InvalidEvidence(
                "staleness event 缺少合法 project_key/event_id/reason".to_owned(),
            ));
        }
        let mut paths = changed_paths.to_vec();
        paths.sort();
        paths.dedup();
        if paths.len() > 4_096
            || paths.iter().any(|path| {
                path.trim().is_empty() || path.len() > 4_096 || path.contains(['\0', '\n', '\r'])
            })
        {
            return Err(StoreError::InvalidEvidence(
                "staleness event 的 changed path 数量或格式无效".to_owned(),
            ));
        }
        let changed_paths_json = serde_json::to_string(&paths)?;
        let event_hash = fingerprint_parts(&[
            project_key.as_bytes(),
            plane.as_str().as_bytes(),
            event_id.as_bytes(),
            reason.as_bytes(),
            changed_paths_json.as_bytes(),
        ]);
        let now = unix_seconds()?;
        let transaction = self.connection.unchecked_transaction()?;
        let existing_hash = transaction
            .query_row(
                "SELECT event_hash FROM evidence_staleness_events
                 WHERE project_key = ?1 AND event_id = ?2",
                params![project_key, event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing_hash) = existing_hash {
            if existing_hash != event_hash {
                return Err(StoreError::EvidenceIdempotencyConflict(event_id.to_owned()));
            }
            transaction.commit()?;
            return Ok(EvidenceStaleResult {
                event_id: event_id.to_owned(),
                heads_marked: 0,
                replayed: true,
            });
        }
        transaction.execute(
            "INSERT INTO evidence_staleness_events(
                 project_key, event_id, plane, reason, changed_paths_json,
                 event_hash, created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                project_key,
                event_id,
                plane.as_str(),
                reason,
                changed_paths_json,
                event_hash,
                now,
            ],
        )?;
        let heads_marked = u64::try_from(transaction.execute(
            "UPDATE evidence_heads
             SET freshness = 'stale', stale_event_id = ?1, stale_reason = ?2,
                 updated_at_unix_seconds = ?3
             WHERE project_key = ?4 AND plane = ?5 AND freshness != 'stale'",
            params![event_id, reason, now, project_key, plane.as_str()],
        )?)
        .map_err(|_| StoreError::Integrity("stale head 数量超出 u64".to_owned()))?;
        transaction.commit()?;
        Ok(EvidenceStaleResult {
            event_id: event_id.to_owned(),
            heads_marked,
            replayed: false,
        })
    }

    /// 原子应用一个 Provider 的完整符号快照，并把快照中消失的旧节点标记为 removed。
    ///
    /// # Errors
    ///
    /// 快照协议不匹配、Provider 边界不一致，或 `SQLite` 事务失败时返回错误。
    pub fn apply_symbol_snapshot(
        &self,
        snapshot: &SymbolSnapshot,
    ) -> Result<GraphDelta, StoreError> {
        validate_snapshot(snapshot)?;
        let transaction = self.connection.unchecked_transaction()?;
        let delta = apply_snapshot_transaction(&transaction, snapshot)?;
        transaction.commit()?;
        Ok(delta)
    }

    /// 原子应用语义快照、保存不可变 symbol observations，并为相邻快照持久化 lineage 候选证据。
    /// 候选生成永远不会改变已有人工裁决状态。
    ///
    /// # Errors
    ///
    /// 快照、观察或 provider 边界不一致，或 `SQLite` 事务失败时返回错误。
    #[allow(
        clippy::too_many_lines,
        reason = "单个事务必须线性地提交快照、group、唯一候选、manifest 与 attestation"
    )]
    pub fn apply_semantic_snapshot(
        &self,
        snapshot: &SymbolSnapshot,
        provider_profile_id: &str,
        observations: &[LineageSymbolObservation],
        path_renames: &[PathRenameEvidence],
        source: &SemanticSnapshotSource,
    ) -> Result<SemanticApplyResult, StoreError> {
        validate_snapshot(snapshot)?;
        validate_semantic_observations(snapshot, provider_profile_id, observations)?;
        validate_semantic_snapshot_source(source)?;
        let now = unix_seconds()?;
        let transaction = self.connection.unchecked_transaction()?;
        let existing: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM semantic_snapshots
                 WHERE project_key = ?1 AND provider_profile_id = ?2
                   AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4
             )",
            params![
                snapshot.project_key,
                provider_profile_id,
                snapshot.provider.id,
                snapshot.source_revision
            ],
            |row| row.get(0),
        )?;
        let source_manifest_recorded =
            semantic_source_manifest_recorded(&transaction, snapshot, provider_profile_id)?;
        let latest_snapshot = transaction
            .query_row(
                "SELECT snapshot_fingerprint FROM semantic_snapshots
                 WHERE project_key = ?1 AND provider_profile_id = ?2
                   AND provider_contract_id = ?3
                 ORDER BY sequence DESC LIMIT 1",
                params![
                    snapshot.project_key,
                    provider_profile_id,
                    snapshot.provider.id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing && latest_snapshot.as_deref() != Some(snapshot.source_revision.as_str()) {
            return Err(StoreError::InvalidLineage(format!(
                "不能把历史 snapshot={} 重新应用为当前图",
                snapshot.source_revision
            )));
        }
        let previous = if existing {
            Vec::new()
        } else {
            latest_semantic_observations(
                &transaction,
                &snapshot.project_key,
                provider_profile_id,
                &snapshot.provider.id,
            )?
        };
        let proposals = if existing {
            LineageProposalSet::default()
        } else {
            propose_lineage(&previous, observations, path_renames)
        };
        let graph = apply_snapshot_transaction(&transaction, snapshot)?;
        let mut candidates_inserted = 0_u64;
        let mut evidence_inserted = 0_u64;
        let mut lineage_groups_inserted = 0_u64;
        let mut lineage_group_members_inserted = 0_u64;
        let potential_lineage_pairs = proposals
            .groups
            .iter()
            .try_fold(0_u64, |total, group| {
                total.checked_add(group.potential_pair_count)
            })
            .ok_or_else(|| {
                StoreError::InvalidLineage("potential lineage pair 计数溢出".to_owned())
            })?;
        if !existing {
            transaction.execute(
                "INSERT INTO semantic_snapshots(
                     project_key, provider_profile_id, provider_contract_id,
                     snapshot_fingerprint, worktree_fingerprint, head_revision,
                     worktree_clean, source_trust, provider_registration_id,
                     executable_sha256, artifact_sha256, created_at_unix_seconds
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    snapshot.project_key,
                    provider_profile_id,
                    snapshot.provider.id,
                    snapshot.source_revision,
                    source.worktree_fingerprint,
                    source.head_revision,
                    source.worktree_clean,
                    source.trust.as_str(),
                    source.provider_registration_id,
                    source.executable_sha256,
                    source.artifact_sha256,
                    now,
                ],
            )?;
            persist_semantic_observations(&transaction, observations)?;
            (lineage_groups_inserted, lineage_group_members_inserted) =
                persist_lineage_groups(&transaction, &proposals.groups, now)?;
            (candidates_inserted, evidence_inserted) =
                persist_lineage_proposals(&transaction, &proposals.candidates, now)?;
            persist_lineage_generation_runs(&transaction, &proposals, now)?;
        }
        if !source_manifest_recorded {
            // v6 及更早快照只有在真实 Provider/离线导入再次提交同一完整快照时才补录，
            // 迁移本身绝不从符号表反推缺失文档。
            persist_semantic_source_manifest(&transaction, snapshot, provider_profile_id)?;
        }
        persist_semantic_attestation(&transaction, snapshot, provider_profile_id, source, now)?;
        transaction.commit()?;
        Ok(SemanticApplyResult {
            graph,
            snapshot_inserted: !existing,
            candidates_inserted,
            evidence_inserted,
            lineage_groups_inserted,
            lineage_group_members_inserted,
            potential_lineage_pairs,
        })
    }

    /// 读取某个 Provider 契约的最新、可审计源码清单。
    ///
    /// v7 之前的快照会返回 `recorded=false`，调用方必须将其视为不可验证，而不是空清单。
    ///
    /// # Errors
    ///
    /// 查询边界无效、SQLite 读取失败或 manifest 完整性校验失败时返回错误。
    pub fn latest_semantic_source_manifest(
        &self,
        project_key: &str,
        provider_profile_id: &str,
        provider_contract_id: &str,
    ) -> Result<Option<SemanticSourceManifest>, StoreError> {
        if !is_valid_project_key(project_key)
            || provider_profile_id.trim().is_empty()
            || provider_contract_id.trim().is_empty()
        {
            return Err(StoreError::InvalidSnapshot(
                "源码清单查询边界无效".to_owned(),
            ));
        }
        let boundary = SemanticScopeBoundary {
            project_key,
            provider_profile_id,
            provider_contract_id,
            language_id: "",
        };
        let Some(latest) = semantic_snapshot_chain(&self.connection, &boundary, "")?.pop() else {
            return Ok(None);
        };
        let fingerprint = latest.fingerprint;
        let source = latest.source;
        let manifest = self
            .connection
            .query_row(
                "SELECT source_count, manifest_sha256
                 FROM semantic_source_manifests
                 WHERE project_key = ?1 AND provider_profile_id = ?2
                   AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4",
                params![
                    project_key,
                    provider_profile_id,
                    provider_contract_id,
                    fingerprint,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((source_count, expected_hash)) = manifest else {
            return Ok(Some(SemanticSourceManifest {
                snapshot_fingerprint: fingerprint,
                source,
                recorded: false,
                sources: Vec::new(),
            }));
        };
        let source_count = u64::try_from(source_count).map_err(|_| {
            StoreError::Integrity(format!(
                "semantic source manifest={fingerprint} 包含负数 source_count"
            ))
        })?;
        let sources = semantic_source_observations(&self.connection, &boundary, &fingerprint)?;
        if u64::try_from(sources.len()).unwrap_or(u64::MAX) != source_count
            || semantic_source_manifest_hash(&sources) != expected_hash
        {
            return Err(StoreError::Integrity(format!(
                "semantic source manifest={fingerprint} 计数或摘要不匹配"
            )));
        }
        Ok(Some(SemanticSourceManifest {
            snapshot_fingerprint: fingerprint,
            source,
            recorded: true,
            sources,
        }))
    }

    /// 查询当前项目的 lineage ledger，不读取或改写符号身份。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或持久化字段无效时返回错误。
    pub fn list_lineage_candidates(
        &self,
        project_key: &str,
        state: Option<LineageState>,
        snapshot_fingerprint: Option<&str>,
        ambiguity_group_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<LineageCandidateRecord>, StoreError> {
        let state = state.map(LineageState::as_str).unwrap_or_default();
        let snapshot = snapshot_fingerprint.unwrap_or_default();
        let ambiguity = ambiguity_group_id.unwrap_or_default();
        let mut statement = self.connection.prepare(
            "SELECT c.candidate_id, c.project_key, c.provider_profile_id,
                    c.provider_contract_id, c.language_id,
                    c.from_snapshot_fingerprint, c.from_symbol_id,
                    c.to_snapshot_fingerprint, c.to_symbol_id, c.state,
                    c.ambiguity_group_id, c.revision,
                    (SELECT COUNT(*) FROM semantic_lineage_evidence e
                     WHERE e.candidate_id = c.candidate_id),
                    c.created_at_unix_seconds, c.updated_at_unix_seconds
             FROM semantic_lineage_candidates c
             WHERE c.project_key = ?1
               AND (?2 = '' OR c.state = ?2)
               AND (?3 = '' OR c.from_snapshot_fingerprint = ?3
                            OR c.to_snapshot_fingerprint = ?3)
               AND (?4 = '' OR c.ambiguity_group_id = ?4)
             ORDER BY c.updated_at_unix_seconds DESC, c.candidate_id
             LIMIT ?5",
        )?;
        statement
            .query_map(
                params![project_key, state, snapshot, ambiguity, limit],
                decode_lineage_candidate_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// 查询 group-first lineage 摘要；不会展开潜在 pair。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或持久化字段无法解码时返回错误。
    pub fn list_lineage_groups(
        &self,
        project_key: &str,
        limit: u32,
    ) -> Result<Vec<LineageGroupRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT group_id, project_key, provider_profile_id, provider_contract_id, language_id,
                    from_snapshot_fingerprint, to_snapshot_fingerprint, symbol_kind,
                    definition_fingerprint, algorithm_id, algorithm_version, from_count, to_count,
                    potential_pair_count, review_class, storage_mode, from_members_hash,
                    to_members_hash, created_at_unix_seconds
             FROM semantic_lineage_groups WHERE project_key = ?1
             ORDER BY created_at_unix_seconds DESC, group_id LIMIT ?2",
        )?;
        statement
            .query_map(params![project_key, limit], decode_lineage_group_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// 读取一个 lineage group 的成员集合；summary-only 超大组只返回空成员与已存摘要。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或持久化字段无法解码时返回错误。
    pub fn lineage_group(
        &self,
        project_key: &str,
        group_id: &str,
    ) -> Result<Option<LineageGroupDetail>, StoreError> {
        let group = self
            .connection
            .query_row(
                "SELECT group_id, project_key, provider_profile_id, provider_contract_id, language_id,
                        from_snapshot_fingerprint, to_snapshot_fingerprint, symbol_kind,
                        definition_fingerprint, algorithm_id, algorithm_version, from_count, to_count,
                        potential_pair_count, review_class, storage_mode, from_members_hash,
                        to_members_hash, created_at_unix_seconds
                 FROM semantic_lineage_groups WHERE project_key = ?1 AND group_id = ?2",
                params![project_key, group_id],
                decode_lineage_group_row,
            )
            .optional()?;
        let Some(group) = group else {
            return Ok(None);
        };
        let members = |side: &str| -> Result<Vec<String>, StoreError> {
            self.connection
                .prepare(
                    "SELECT symbol_id FROM semantic_lineage_group_members
                     WHERE group_id = ?1 AND side = ?2 ORDER BY symbol_id",
                )?
                .query_map(params![group_id, side], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::from)
        };
        Ok(Some(LineageGroupDetail {
            group,
            from_members: members("from")?,
            to_members: members("to")?,
        }))
    }

    /// 只读分析 V7 遗留的 pair-first 歧义候选；不会写表或删除记录。
    ///
    /// 只有完整笛卡尔积、仍为 proposed、仅含 V7 生成证据且从未被人工裁决引用的
    /// group 才会列为可压缩。其余候选全部保留。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询、计数校验或审计摘要生成失败时返回错误。
    pub fn preview_legacy_lineage_compaction(
        &self,
        project_key: &str,
    ) -> Result<LegacyLineageCompactionReport, StoreError> {
        Ok(
            build_legacy_lineage_compaction_plan(&self.connection, project_key, "dry_run", None)?
                .report,
        )
    }

    /// 把可证明为完整笛卡尔积的 V7 歧义 pair 压缩为不可变 group 摘要。
    ///
    /// 操作以 `request_id` 幂等；同一请求重放返回原审计报告。删除仅覆盖本次计划
    /// 中的 proposed 候选及其唯一 V7 证据，不执行 `VACUUM`。
    ///
    /// # Errors
    ///
    /// 请求冲突、历史记录不满足压缩前提或事务失败时返回错误。
    #[allow(
        clippy::too_many_lines,
        reason = "单个事务按审计先行顺序持久化 group、run、manifest 和集合删除，拆分会隐藏原子边界"
    )]
    pub fn apply_legacy_lineage_compaction(
        &self,
        project_key: &str,
        request_id: &str,
    ) -> Result<LegacyLineageCompactionReport, StoreError> {
        let request_id = request_id.trim();
        if request_id.is_empty() || request_id.len() > 192 {
            return Err(StoreError::InvalidLineage(
                "legacy compaction request_id 为空或过长".to_owned(),
            ));
        }
        let request_hash = legacy_compaction_request_hash(project_key);
        let transaction = self.connection.unchecked_transaction()?;
        let replay = transaction
            .query_row(
                "SELECT request_hash, report_json FROM semantic_lineage_compaction_runs
                 WHERE project_key = ?1 AND request_id = ?2",
                params![project_key, request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((stored_hash, report_json)) = replay {
            if stored_hash != request_hash {
                return Err(StoreError::LineageIdempotencyConflict(
                    request_id.to_owned(),
                ));
            }
            let mut report: LegacyLineageCompactionReport = serde_json::from_str(&report_json)?;
            report.replayed = true;
            return Ok(report);
        }

        let mut plan = build_legacy_lineage_compaction_plan(
            &transaction,
            project_key,
            "apply",
            Some(request_id),
        )?;
        plan.report.applied = true;
        let now = unix_seconds()?;
        let proposals = plan
            .groups
            .iter()
            .map(|group| {
                let language =
                    SourceLanguage::parse(&group.report.language_id).ok_or_else(|| {
                        StoreError::InvalidLineage(format!(
                            "legacy group language_id 无效：{:?}",
                            group.report.language_id
                        ))
                    })?;
                let oversized = group.report.storage_mode == "summary_only";
                Ok(LineageGroupProposal {
                    group_id: group.report.group_id.clone(),
                    project_key: project_key.to_owned(),
                    provider_profile_id: group.report.provider_profile_id.clone(),
                    provider_contract_id: group.report.provider_contract_id.clone(),
                    language,
                    from_snapshot: group.report.from_snapshot_fingerprint.clone(),
                    to_snapshot: group.report.to_snapshot_fingerprint.clone(),
                    symbol_kind: group.report.symbol_kind.clone(),
                    definition_fingerprint: group.report.definition_fingerprint.clone(),
                    algorithm_id: LEGACY_COMPACTION_ALGORITHM_ID.to_owned(),
                    algorithm_version: LEGACY_COMPACTION_ALGORITHM_VERSION.to_owned(),
                    from_count: group.report.from_count,
                    to_count: group.report.to_count,
                    potential_pair_count: group.report.potential_pair_count,
                    review_class: if oversized {
                        LineageGroupReviewClass::Oversized
                    } else {
                        LineageGroupReviewClass::Ambiguous
                    },
                    storage_mode: if oversized {
                        LineageGroupStorageMode::SummaryOnly
                    } else {
                        LineageGroupStorageMode::Members
                    },
                    from_members_hash: group.report.from_members_hash.clone(),
                    to_members_hash: group.report.to_members_hash.clone(),
                    from_members: if oversized {
                        Vec::new()
                    } else {
                        group.from_members.clone()
                    },
                    to_members: if oversized {
                        Vec::new()
                    } else {
                        group.to_members.clone()
                    },
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        persist_lineage_groups(&transaction, &proposals, now)?;

        let report_json = serde_json::to_string(&plan.report)?;
        transaction.execute(
            "INSERT INTO semantic_lineage_compaction_runs(
                 project_key, request_id, request_hash, operation_version,
                 compacted_group_count, compacted_candidate_count,
                 compacted_evidence_count, protected_candidate_count,
                 compaction_manifest_hash, report_json, created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                project_key,
                request_id,
                request_hash,
                i64::from(plan.report.operation_version),
                i64::try_from(plan.report.compactable_group_count).map_err(|_| {
                    StoreError::InvalidLineage("compaction group count 溢出".to_owned())
                })?,
                i64::try_from(plan.report.compactable_candidate_count).map_err(|_| {
                    StoreError::InvalidLineage("compaction candidate count 溢出".to_owned())
                })?,
                i64::try_from(plan.report.compactable_evidence_count).map_err(|_| {
                    StoreError::InvalidLineage("compaction evidence count 溢出".to_owned())
                })?,
                i64::try_from(plan.report.protected_candidate_count).map_err(|_| {
                    StoreError::InvalidLineage("protected candidate count 溢出".to_owned())
                })?,
                plan.report.compaction_manifest_hash,
                report_json,
                now,
            ],
        )?;
        let run_id: String = transaction.query_row(
            "SELECT run_id FROM semantic_lineage_compaction_runs
             WHERE project_key = ?1 AND request_id = ?2",
            params![project_key, request_id],
            |row| row.get(0),
        )?;
        for group in &plan.groups {
            transaction.execute(
                "INSERT INTO semantic_lineage_compaction_groups(
                     run_id, group_id, legacy_ambiguity_group_id,
                     candidate_count, evidence_count, candidate_manifest_hash,
                     evidence_manifest_hash
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id,
                    group.report.group_id,
                    group.report.legacy_ambiguity_group_id,
                    i64::try_from(group.report.candidate_count).unwrap_or(i64::MAX),
                    i64::try_from(group.report.evidence_count).unwrap_or(i64::MAX),
                    group.report.candidate_manifest_hash,
                    group.report.evidence_manifest_hash,
                ],
            )?;
        }

        transaction.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS project_brain_compaction_candidates(
                 candidate_id TEXT PRIMARY KEY
             );
             DELETE FROM project_brain_compaction_candidates;",
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO project_brain_compaction_candidates(candidate_id) VALUES (?1)",
            )?;
            for candidate_id in plan
                .groups
                .iter()
                .flat_map(|group| group.candidate_ids.iter())
            {
                insert.execute([candidate_id])?;
            }
        }
        let deleted_evidence = transaction.execute(
            "DELETE FROM semantic_lineage_evidence
             WHERE candidate_id IN (SELECT candidate_id FROM project_brain_compaction_candidates)",
            [],
        )?;
        let deleted_candidates = transaction.execute(
            "DELETE FROM semantic_lineage_candidates
             WHERE candidate_id IN (SELECT candidate_id FROM project_brain_compaction_candidates)",
            [],
        )?;
        if u64::try_from(deleted_evidence).unwrap_or(u64::MAX)
            != plan.report.compactable_evidence_count
            || u64::try_from(deleted_candidates).unwrap_or(u64::MAX)
                != plan.report.compactable_candidate_count
        {
            return Err(StoreError::Integrity(
                "legacy compaction 实际删除计数与预演不一致".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(plan.report)
    }

    /// 从一个已持久化的非超大 ambiguity group 中显式物化单个 proposed pair。
    ///
    /// 该操作只创建可审计候选，不会自动确认。重复物化同一 pair 幂等返回现有候选。
    ///
    /// # Errors
    ///
    /// group/member 边界无效、group 为 summary-only、或事务写入失败时返回错误。
    #[allow(
        clippy::too_many_lines,
        reason = "显式 pair 物化在一个线性流程中验证 group、成员、证据与幂等回读"
    )]
    pub fn materialize_lineage_group_pair(
        &self,
        project_key: &str,
        group_id: &str,
        from_symbol_id: &str,
        to_symbol_id: &str,
    ) -> Result<LineageCandidateRecord, StoreError> {
        let group = self
            .connection
            .query_row(
                "SELECT project_key, provider_profile_id, provider_contract_id, language_id,
                    from_snapshot_fingerprint, to_snapshot_fingerprint, symbol_kind,
                    definition_fingerprint, algorithm_id, algorithm_version, storage_mode
             FROM semantic_lineage_groups WHERE group_id = ?1",
                [group_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::InvalidLineage(format!("lineage group 不存在：{group_id}"))
            })?;
        if group.0 != project_key {
            return Err(StoreError::LineageConflict(
                "lineage group 不属于当前项目".to_owned(),
            ));
        }
        if group.10 != "members" {
            return Err(StoreError::InvalidLineage(
                "summary_only 超大 group 必须先从 immutable snapshots 重新验证成员".to_owned(),
            ));
        }
        if group.8 == LEGACY_COMPACTION_ALGORITHM_ID {
            return Err(StoreError::InvalidLineage(
                "legacy compacted group 只保留历史审计，不得重新物化候选".to_owned(),
            ));
        }
        for (side, symbol_id) in [("from", from_symbol_id), ("to", to_symbol_id)] {
            let exists: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM semantic_lineage_group_members
                 WHERE group_id = ?1 AND side = ?2 AND symbol_id = ?3)",
                params![group_id, side, symbol_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StoreError::InvalidLineage(format!(
                    "symbol={symbol_id} 不是 group={group_id} 的 {side} member"
                )));
            }
        }
        let language = SourceLanguage::parse(&group.3).ok_or_else(|| {
            StoreError::InvalidLineage(format!("group language_id 无效：{:?}", group.3))
        })?;
        let evidence = vec![
            LineageEvidence::KindEqual,
            LineageEvidence::NormalizedDefinitionEqual,
        ];
        let evidence_json = serde_json::to_vec(&evidence)?;
        let input_fingerprint = format!(
            "sha256_{:x}",
            Sha256::digest(format!(
                "{}\0{}\0{}\0{}",
                group_id, group.4, from_symbol_id, to_symbol_id
            ))
        );
        let proposal = LineageCandidateProposal {
            project_key: group.0,
            provider_profile_id: group.1,
            provider_contract_id: group.2,
            language,
            from_snapshot: group.4.clone(),
            from_symbol: from_symbol_id.to_owned(),
            to_snapshot: group.5.clone(),
            to_symbol: to_symbol_id.to_owned(),
            ambiguity_group_id: None,
            origin_group_id: Some(group_id.to_owned()),
            proposal_origin: "human_group_pair".to_owned(),
            algorithm_id: group.8,
            algorithm_version: group.9,
            confidence: LineageConfidence::Low,
            input_fingerprint,
            evidence_fingerprint: format!("sha256_{:x}", Sha256::digest(&evidence_json)),
            evidence,
        };
        let transaction = self.connection.unchecked_transaction()?;
        persist_lineage_proposals(&transaction, &[proposal], unix_seconds()?)?;
        transaction.commit()?;
        self.list_lineage_candidates(project_key, None, None, None, u32::MAX)?
            .into_iter()
            .find(|candidate| {
                candidate.from_snapshot_fingerprint == group.4
                    && candidate.from_symbol_id == from_symbol_id
                    && candidate.to_snapshot_fingerprint == group.5
                    && candidate.to_symbol_id == to_symbol_id
            })
            .ok_or_else(|| StoreError::Integrity("已物化 lineage candidate 无法回读".to_owned()))
    }

    /// 验证 semantic 锚点，并只沿相邻快照中的直接身份或 confirmed lineage 解析到最新符号。
    /// proposed/ambiguous/local/syntax fallback 都不会获得硬证据资格。
    ///
    /// # Errors
    ///
    /// 当 scope 边界非法、数据库字段损坏或 `SQLite` 查询失败时返回错误。
    #[allow(
        clippy::too_many_lines,
        reason = "解析器在一个线性流程中显式保留锚点、每一跳 lineage 与最终定义的拒绝原因"
    )]
    pub fn resolve_semantic_scope(
        &self,
        project_key: &str,
        provider_profile_id: &str,
        provider_contract_id: &str,
        language_id: &str,
        anchor_snapshot: &str,
        anchor_symbol: &str,
    ) -> Result<SemanticScopeResolution, StoreError> {
        let boundary = SemanticScopeBoundary {
            project_key,
            provider_profile_id,
            provider_contract_id,
            language_id,
        };
        validate_scope_boundary(&boundary, anchor_snapshot, anchor_symbol)?;
        let snapshots = semantic_snapshot_chain(&self.connection, &boundary, anchor_snapshot)?;
        let Some(first) = snapshots.first() else {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                None,
                "锚点 snapshot 不存在或不属于指定 provider/language 边界",
            ));
        };
        let Some(anchor) = semantic_observation(
            &self.connection,
            &boundary,
            &first.fingerprint,
            anchor_symbol,
        )?
        else {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                snapshots.last(),
                "锚点 symbol 不存在于指定 semantic snapshot",
            ));
        };
        if anchor.is_local || anchor.provider_symbol.as_deref().is_none_or(str::is_empty) {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                snapshots.last(),
                "local 或缺少 provider symbol 的观察不得成为硬规则锚点",
            ));
        }
        if provider_symbol_is_ambiguous(&self.connection, &boundary, &first.fingerprint, &anchor)? {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                snapshots.last(),
                "provider symbol 在锚点 snapshot 中不唯一",
            ));
        }

        let mut current_symbol = anchor_symbol.to_owned();
        let mut decisions = Vec::new();
        for pair in snapshots.windows(2) {
            let from = &pair[0];
            let to = &pair[1];
            if semantic_observation(
                &self.connection,
                &boundary,
                &to.fingerprint,
                &current_symbol,
            )?
            .is_some()
            {
                continue;
            }
            let confirmed = confirmed_lineage_hop(
                &self.connection,
                &boundary,
                &from.fingerprint,
                &current_symbol,
                &to.fingerprint,
            )?;
            let Some((next_symbol, decision_id)) = confirmed else {
                return Ok(unresolved_scope(
                    anchor_snapshot,
                    anchor_symbol,
                    Some(to),
                    "相邻 semantic snapshot 间缺少唯一 confirmed lineage",
                ));
            };
            current_symbol = next_symbol;
            decisions.push(decision_id);
        }
        let Some(latest) = snapshots.last() else {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                None,
                "semantic snapshot chain 意外为空",
            ));
        };
        let Some(symbol) = current_semantic_symbol(
            &self.connection,
            &boundary,
            &latest.fingerprint,
            &current_symbol,
        )?
        else {
            return Ok(unresolved_scope(
                anchor_snapshot,
                anchor_symbol,
                Some(latest),
                "最新 snapshot 中解析目标缺少有效 semantic definition",
            ));
        };
        Ok(SemanticScopeResolution {
            kind: if decisions.is_empty() {
                SemanticResolutionKind::DirectSemantic
            } else {
                SemanticResolutionKind::ConfirmedLineage
            },
            anchor_snapshot_fingerprint: anchor_snapshot.to_owned(),
            anchor_symbol_id: anchor_symbol.to_owned(),
            latest_snapshot_fingerprint: Some(latest.fingerprint.clone()),
            resolved_symbol: Some(symbol),
            source: Some(latest.source.clone()),
            lineage_decision_ids: decisions,
            reason: None,
        })
    }

    /// 通过显式用户裁决确认候选；可在同一事务中 supersede 一条旧确认。
    ///
    /// # Errors
    ///
    /// 候选不存在、状态/边界冲突、request ID 冲突或 CAS 失败时返回错误。
    pub fn confirm_lineage(
        &self,
        project_key: &str,
        candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
        supersede_candidate_id: Option<&str>,
    ) -> Result<LineageAdjudicationResult, StoreError> {
        if let Some(old_candidate_id) = supersede_candidate_id {
            return self.confirm_and_supersede_lineage(
                project_key,
                candidate_id,
                old_candidate_id,
                request_id,
                actor_ref,
                reason,
            );
        }
        self.adjudicate_single_lineage(
            project_key,
            candidate_id,
            request_id,
            LineageDecisionAction::Confirm,
            actor_ref,
            reason,
        )
    }

    /// 通过显式用户裁决拒绝一条尚未裁决的候选。
    ///
    /// # Errors
    ///
    /// 候选不存在、状态冲突、request ID 冲突或 CAS 失败时返回错误。
    pub fn reject_lineage(
        &self,
        project_key: &str,
        candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
    ) -> Result<LineageAdjudicationResult, StoreError> {
        self.adjudicate_single_lineage(
            project_key,
            candidate_id,
            request_id,
            LineageDecisionAction::Reject,
            actor_ref,
            reason,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn adjudicate_single_lineage(
        &self,
        project_key: &str,
        candidate_id: &str,
        request_id: &str,
        action: LineageDecisionAction,
        actor_ref: Option<&str>,
        reason: Option<&str>,
    ) -> Result<LineageAdjudicationResult, StoreError> {
        validate_lineage_command(project_key, candidate_id, request_id, actor_ref, reason)?;
        let request_hash = lineage_request_hash(
            project_key,
            candidate_id,
            request_id,
            action,
            None,
            actor_ref,
            reason,
        );
        let now = unix_seconds()?;
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(replayed) =
            replay_lineage_decision(&transaction, project_key, request_id, &request_hash)?
        {
            transaction.commit()?;
            return Ok(replayed);
        }
        let before = lineage_candidate_by_id(&transaction, project_key, candidate_id)?;
        let to_state = match (action, before.state) {
            (LineageDecisionAction::Confirm, LineageState::Proposed | LineageState::Rejected) => {
                LineageState::Confirmed
            }
            (LineageDecisionAction::Reject, LineageState::Proposed) => LineageState::Rejected,
            _ => {
                return Err(StoreError::LineageConflict(format!(
                    "不允许 action={} 从 state={} 转移 candidate={candidate_id}",
                    action.as_str(),
                    before.state.as_str()
                )));
            }
        };
        let decision = insert_lineage_decision(
            &transaction,
            project_key,
            request_id,
            &request_hash,
            candidate_id,
            action,
            before.state,
            to_state,
            None,
            actor_ref,
            reason,
            now,
        )?;
        cas_lineage_state(&transaction, &before, to_state, now)?;
        let candidate = lineage_candidate_by_id(&transaction, project_key, candidate_id)?;
        transaction.commit()?;
        Ok(LineageAdjudicationResult {
            decision,
            candidate,
            superseded_candidate: None,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm_and_supersede_lineage(
        &self,
        project_key: &str,
        new_candidate_id: &str,
        old_candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
    ) -> Result<LineageAdjudicationResult, StoreError> {
        validate_lineage_command(project_key, new_candidate_id, request_id, actor_ref, reason)?;
        if new_candidate_id == old_candidate_id {
            return Err(StoreError::LineageConflict(
                "不能用候选 supersede 自身".to_owned(),
            ));
        }
        let action = LineageDecisionAction::Supersede;
        let request_hash = lineage_request_hash(
            project_key,
            new_candidate_id,
            request_id,
            action,
            Some(old_candidate_id),
            actor_ref,
            reason,
        );
        let now = unix_seconds()?;
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(replayed) =
            replay_lineage_decision(&transaction, project_key, request_id, &request_hash)?
        {
            transaction.commit()?;
            return Ok(replayed);
        }
        let new_before = lineage_candidate_by_id(&transaction, project_key, new_candidate_id)?;
        let old_before = lineage_candidate_by_id(&transaction, project_key, old_candidate_id)?;
        if old_before.state != LineageState::Confirmed
            || !matches!(
                new_before.state,
                LineageState::Proposed | LineageState::Rejected
            )
            || !same_lineage_boundary(&old_before, &new_before)
        {
            return Err(StoreError::LineageConflict(
                "supersede 要求旧候选已 confirmed、新候选为 proposed/rejected，且 snapshot/provider/language 边界一致"
                    .to_owned(),
            ));
        }
        let decision = insert_lineage_decision(
            &transaction,
            project_key,
            request_id,
            &request_hash,
            old_candidate_id,
            action,
            LineageState::Confirmed,
            LineageState::Superseded,
            Some(new_candidate_id),
            actor_ref,
            reason,
            now,
        )?;
        cas_lineage_state(&transaction, &old_before, LineageState::Superseded, now)?;
        cas_lineage_state(&transaction, &new_before, LineageState::Confirmed, now)?;
        let candidate = lineage_candidate_by_id(&transaction, project_key, new_candidate_id)?;
        let superseded_candidate = Some(lineage_candidate_by_id(
            &transaction,
            project_key,
            old_candidate_id,
        )?);
        transaction.commit()?;
        Ok(LineageAdjudicationResult {
            decision,
            candidate,
            superseded_candidate,
            replayed: false,
        })
    }

    /// 查询当前或历史符号。路径过滤使用项目相对路径边界，而不是任意字符串前缀。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或持久化枚举字段无效时返回错误。
    pub fn list_symbols(
        &self,
        project_key: &str,
        path: Option<&str>,
        include_removed: bool,
        limit: u32,
    ) -> Result<Vec<SymbolNode>, StoreError> {
        let path = path.unwrap_or_default();
        let path_pattern = escape_like_pattern(path);
        let mut statement = self.connection.prepare(
            "SELECT project_key, id, provider_id, identity_quality, language, kind, provider_key,
                    display_name, path, start_line, end_line, content_fingerprint, status
             FROM symbol_nodes
             WHERE project_key = ?1
               AND (?2 = 1 OR status = 'active')
               AND (?3 = '' OR path = ?3 OR path LIKE ?4 || '/%' ESCAPE '!')
             ORDER BY path, start_line, end_line, id
             LIMIT ?5",
        )?;
        statement
            .query_map(
                params![project_key, include_removed, path, path_pattern, limit],
                decode_symbol_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// 返回当前数据库 schema 版本，主要用于迁移验证和诊断。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或 metadata 值无效时返回错误。
    pub fn database_schema_version(&self) -> Result<i64, StoreError> {
        let value: String = self.connection.query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        value
            .parse()
            .map_err(|_| StoreError::Integrity("schema_version 不是整数".to_owned()))
    }

    /// 追加一次显式 Provider 稳定性资格结论；不会改写旧结论。
    ///
    /// # Errors
    ///
    /// 资格字段无效或 `SQLite` 写入失败时返回错误。
    #[allow(clippy::too_many_arguments)]
    pub fn record_provider_qualification(
        &self,
        project_key: &str,
        provider_profile_id: &str,
        status: &str,
        runs: u64,
        registration_id: &str,
        registration_revision: u64,
        executable_sha256: &str,
        source_fingerprint: &str,
        evidence_manifest_hash: &str,
    ) -> Result<ProviderQualificationRecord, StoreError> {
        if !is_valid_project_key(project_key)
            || provider_profile_id.trim().is_empty()
            || provider_profile_id.len() > 64
            || !matches!(
                status,
                "stable_complete" | "stable_incomplete" | "nondeterministic"
            )
            || runs < 2
            || registration_id.trim().is_empty()
            || registration_id.len() > 192
            || registration_revision == 0
            || !is_raw_sha256(executable_sha256)
            || !is_raw_sha256(source_fingerprint)
            || !is_sha256_fingerprint(evidence_manifest_hash)
        {
            return Err(StoreError::InvalidProviderQualification(
                "project/profile/status/runs/binding/source/evidence 边界不完整".to_owned(),
            ));
        }
        let created_at = unix_seconds()?;
        self.connection.execute(
            "INSERT INTO semantic_provider_qualification_events(
                 project_key, provider_profile_id, status, runs, registration_id,
                 registration_revision, executable_sha256, source_fingerprint,
                 evidence_manifest_hash, created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                project_key,
                provider_profile_id,
                status,
                i64::try_from(runs).map_err(|_| StoreError::InvalidProviderQualification(
                    "runs 超出 SQLite INTEGER".to_owned()
                ))?,
                registration_id,
                i64::try_from(registration_revision).map_err(|_| {
                    StoreError::InvalidProviderQualification(
                        "registration_revision 超出 SQLite INTEGER".to_owned(),
                    )
                })?,
                executable_sha256,
                source_fingerprint,
                evidence_manifest_hash,
                created_at,
            ],
        )?;
        self.latest_provider_qualification(project_key, provider_profile_id)?
            .ok_or_else(|| StoreError::Integrity("刚写入的 Provider 资格状态无法回读".to_owned()))
    }

    /// 返回项目/profile 最新的 append-only Provider 稳定性资格结论。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询或整数转换失败时返回错误。
    pub fn latest_provider_qualification(
        &self,
        project_key: &str,
        provider_profile_id: &str,
    ) -> Result<Option<ProviderQualificationRecord>, StoreError> {
        self.connection
            .query_row(
                "SELECT sequence, project_key, provider_profile_id, status, runs,
                        registration_id, registration_revision, executable_sha256,
                        source_fingerprint, evidence_manifest_hash, created_at_unix_seconds
                 FROM semantic_provider_qualification_events
                 WHERE project_key = ?1 AND provider_profile_id = ?2
                 ORDER BY sequence DESC LIMIT 1",
                params![project_key, provider_profile_id],
                |row| {
                    let sequence = row.get::<_, i64>(0)?;
                    let runs = row.get::<_, i64>(4)?;
                    let revision = row.get::<_, i64>(6)?;
                    Ok(ProviderQualificationRecord {
                        sequence: u64::try_from(sequence).map_err(|_| {
                            invalid_provider_qualification_sql(0, format!("sequence={sequence}"))
                        })?,
                        project_key: row.get(1)?,
                        provider_profile_id: row.get(2)?,
                        status: row.get(3)?,
                        runs: u64::try_from(runs).map_err(|_| {
                            invalid_provider_qualification_sql(4, format!("runs={runs}"))
                        })?,
                        registration_id: row.get(5)?,
                        registration_revision: u64::try_from(revision).map_err(|_| {
                            invalid_provider_qualification_sql(
                                6,
                                format!("registration_revision={revision}"),
                            )
                        })?,
                        executable_sha256: row.get(7)?,
                        source_fingerprint: row.get(8)?,
                        evidence_manifest_hash: row.get(9)?,
                        created_at_unix_seconds: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// 以不可变 JSON 快照记录一次动作和决策。
    ///
    /// # Errors
    ///
    /// 当时钟、JSON 序列化或 `SQLite` 写入失败时返回错误。
    pub fn record(
        &self,
        hook_event: &str,
        action: &ActionDescriptor,
        decision: &Decision,
    ) -> Result<i64, StoreError> {
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let created_at_unix_seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
        let action_json = serde_json::to_string(action)?;
        let decision_json = serde_json::to_string(decision)?;

        self.connection.execute(
            "INSERT INTO audit_events(
                 event_id, session_id, hook_event, action_json, decision_json,
                 created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                action.event_id,
                action.session_id,
                hook_event,
                action_json,
                decision_json,
                created_at_unix_seconds
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// 返回当前数据库中的审计事件数。
    ///
    /// # Errors
    ///
    /// 当 `SQLite` 查询失败时返回错误。
    pub fn audit_count(&self) -> Result<u64, StoreError> {
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// 按时间倒序返回最近的审计事件。
    ///
    /// # Errors
    ///
    /// 当 `SQLite` 查询或行解码失败时返回错误。
    pub fn recent_audit(&self, limit: u32) -> Result<Vec<AuditRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, event_id, session_id, hook_event, action_json, decision_json,
                    created_at_unix_seconds
             FROM audit_events
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let records = statement
            .query_map([limit], |row| {
                Ok(AuditRecord {
                    id: row.get(0)?,
                    event_id: row.get(1)?,
                    session_id: row.get(2)?,
                    hook_event: row.get(3)?,
                    action_json: row.get(4)?,
                    decision_json: row.get(5)?,
                    created_at_unix_seconds: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    /// 原子记录内部 Hook 事件与结果；同一项目、适配器和 `event_id` 重放时返回首次结果。
    ///
    /// # Errors
    ///
    /// 事件无效，或 JSON/SQLite 操作失败时返回错误。
    pub fn record_adapter_event(
        &self,
        event: &InternalHookEvent,
        outcome: &InternalHookOutcome,
        latency_ms: u64,
    ) -> Result<AdapterRecordResult, StoreError> {
        event
            .validate()
            .map_err(|error| StoreError::InvalidHookEvent(error.to_string()))?;
        if outcome.protocol_version != HOOK_PROTOCOL_VERSION || outcome.event_id != event.event_id {
            return Err(StoreError::InvalidHookEvent(
                "outcome 与 event 的协议版本或 event_id 不一致".to_owned(),
            ));
        }
        let event_json = serde_json::to_string(event)?;
        let outcome_json = serde_json::to_string(outcome)?;
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let created_at_unix_seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
        let latency_ms = i64::try_from(latency_ms).unwrap_or(i64::MAX);
        let existing = self
            .connection
            .query_row(
                "SELECT outcome_json FROM adapter_audit_events
                 WHERE project_key = ?1 AND adapter_kind = ?2 AND event_id = ?3",
                params![
                    event.project_key,
                    event.adapter.kind.as_str(),
                    event.event_id
                ],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if let Some(Some(existing)) = existing {
            return Ok(AdapterRecordResult::Duplicate(serde_json::from_str(
                &existing,
            )?));
        }
        let changed = self.connection.execute(
            "INSERT INTO adapter_audit_events(
                 project_key, adapter_kind, adapter_version, event_id, session_key,
                 event_kind, event_json, outcome_json, latency_ms, failure,
                 created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)
             ON CONFLICT(project_key, adapter_kind, event_id) DO UPDATE SET
                 event_json = excluded.event_json,
                 outcome_json = excluded.outcome_json,
                 latency_ms = excluded.latency_ms,
                 failure = NULL,
                 created_at_unix_seconds = excluded.created_at_unix_seconds
             WHERE adapter_audit_events.outcome_json IS NULL",
            params![
                event.project_key,
                event.adapter.kind.as_str(),
                i64::from(event.adapter.adapter_version),
                event.event_id,
                event.session_key,
                event.kind().as_str(),
                event_json,
                outcome_json,
                latency_ms,
                created_at_unix_seconds,
            ],
        )?;
        if changed == 1 {
            let id = self.connection.query_row(
                "SELECT id FROM adapter_audit_events
                 WHERE project_key = ?1 AND adapter_kind = ?2 AND event_id = ?3",
                params![
                    event.project_key,
                    event.adapter.kind.as_str(),
                    event.event_id
                ],
                |row| row.get(0),
            )?;
            return Ok(AdapterRecordResult::Inserted(id));
        }
        let existing: String = self.connection.query_row(
            "SELECT outcome_json FROM adapter_audit_events
             WHERE project_key = ?1 AND adapter_kind = ?2 AND event_id = ?3",
            params![
                event.project_key,
                event.adapter.kind.as_str(),
                event.event_id
            ],
            |row| row.get(0),
        )?;
        Ok(AdapterRecordResult::Duplicate(serde_json::from_str(
            &existing,
        )?))
    }

    /// 记录已规范化但处理失败的适配器事件，供诊断和后续重放。
    ///
    /// # Errors
    ///
    /// 事件无效，或 JSON/SQLite 操作失败时返回错误。
    pub fn record_adapter_failure(
        &self,
        event: &InternalHookEvent,
        latency_ms: u64,
        failure: &str,
    ) -> Result<(), StoreError> {
        event
            .validate()
            .map_err(|error| StoreError::InvalidHookEvent(error.to_string()))?;
        let event_json = serde_json::to_string(event)?;
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let created_at_unix_seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
        self.connection.execute(
            "INSERT INTO adapter_audit_events(
                 project_key, adapter_kind, adapter_version, event_id, session_key,
                 event_kind, event_json, outcome_json, latency_ms, failure,
                 created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10)
             ON CONFLICT(project_key, adapter_kind, event_id) DO UPDATE SET
                 latency_ms = excluded.latency_ms,
                 failure = excluded.failure,
                 created_at_unix_seconds = excluded.created_at_unix_seconds
             WHERE adapter_audit_events.outcome_json IS NULL",
            params![
                event.project_key,
                event.adapter.kind.as_str(),
                i64::from(event.adapter.adapter_version),
                event.event_id,
                event.session_key,
                event.kind().as_str(),
                event_json,
                i64::try_from(latency_ms).unwrap_or(i64::MAX),
                failure,
                created_at_unix_seconds,
            ],
        )?;
        Ok(())
    }

    /// 按项目返回最近的适配器审计记录。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询或字段转换失败时返回错误。
    pub fn recent_adapter_audit(
        &self,
        project_key: &str,
        limit: u32,
    ) -> Result<Vec<AdapterAuditRecord>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id, project_key, adapter_kind, adapter_version, event_id, session_key,
                    event_kind, event_json, outcome_json, latency_ms, failure,
                    created_at_unix_seconds
             FROM adapter_audit_events
             WHERE project_key = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        statement
            .query_map(params![project_key, limit], |row| {
                let adapter_version: i64 = row.get(3)?;
                let latency_ms: i64 = row.get(9)?;
                Ok(AdapterAuditRecord {
                    id: row.get(0)?,
                    project_key: row.get(1)?,
                    adapter_kind: row.get(2)?,
                    adapter_version: u16::try_from(adapter_version).unwrap_or_default(),
                    event_id: row.get(4)?,
                    session_key: row.get(5)?,
                    event_kind: row.get(6)?,
                    event_json: row.get(7)?,
                    outcome_json: row.get(8)?,
                    latency_ms: u64::try_from(latency_ms).unwrap_or_default(),
                    failure: row.get(10)?,
                    created_at_unix_seconds: row.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

type LegacyCompactionKey = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

struct LegacyCompactionAccumulator {
    all_rows_eligible: bool,
    from_members: std::collections::BTreeSet<String>,
    to_members: std::collections::BTreeSet<String>,
    pairs: std::collections::BTreeSet<(String, String)>,
    candidate_ids: Vec<String>,
    evidence_tokens: Vec<String>,
}

struct LegacyCompactionPlanGroup {
    report: LegacyLineageCompactionGroup,
    from_members: Vec<String>,
    to_members: Vec<String>,
    candidate_ids: Vec<String>,
}

struct LegacyCompactionPlan {
    report: LegacyLineageCompactionReport,
    groups: Vec<LegacyCompactionPlanGroup>,
}

#[allow(
    clippy::too_many_lines,
    reason = "压缩预演必须在同一只读快照中完成候选资格、完整笛卡尔积和摘要校验"
)]
fn build_legacy_lineage_compaction_plan(
    connection: &Connection,
    project_key: &str,
    mode: &str,
    request_id: Option<&str>,
) -> Result<LegacyCompactionPlan, StoreError> {
    let total_raw: i64 = connection.query_row(
        "SELECT COUNT(*) FROM semantic_lineage_candidates
         WHERE project_key = ?1 AND proposal_origin = 'legacy_v7'
           AND ambiguity_group_id IS NOT NULL",
        [project_key],
        |row| row.get(0),
    )?;
    let total = u64::try_from(total_raw)
        .map_err(|_| StoreError::Integrity("legacy ambiguous candidate count 为负数".to_owned()))?;
    let mut statement = connection.prepare(
        "SELECT c.candidate_id, c.provider_profile_id, c.provider_contract_id,
                c.language_id, c.from_snapshot_fingerprint, c.from_symbol_id,
                c.to_snapshot_fingerprint, c.to_symbol_id, c.state,
                c.ambiguity_group_id,
                old.kind, old.normalized_definition_fingerprint,
                new.kind, new.normalized_definition_fingerprint,
                COUNT(e.evidence_id), MIN(e.evidence_id),
                MIN(e.algorithm_id), MAX(e.algorithm_id),
                MIN(e.algorithm_version), MAX(e.algorithm_version),
                MIN(e.evidence_schema_version), MAX(e.evidence_schema_version),
                MIN(e.evidence_hash),
                (SELECT COUNT(*) FROM semantic_lineage_decisions d
                 WHERE d.candidate_id = c.candidate_id
                    OR d.related_candidate_id = c.candidate_id)
         FROM semantic_lineage_candidates c
         LEFT JOIN semantic_symbol_observations old
           ON old.project_key = c.project_key
          AND old.provider_profile_id = c.provider_profile_id
          AND old.provider_contract_id = c.provider_contract_id
          AND old.snapshot_fingerprint = c.from_snapshot_fingerprint
          AND old.symbol_id = c.from_symbol_id
         LEFT JOIN semantic_symbol_observations new
           ON new.project_key = c.project_key
          AND new.provider_profile_id = c.provider_profile_id
          AND new.provider_contract_id = c.provider_contract_id
          AND new.snapshot_fingerprint = c.to_snapshot_fingerprint
          AND new.symbol_id = c.to_symbol_id
         LEFT JOIN semantic_lineage_evidence e ON e.candidate_id = c.candidate_id
         WHERE c.project_key = ?1 AND c.proposal_origin = 'legacy_v7'
           AND c.ambiguity_group_id IS NOT NULL
         GROUP BY c.candidate_id
         ORDER BY c.provider_profile_id, c.provider_contract_id, c.language_id,
                  c.from_snapshot_fingerprint, c.to_snapshot_fingerprint,
                  c.ambiguity_group_id, c.from_symbol_id, c.to_symbol_id",
    )?;
    let rows = statement.query_map([project_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, i64>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, Option<String>>(18)?,
            row.get::<_, Option<String>>(19)?,
            row.get::<_, Option<i64>>(20)?,
            row.get::<_, Option<i64>>(21)?,
            row.get::<_, Option<String>>(22)?,
            row.get::<_, i64>(23)?,
        ))
    })?;
    let mut groups =
        std::collections::BTreeMap::<LegacyCompactionKey, LegacyCompactionAccumulator>::new();
    let mut unkeyed_protected = 0_u64;
    for row in rows {
        let (
            candidate_id,
            profile,
            contract,
            language,
            from_snapshot,
            from_symbol,
            to_snapshot,
            to_symbol,
            state,
            ambiguity,
            old_kind,
            old_fingerprint,
            new_kind,
            new_fingerprint,
            evidence_count,
            evidence_id,
            algorithm_min,
            algorithm_max,
            version_min,
            version_max,
            schema_min,
            schema_max,
            evidence_hash,
            decision_references,
        ) = row?;
        let compatible = old_kind.is_some()
            && old_kind == new_kind
            && old_fingerprint.is_some()
            && old_fingerprint == new_fingerprint;
        if !compatible {
            unkeyed_protected = unkeyed_protected.saturating_add(1);
            continue;
        }
        let kind = old_kind.unwrap_or_default();
        let fingerprint = old_fingerprint.unwrap_or_default();
        let key = (
            profile,
            contract,
            language.clone(),
            from_snapshot,
            to_snapshot,
            kind,
            fingerprint.clone(),
            ambiguity,
        );
        let row_eligible = state == "proposed"
            && SourceLanguage::parse(&language).is_some()
            && is_sha256_fingerprint(&fingerprint)
            && evidence_count == 1
            && algorithm_min.as_deref() == Some(LEGACY_LINEAGE_ALGORITHM_ID)
            && algorithm_max.as_deref() == Some(LEGACY_LINEAGE_ALGORITHM_ID)
            && version_min.as_deref() == Some(LEGACY_LINEAGE_ALGORITHM_VERSION)
            && version_max.as_deref() == Some(LEGACY_LINEAGE_ALGORITHM_VERSION)
            && schema_min == Some(i64::from(LINEAGE_EVIDENCE_SCHEMA_VERSION))
            && schema_max == Some(i64::from(LINEAGE_EVIDENCE_SCHEMA_VERSION))
            && evidence_id.is_some()
            && evidence_hash.is_some()
            && decision_references == 0;
        let group = groups
            .entry(key)
            .or_insert_with(|| LegacyCompactionAccumulator {
                all_rows_eligible: true,
                from_members: std::collections::BTreeSet::new(),
                to_members: std::collections::BTreeSet::new(),
                pairs: std::collections::BTreeSet::new(),
                candidate_ids: Vec::new(),
                evidence_tokens: Vec::new(),
            });
        group.all_rows_eligible &= row_eligible;
        group.from_members.insert(from_symbol.clone());
        group.to_members.insert(to_symbol.clone());
        group.pairs.insert((from_symbol, to_symbol));
        group.candidate_ids.push(candidate_id);
        if let (Some(id), Some(hash)) = (evidence_id, evidence_hash) {
            group.evidence_tokens.push(format!("{id}\0{hash}"));
        }
    }

    let mut protected = unkeyed_protected;
    let mut compactable = Vec::new();
    for (key, mut group) in groups {
        group.candidate_ids.sort();
        group.evidence_tokens.sort();
        let from_members = group.from_members.into_iter().collect::<Vec<_>>();
        let to_members = group.to_members.into_iter().collect::<Vec<_>>();
        let from_count = u64::try_from(from_members.len()).unwrap_or(u64::MAX);
        let to_count = u64::try_from(to_members.len()).unwrap_or(u64::MAX);
        let candidate_count = u64::try_from(group.candidate_ids.len()).unwrap_or(u64::MAX);
        let potential_pair_count = from_count.checked_mul(to_count).ok_or_else(|| {
            StoreError::InvalidLineage("legacy compaction potential pair 计数溢出".to_owned())
        })?;
        let complete_cartesian = group.all_rows_eligible
            && candidate_count > 1
            && u64::try_from(group.pairs.len()).unwrap_or(u64::MAX) == candidate_count
            && candidate_count == potential_pair_count
            && group.evidence_tokens.len() == group.candidate_ids.len();
        if !complete_cartesian {
            protected = protected.saturating_add(candidate_count);
            continue;
        }
        let oversized = from_members.len() > MAX_LINEAGE_GROUP_MEMBERS_PER_SIDE
            || to_members.len() > MAX_LINEAGE_GROUP_MEMBERS_PER_SIDE;
        let group_id = legacy_compaction_group_id(project_key, &key);
        compactable.push(LegacyCompactionPlanGroup {
            report: LegacyLineageCompactionGroup {
                group_id,
                legacy_ambiguity_group_id: key.7,
                provider_profile_id: key.0,
                provider_contract_id: key.1,
                language_id: key.2,
                from_snapshot_fingerprint: key.3,
                to_snapshot_fingerprint: key.4,
                symbol_kind: key.5,
                definition_fingerprint: key.6,
                from_count,
                to_count,
                potential_pair_count,
                candidate_count,
                evidence_count: candidate_count,
                storage_mode: if oversized {
                    "summary_only".to_owned()
                } else {
                    "members".to_owned()
                },
                from_members_hash: stable_string_set_hash(&from_members),
                to_members_hash: stable_string_set_hash(&to_members),
                candidate_manifest_hash: stable_string_set_hash(&group.candidate_ids),
                evidence_manifest_hash: stable_string_set_hash(&group.evidence_tokens),
            },
            from_members,
            to_members,
            candidate_ids: group.candidate_ids,
        });
    }
    compactable.sort_by(|left, right| left.report.group_id.cmp(&right.report.group_id));
    let compactable_candidate_count = compactable
        .iter()
        .try_fold(0_u64, |total, group| {
            total.checked_add(group.report.candidate_count)
        })
        .ok_or_else(|| StoreError::InvalidLineage("compactable candidate count 溢出".to_owned()))?;
    let compactable_evidence_count = compactable
        .iter()
        .try_fold(0_u64, |total, group| {
            total.checked_add(group.report.evidence_count)
        })
        .ok_or_else(|| StoreError::InvalidLineage("compactable evidence count 溢出".to_owned()))?;
    if protected.saturating_add(compactable_candidate_count) != total {
        return Err(StoreError::Integrity(
            "legacy compaction 候选分类计数不守恒".to_owned(),
        ));
    }
    let group_member_count = compactable.iter().try_fold(0_u64, |total, group| {
        if group.report.storage_mode == "summary_only" {
            Ok(total)
        } else {
            total
                .checked_add(group.report.from_count)
                .and_then(|value| value.checked_add(group.report.to_count))
                .ok_or_else(|| StoreError::InvalidLineage("group member count 溢出".to_owned()))
        }
    })?;
    let reports = compactable
        .iter()
        .map(|group| group.report.clone())
        .collect::<Vec<_>>();
    let manifest = serde_json::to_vec(&reports)?;
    Ok(LegacyCompactionPlan {
        report: LegacyLineageCompactionReport {
            project_key: project_key.to_owned(),
            operation_version: 1,
            mode: mode.to_owned(),
            applied: false,
            replayed: false,
            request_id: request_id.map(str::to_owned),
            legacy_ambiguous_candidate_count: total,
            compactable_group_count: u64::try_from(reports.len()).unwrap_or(u64::MAX),
            compactable_candidate_count,
            compactable_evidence_count,
            protected_candidate_count: protected,
            group_member_count,
            oversized_group_count: u64::try_from(
                reports
                    .iter()
                    .filter(|group| group.storage_mode == "summary_only")
                    .count(),
            )
            .unwrap_or(u64::MAX),
            compaction_manifest_hash: format!("sha256_{:x}", Sha256::digest(manifest)),
            groups: reports,
        },
        groups: compactable,
    })
}

fn legacy_compaction_group_id(project_key: &str, key: &LegacyCompactionKey) -> String {
    let values = [
        project_key,
        key.0.as_str(),
        key.1.as_str(),
        key.2.as_str(),
        key.3.as_str(),
        key.4.as_str(),
        key.5.as_str(),
        key.6.as_str(),
        LEGACY_COMPACTION_ALGORITHM_ID,
        LEGACY_COMPACTION_ALGORITHM_VERSION,
    ];
    format!("lineage_group_legacy_v1_{}", stable_parts_hash(&values))
}

fn stable_string_set_hash(values: &[String]) -> String {
    let parts = values.iter().map(String::as_str).collect::<Vec<_>>();
    format!("sha256_{}", stable_parts_hash(&parts))
}

fn stable_parts_hash(values: &[&str]) -> String {
    let mut digest = Sha256::new();
    for value in values {
        digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(value.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn legacy_compaction_request_hash(project_key: &str) -> String {
    format!(
        "sha256_{}",
        stable_parts_hash(&[
            project_key,
            "compact-legacy-proposals",
            LEGACY_COMPACTION_ALGORITHM_VERSION,
        ])
    )
}

struct SemanticScopeBoundary<'a> {
    project_key: &'a str,
    provider_profile_id: &'a str,
    provider_contract_id: &'a str,
    language_id: &'a str,
}

#[derive(Debug)]
struct SemanticSnapshotRecord {
    fingerprint: String,
    source: SemanticSnapshotSource,
}

#[derive(Debug)]
struct StoredObservation {
    provider_symbol: Option<String>,
    is_local: bool,
}

fn validate_scope_boundary(
    boundary: &SemanticScopeBoundary<'_>,
    snapshot: &str,
    symbol: &str,
) -> Result<(), StoreError> {
    if !is_valid_project_key(boundary.project_key)
        || boundary.provider_profile_id.trim().is_empty()
        || boundary.provider_contract_id.trim().is_empty()
        || SourceLanguage::parse(boundary.language_id).is_none()
        || snapshot.trim().is_empty()
        || symbol.trim().is_empty()
    {
        return Err(StoreError::InvalidLineage(
            "semantic scope 边界或锚点无效".to_owned(),
        ));
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "单条只读 SQL 明确选择每个快照的最新 append-only provenance，拆分会重复边界条件"
)]
fn semantic_snapshot_chain(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    anchor_snapshot: &str,
) -> Result<Vec<SemanticSnapshotRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.snapshot_fingerprint,
                COALESCE((SELECT a.worktree_fingerprint
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.worktree_fingerprint),
                COALESCE((SELECT a.head_revision
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.head_revision),
                COALESCE((SELECT a.worktree_clean
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.worktree_clean),
                COALESCE((SELECT a.source_trust
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.source_trust),
                COALESCE((SELECT a.provider_registration_id
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.provider_registration_id),
                COALESCE((SELECT a.executable_sha256
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.executable_sha256),
                COALESCE((SELECT a.artifact_sha256
                          FROM semantic_snapshot_attestations a
                          WHERE a.project_key = s.project_key
                            AND a.provider_profile_id = s.provider_profile_id
                            AND a.provider_contract_id = s.provider_contract_id
                            AND a.snapshot_fingerprint = s.snapshot_fingerprint
                          ORDER BY a.sequence DESC LIMIT 1), s.artifact_sha256)
         FROM semantic_snapshots s
         WHERE s.project_key = ?1 AND s.provider_profile_id = ?2 AND s.provider_contract_id = ?3
           AND (?4 = '' OR s.sequence >= COALESCE((
               SELECT sequence FROM semantic_snapshots
               WHERE project_key = ?1 AND provider_profile_id = ?2
                 AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4
           ), 9223372036854775807))
         ORDER BY s.sequence",
    )?;
    let rows = statement
        .query_map(
            params![
                boundary.project_key,
                boundary.provider_profile_id,
                boundary.provider_contract_id,
                anchor_snapshot,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(fingerprint, worktree, head, clean, trust, registration, executable, artifact)| {
                let trust = SemanticSourceTrust::parse(&trust).ok_or_else(|| {
                    StoreError::InvalidSymbolField {
                        field: "semantic_source_trust",
                        value: trust,
                    }
                })?;
                Ok(SemanticSnapshotRecord {
                    fingerprint,
                    source: SemanticSnapshotSource {
                        worktree_fingerprint: worktree,
                        head_revision: head,
                        worktree_clean: clean,
                        trust,
                        provider_registration_id: registration,
                        executable_sha256: executable,
                        artifact_sha256: artifact,
                    },
                })
            },
        )
        .collect()
}

fn semantic_source_observations(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    snapshot_fingerprint: &str,
) -> Result<Vec<SourceFileState>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT path, language_id, content_fingerprint, has_syntax_errors
         FROM semantic_source_observations
         WHERE project_key = ?1 AND provider_profile_id = ?2
           AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4
         ORDER BY path",
    )?;
    let rows = statement
        .query_map(
            params![
                boundary.project_key,
                boundary.provider_profile_id,
                boundary.provider_contract_id,
                snapshot_fingerprint,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(path, language, content_fingerprint, has_syntax_errors)| {
            let language =
                SourceLanguage::parse(&language).ok_or_else(|| StoreError::InvalidSymbolField {
                    field: "semantic_source_language",
                    value: language,
                })?;
            Ok(SourceFileState {
                path,
                language,
                content_fingerprint,
                has_syntax_errors,
            })
        })
        .collect()
}

fn semantic_observation(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    snapshot: &str,
    symbol: &str,
) -> Result<Option<StoredObservation>, StoreError> {
    connection
        .query_row(
            "SELECT provider_symbol, is_local
             FROM semantic_symbol_observations
             WHERE project_key = ?1 AND provider_profile_id = ?2
               AND provider_contract_id = ?3 AND language_id = ?4
               AND snapshot_fingerprint = ?5 AND symbol_id = ?6",
            params![
                boundary.project_key,
                boundary.provider_profile_id,
                boundary.provider_contract_id,
                boundary.language_id,
                snapshot,
                symbol,
            ],
            |row| {
                Ok(StoredObservation {
                    provider_symbol: row.get(0)?,
                    is_local: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn provider_symbol_is_ambiguous(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    snapshot: &str,
    observation: &StoredObservation,
) -> Result<bool, StoreError> {
    let Some(provider_symbol) = observation.provider_symbol.as_deref() else {
        return Ok(true);
    };
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM semantic_symbol_observations
         WHERE project_key = ?1 AND provider_profile_id = ?2
           AND provider_contract_id = ?3 AND language_id = ?4
           AND snapshot_fingerprint = ?5 AND provider_symbol = ?6",
        params![
            boundary.project_key,
            boundary.provider_profile_id,
            boundary.provider_contract_id,
            boundary.language_id,
            snapshot,
            provider_symbol,
        ],
        |row| row.get(0),
    )?;
    Ok(count != 1)
}

fn confirmed_lineage_hop(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    from_snapshot: &str,
    from_symbol: &str,
    to_snapshot: &str,
) -> Result<Option<(String, String)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT c.to_symbol_id,
                COALESCE((
                    SELECT d.decision_id FROM semantic_lineage_decisions d
                    WHERE (d.candidate_id = c.candidate_id AND d.to_state = 'confirmed')
                       OR (d.related_candidate_id = c.candidate_id AND d.action = 'supersede')
                    ORDER BY d.created_at_unix_seconds DESC, d.decision_id DESC LIMIT 1
                ), '')
         FROM semantic_lineage_candidates c
         WHERE c.project_key = ?1 AND c.provider_profile_id = ?2
           AND c.provider_contract_id = ?3 AND c.language_id = ?4
           AND c.from_snapshot_fingerprint = ?5 AND c.from_symbol_id = ?6
           AND c.to_snapshot_fingerprint = ?7 AND c.state = 'confirmed'",
    )?;
    let rows = statement
        .query_map(
            params![
                boundary.project_key,
                boundary.provider_profile_id,
                boundary.provider_contract_id,
                boundary.language_id,
                from_snapshot,
                from_symbol,
                to_snapshot,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() != 1 || rows[0].1.is_empty() {
        return Ok(None);
    }
    Ok(rows.into_iter().next())
}

fn current_semantic_symbol(
    connection: &Connection,
    boundary: &SemanticScopeBoundary<'_>,
    snapshot: &str,
    symbol: &str,
) -> Result<Option<SymbolNode>, StoreError> {
    let observation = semantic_observation(connection, boundary, snapshot, symbol)?;
    let Some(observation) = observation else {
        return Ok(None);
    };
    if observation.is_local
        || observation
            .provider_symbol
            .as_deref()
            .is_none_or(str::is_empty)
        || provider_symbol_is_ambiguous(connection, boundary, snapshot, &observation)?
    {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT project_key, id, provider_id, identity_quality, language, kind, provider_key,
                    display_name, path, start_line, end_line, content_fingerprint, status
             FROM symbol_nodes
             WHERE project_key = ?1 AND id = ?2 AND provider_id = ?3
               AND identity_quality = 'semantic' AND language = ?4 AND status = 'active'",
            params![
                boundary.project_key,
                symbol,
                boundary.provider_contract_id,
                boundary.language_id,
            ],
            decode_symbol_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn unresolved_scope(
    anchor_snapshot: &str,
    anchor_symbol: &str,
    latest: Option<&SemanticSnapshotRecord>,
    reason: &str,
) -> SemanticScopeResolution {
    SemanticScopeResolution {
        kind: SemanticResolutionKind::Unresolved,
        anchor_snapshot_fingerprint: anchor_snapshot.to_owned(),
        anchor_symbol_id: anchor_symbol.to_owned(),
        latest_snapshot_fingerprint: latest.map(|snapshot| snapshot.fingerprint.clone()),
        resolved_symbol: None,
        source: latest.map(|snapshot| snapshot.source.clone()),
        lineage_decision_ids: Vec::new(),
        reason: Some(reason.to_owned()),
    }
}

fn semantic_source_manifest_recorded(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
    provider_profile_id: &str,
) -> Result<bool, StoreError> {
    transaction
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM semantic_source_manifests
                 WHERE project_key = ?1 AND provider_profile_id = ?2
                   AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4
             )",
            params![
                snapshot.project_key,
                provider_profile_id,
                snapshot.provider.id,
                snapshot.source_revision
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn persist_semantic_source_manifest(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
    provider_profile_id: &str,
) -> Result<(), StoreError> {
    let source_count = i64::try_from(snapshot.sources.len()).map_err(|_| {
        StoreError::InvalidSnapshot("源文件清单数量超出 SQLite 整数范围".to_owned())
    })?;
    transaction.execute(
        "INSERT INTO semantic_source_manifests(
             project_key, provider_profile_id, provider_contract_id,
             snapshot_fingerprint, source_count, manifest_sha256
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            snapshot.project_key,
            provider_profile_id,
            snapshot.provider.id,
            snapshot.source_revision,
            source_count,
            semantic_source_manifest_hash(&snapshot.sources),
        ],
    )?;
    let mut statement = transaction.prepare_cached(
        "INSERT INTO semantic_source_observations(
             project_key, provider_profile_id, provider_contract_id,
             snapshot_fingerprint, path, language_id,
             content_fingerprint, has_syntax_errors
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for source in &snapshot.sources {
        statement.execute(params![
            snapshot.project_key,
            provider_profile_id,
            snapshot.provider.id,
            snapshot.source_revision,
            source.path,
            source.language.as_str(),
            source.content_fingerprint,
            source.has_syntax_errors,
        ])?;
    }
    Ok(())
}

fn semantic_source_manifest_hash(sources: &[SourceFileState]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"project-brain/semantic-source-manifest/v1\0");
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered
        .sort_by(|left, right| (&left.path, &left.language).cmp(&(&right.path, &right.language)));
    for source in ordered {
        for value in [
            source.path.as_bytes(),
            source.language.as_str().as_bytes(),
            source.content_fingerprint.as_bytes(),
        ] {
            digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
            digest.update(value);
        }
        digest.update([u8::from(source.has_syntax_errors)]);
    }
    format!("{:x}", digest.finalize())
}

fn persist_semantic_attestation(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
    provider_profile_id: &str,
    source: &SemanticSnapshotSource,
    now: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT OR IGNORE INTO semantic_snapshot_attestations(
             project_key, provider_profile_id, provider_contract_id,
             snapshot_fingerprint, worktree_fingerprint, head_revision,
             worktree_clean, source_trust, provider_registration_id,
             executable_sha256, artifact_sha256, created_at_unix_seconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            snapshot.project_key,
            provider_profile_id,
            snapshot.provider.id,
            snapshot.source_revision,
            source.worktree_fingerprint,
            source.head_revision,
            source.worktree_clean,
            source.trust.as_str(),
            source.provider_registration_id,
            source.executable_sha256,
            source.artifact_sha256,
            now,
        ],
    )?;
    Ok(())
}

fn unix_seconds() -> Result<i64, StoreError> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn fingerprint_parts(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    format!("sha256_{:x}", digest.finalize())
}

fn validate_semantic_snapshot_source(source: &SemanticSnapshotSource) -> Result<(), StoreError> {
    if source.worktree_fingerprint.len() != 64
        || !source
            .worktree_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || source.head_revision.trim().is_empty()
        || source.head_revision.len() > 256
    {
        return Err(StoreError::InvalidSnapshot(
            "semantic snapshot 缺少合法 worktree_fingerprint/head_revision".to_owned(),
        ));
    }
    let trusted_fields = [
        source.provider_registration_id.as_deref(),
        source.executable_sha256.as_deref(),
        source.artifact_sha256.as_deref(),
    ];
    match source.trust {
        SemanticSourceTrust::OfflineImport if trusted_fields.iter().any(Option::is_some) => {
            return Err(StoreError::InvalidSnapshot(
                "offline_import 不得携带伪造的 Provider trust 字段".to_owned(),
            ));
        }
        SemanticSourceTrust::TrustedProvider => {
            let Some(registration) = source.provider_registration_id.as_deref() else {
                return Err(StoreError::InvalidSnapshot(
                    "trusted_provider 缺少 registration_id".to_owned(),
                ));
            };
            if registration.trim().is_empty()
                || registration.len() > 192
                || ![
                    source.executable_sha256.as_deref(),
                    source.artifact_sha256.as_deref(),
                ]
                .into_iter()
                .all(|value| {
                    value.is_some_and(|digest| {
                        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                })
            {
                return Err(StoreError::InvalidSnapshot(
                    "trusted_provider 的 registration/executable/artifact 证明无效".to_owned(),
                ));
            }
        }
        SemanticSourceTrust::OfflineImport => {}
    }
    Ok(())
}

fn validate_semantic_observations(
    snapshot: &SymbolSnapshot,
    provider_profile_id: &str,
    observations: &[LineageSymbolObservation],
) -> Result<(), StoreError> {
    if provider_profile_id.trim().is_empty() || provider_profile_id.len() > 64 {
        return Err(StoreError::InvalidLineage(
            "provider_profile_id 为空或过长".to_owned(),
        ));
    }
    let symbols = snapshot
        .symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect::<std::collections::BTreeMap<_, _>>();
    if observations.len() != symbols.len() {
        return Err(StoreError::InvalidLineage(format!(
            "observation 数量 {} 与 snapshot symbols {} 不一致",
            observations.len(),
            symbols.len()
        )));
    }
    let mut observed = std::collections::BTreeSet::new();
    for observation in observations {
        let Some(symbol) = symbols.get(observation.symbol_id.as_str()) else {
            return Err(StoreError::InvalidLineage(format!(
                "observation 引用 snapshot 外 symbol={}",
                observation.symbol_id
            )));
        };
        if !observed.insert(observation.symbol_id.as_str())
            || observation.project_key != snapshot.project_key
            || observation.provider_profile_id != provider_profile_id
            || observation.provider_contract_id != snapshot.provider.id
            || observation.snapshot_revision != snapshot.source_revision
            || observation.language != symbol.language
            || observation.kind != symbol.kind
            || observation.display_name != symbol.display_name
            || observation.path != symbol.path
            || !is_sha256_fingerprint(&observation.normalized_definition_fingerprint)
        {
            return Err(StoreError::InvalidLineage(format!(
                "observation 与 snapshot 边界或内容不一致：symbol={}",
                observation.symbol_id
            )));
        }
    }
    Ok(())
}

fn latest_semantic_observations(
    transaction: &Transaction<'_>,
    project_key: &str,
    provider_profile_id: &str,
    provider_contract_id: &str,
) -> Result<Vec<LineageSymbolObservation>, StoreError> {
    let latest = transaction
        .query_row(
            "SELECT snapshot_fingerprint FROM semantic_snapshots
             WHERE project_key = ?1 AND provider_profile_id = ?2 AND provider_contract_id = ?3
             ORDER BY sequence DESC LIMIT 1",
            params![project_key, provider_profile_id, provider_contract_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(snapshot) = latest else {
        return Ok(Vec::new());
    };
    let mut statement = transaction.prepare(
        "SELECT project_key, provider_profile_id, provider_contract_id, language_id,
                snapshot_fingerprint, symbol_id, provider_symbol, is_local, kind,
                display_name, path, normalized_definition_fingerprint
         FROM semantic_symbol_observations
         WHERE project_key = ?1 AND provider_profile_id = ?2
           AND provider_contract_id = ?3 AND snapshot_fingerprint = ?4
         ORDER BY symbol_id",
    )?;
    statement
        .query_map(
            params![
                project_key,
                provider_profile_id,
                provider_contract_id,
                snapshot
            ],
            decode_lineage_observation_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

fn persist_semantic_observations(
    transaction: &Transaction<'_>,
    observations: &[LineageSymbolObservation],
) -> Result<(), StoreError> {
    for observation in observations {
        transaction.execute(
            "INSERT INTO semantic_symbol_observations(
                 project_key, provider_profile_id, provider_contract_id, language_id,
                 snapshot_fingerprint, symbol_id, provider_symbol, is_local, kind,
                 display_name, path, normalized_definition_fingerprint
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                observation.project_key,
                observation.provider_profile_id,
                observation.provider_contract_id,
                observation.language.as_str(),
                observation.snapshot_revision,
                observation.symbol_id,
                observation.provider_symbol,
                observation.is_local,
                observation.kind,
                observation.display_name,
                observation.path,
                observation.normalized_definition_fingerprint,
            ],
        )?;
    }
    Ok(())
}

fn persist_lineage_groups(
    transaction: &Transaction<'_>,
    groups: &[LineageGroupProposal],
    now: i64,
) -> Result<(u64, u64), StoreError> {
    let mut groups_inserted = 0_u64;
    let mut members_inserted = 0_u64;
    for group in groups {
        let from_count = i64::try_from(group.from_count)
            .map_err(|_| StoreError::InvalidLineage("from_count 超出 SQLite INTEGER".to_owned()))?;
        let to_count = i64::try_from(group.to_count)
            .map_err(|_| StoreError::InvalidLineage("to_count 超出 SQLite INTEGER".to_owned()))?;
        let potential_pair_count = i64::try_from(group.potential_pair_count).map_err(|_| {
            StoreError::InvalidLineage("potential_pair_count 超出 SQLite INTEGER".to_owned())
        })?;
        groups_inserted += u64::try_from(transaction.execute(
            "INSERT INTO semantic_lineage_groups(
                 group_id, project_key, provider_profile_id, provider_contract_id, language_id,
                 from_snapshot_fingerprint, to_snapshot_fingerprint, symbol_kind,
                 definition_fingerprint, algorithm_id, algorithm_version,
                 from_count, to_count, potential_pair_count, review_class, storage_mode,
                 from_members_hash, to_members_hash, created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(group_id) DO NOTHING",
            params![
                group.group_id,
                group.project_key,
                group.provider_profile_id,
                group.provider_contract_id,
                group.language.as_str(),
                group.from_snapshot,
                group.to_snapshot,
                group.symbol_kind,
                group.definition_fingerprint,
                group.algorithm_id,
                group.algorithm_version,
                from_count,
                to_count,
                potential_pair_count,
                group.review_class.as_str(),
                group.storage_mode.as_str(),
                group.from_members_hash,
                group.to_members_hash,
                now,
            ],
        )?)
        .unwrap_or(u64::MAX);
        let stored = transaction.query_row(
            "SELECT from_count, to_count, potential_pair_count, review_class, storage_mode,
                    from_members_hash, to_members_hash
             FROM semantic_lineage_groups WHERE group_id = ?1",
            [&group.group_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        if stored
            != (
                from_count,
                to_count,
                potential_pair_count,
                group.review_class.as_str().to_owned(),
                group.storage_mode.as_str().to_owned(),
                group.from_members_hash.clone(),
                group.to_members_hash.clone(),
            )
        {
            return Err(StoreError::InvalidLineage(format!(
                "同一 immutable lineage group 重算结果不同：{}",
                group.group_id
            )));
        }
        for (side, members) in [("from", &group.from_members), ("to", &group.to_members)] {
            for symbol_id in members {
                members_inserted += u64::try_from(transaction.execute(
                    "INSERT INTO semantic_lineage_group_members(group_id, side, symbol_id)
                     VALUES (?1, ?2, ?3) ON CONFLICT(group_id, side, symbol_id) DO NOTHING",
                    params![group.group_id, side, symbol_id],
                )?)
                .unwrap_or(u64::MAX);
            }
        }
    }
    Ok((groups_inserted, members_inserted))
}

fn persist_lineage_generation_runs(
    transaction: &Transaction<'_>,
    proposals: &LineageProposalSet,
    now: i64,
) -> Result<(), StoreError> {
    type Boundary = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let mut by_boundary = std::collections::BTreeMap::<Boundary, Vec<&LineageGroupProposal>>::new();
    for group in &proposals.groups {
        by_boundary
            .entry((
                group.project_key.clone(),
                group.provider_profile_id.clone(),
                group.provider_contract_id.clone(),
                group.language.as_str().to_owned(),
                group.from_snapshot.clone(),
                group.to_snapshot.clone(),
                group.algorithm_id.clone(),
                group.algorithm_version.clone(),
            ))
            .or_default()
            .push(group);
    }
    for (boundary, mut groups) in by_boundary {
        groups.sort_by_key(|group| group.group_id.as_str());
        let unique = groups
            .iter()
            .filter(|group| group.review_class.as_str() == "unique")
            .count();
        let ambiguous = groups
            .iter()
            .filter(|group| group.review_class.as_str() == "ambiguous")
            .count();
        let oversized = groups
            .iter()
            .filter(|group| group.review_class.as_str() == "oversized")
            .count();
        let potential = groups
            .iter()
            .try_fold(0_u64, |total, group| {
                total.checked_add(group.potential_pair_count)
            })
            .ok_or_else(|| StoreError::InvalidLineage("generation run pair 计数溢出".to_owned()))?;
        let manifest = serde_json::to_vec(&groups)?;
        let manifest_hash = format!("sha256_{:x}", Sha256::digest(&manifest));
        let candidate_count = proposals
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.project_key == boundary.0
                    && candidate.provider_profile_id == boundary.1
                    && candidate.provider_contract_id == boundary.2
                    && candidate.language.as_str() == boundary.3
                    && candidate.from_snapshot == boundary.4
                    && candidate.to_snapshot == boundary.5
            })
            .count();
        transaction.execute(
            "INSERT INTO semantic_lineage_generation_runs(
                 project_key, provider_profile_id, provider_contract_id, language_id,
                 from_snapshot_fingerprint, to_snapshot_fingerprint, algorithm_id,
                 algorithm_version, group_count, unique_group_count, ambiguous_group_count,
                 oversized_group_count, potential_pair_count, materialized_candidate_count,
                 group_manifest_hash, created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(project_key, provider_profile_id, provider_contract_id, language_id,
                         from_snapshot_fingerprint, to_snapshot_fingerprint,
                         algorithm_id, algorithm_version) DO NOTHING",
            params![
                boundary.0,
                boundary.1,
                boundary.2,
                boundary.3,
                boundary.4,
                boundary.5,
                boundary.6,
                boundary.7,
                i64::try_from(groups.len()).unwrap_or(i64::MAX),
                i64::try_from(unique).unwrap_or(i64::MAX),
                i64::try_from(ambiguous).unwrap_or(i64::MAX),
                i64::try_from(oversized).unwrap_or(i64::MAX),
                i64::try_from(potential).map_err(|_| StoreError::InvalidLineage(
                    "generation run potential pair 超出 SQLite INTEGER".to_owned()
                ))?,
                i64::try_from(candidate_count).unwrap_or(i64::MAX),
                manifest_hash,
                now,
            ],
        )?;
    }
    Ok(())
}

fn persist_lineage_proposals(
    transaction: &Transaction<'_>,
    proposals: &[LineageCandidateProposal],
    now: i64,
) -> Result<(u64, u64), StoreError> {
    let mut candidates_inserted = 0_u64;
    let mut evidence_inserted = 0_u64;
    for proposal in proposals {
        if proposal.from_snapshot == proposal.to_snapshot {
            return Err(StoreError::InvalidLineage(
                "lineage endpoints 必须位于不同 snapshot".to_owned(),
            ));
        }
        candidates_inserted += u64::try_from(transaction.execute(
            "INSERT INTO semantic_lineage_candidates(
                 project_key, provider_profile_id, provider_contract_id, language_id,
                 from_snapshot_fingerprint, from_symbol_id,
                 to_snapshot_fingerprint, to_symbol_id, state,
                 ambiguity_group_id, origin_group_id, proposal_origin,
                 created_at_unix_seconds, updated_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'proposed', ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT(project_key, provider_profile_id, provider_contract_id, language_id,
                         from_snapshot_fingerprint, from_symbol_id,
                         to_snapshot_fingerprint, to_symbol_id) DO NOTHING",
            params![
                proposal.project_key,
                proposal.provider_profile_id,
                proposal.provider_contract_id,
                proposal.language.as_str(),
                proposal.from_snapshot,
                proposal.from_symbol,
                proposal.to_snapshot,
                proposal.to_symbol,
                proposal.ambiguity_group_id,
                proposal.origin_group_id,
                proposal.proposal_origin,
                now,
            ],
        )?)
        .unwrap_or(u64::MAX);
        let candidate_id: String = transaction.query_row(
            "SELECT candidate_id FROM semantic_lineage_candidates
             WHERE project_key = ?1 AND provider_profile_id = ?2
               AND provider_contract_id = ?3 AND language_id = ?4
               AND from_snapshot_fingerprint = ?5 AND from_symbol_id = ?6
               AND to_snapshot_fingerprint = ?7 AND to_symbol_id = ?8",
            params![
                proposal.project_key,
                proposal.provider_profile_id,
                proposal.provider_contract_id,
                proposal.language.as_str(),
                proposal.from_snapshot,
                proposal.from_symbol,
                proposal.to_snapshot,
                proposal.to_symbol,
            ],
            |row| row.get(0),
        )?;
        let evidence_json = serde_json::to_string(&proposal.evidence)?;
        evidence_inserted += u64::try_from(transaction.execute(
            "INSERT INTO semantic_lineage_evidence(
                 candidate_id, algorithm_id, algorithm_version, evidence_schema_version,
                 input_fingerprint, confidence_band, evidence_json, evidence_hash,
                 created_at_unix_seconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(candidate_id, algorithm_id, algorithm_version,
                         input_fingerprint, evidence_hash) DO NOTHING",
            params![
                candidate_id,
                proposal.algorithm_id,
                proposal.algorithm_version,
                LINEAGE_EVIDENCE_SCHEMA_VERSION,
                proposal.input_fingerprint,
                proposal.confidence.as_str(),
                evidence_json,
                proposal.evidence_fingerprint,
                now,
            ],
        )?)
        .unwrap_or(u64::MAX);
    }
    Ok((candidates_inserted, evidence_inserted))
}

fn decode_lineage_observation_row(
    row: &rusqlite::Row<'_>,
) -> Result<LineageSymbolObservation, rusqlite::Error> {
    let raw_language: String = row.get(3)?;
    let language = SourceLanguage::parse(&raw_language)
        .ok_or_else(|| invalid_lineage_sql(3, format!("language_id={raw_language:?}")))?;
    Ok(LineageSymbolObservation {
        project_key: row.get(0)?,
        provider_profile_id: row.get(1)?,
        provider_contract_id: row.get(2)?,
        language,
        snapshot_revision: row.get(4)?,
        symbol_id: row.get(5)?,
        provider_symbol: row.get(6)?,
        is_local: row.get(7)?,
        kind: row.get(8)?,
        display_name: row.get(9)?,
        path: row.get(10)?,
        normalized_definition_fingerprint: row.get(11)?,
    })
}

fn decode_lineage_group_row(
    row: &rusqlite::Row<'_>,
) -> Result<LineageGroupRecord, rusqlite::Error> {
    let from_count = row.get::<_, i64>(11)?;
    let to_count = row.get::<_, i64>(12)?;
    let potential_pair_count = row.get::<_, i64>(13)?;
    Ok(LineageGroupRecord {
        group_id: row.get(0)?,
        project_key: row.get(1)?,
        provider_profile_id: row.get(2)?,
        provider_contract_id: row.get(3)?,
        language_id: row.get(4)?,
        from_snapshot_fingerprint: row.get(5)?,
        to_snapshot_fingerprint: row.get(6)?,
        symbol_kind: row.get(7)?,
        definition_fingerprint: row.get(8)?,
        algorithm_id: row.get(9)?,
        algorithm_version: row.get(10)?,
        from_count: u64::try_from(from_count)
            .map_err(|_| invalid_lineage_sql(11, format!("from_count={from_count}")))?,
        to_count: u64::try_from(to_count)
            .map_err(|_| invalid_lineage_sql(12, format!("to_count={to_count}")))?,
        potential_pair_count: u64::try_from(potential_pair_count).map_err(|_| {
            invalid_lineage_sql(13, format!("potential_pair_count={potential_pair_count}"))
        })?,
        review_class: row.get(14)?,
        storage_mode: row.get(15)?,
        from_members_hash: row.get(16)?,
        to_members_hash: row.get(17)?,
        created_at_unix_seconds: row.get(18)?,
    })
}

fn decode_lineage_candidate_row(
    row: &rusqlite::Row<'_>,
) -> Result<LineageCandidateRecord, rusqlite::Error> {
    let raw_language: String = row.get(4)?;
    let language = SourceLanguage::parse(&raw_language)
        .ok_or_else(|| invalid_lineage_sql(4, format!("language_id={raw_language:?}")))?;
    let raw_state: String = row.get(9)?;
    let state = LineageState::parse(&raw_state)
        .ok_or_else(|| invalid_lineage_sql(9, format!("state={raw_state:?}")))?;
    let revision: i64 = row.get(11)?;
    let evidence_count: i64 = row.get(12)?;
    Ok(LineageCandidateRecord {
        candidate_id: row.get(0)?,
        project_key: row.get(1)?,
        provider_profile_id: row.get(2)?,
        provider_contract_id: row.get(3)?,
        language,
        from_snapshot_fingerprint: row.get(5)?,
        from_symbol_id: row.get(6)?,
        to_snapshot_fingerprint: row.get(7)?,
        to_symbol_id: row.get(8)?,
        state,
        ambiguity_group_id: row.get(10)?,
        revision: u64::try_from(revision).unwrap_or_default(),
        evidence_count: u64::try_from(evidence_count).unwrap_or_default(),
        created_at_unix_seconds: row.get(13)?,
        updated_at_unix_seconds: row.get(14)?,
    })
}

fn invalid_lineage_sql(column: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(StoreError::InvalidLineage(message)),
    )
}

fn invalid_provider_qualification_sql(column: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Integer,
        Box::new(StoreError::InvalidProviderQualification(message)),
    )
}

fn validate_lineage_command(
    project_key: &str,
    candidate_id: &str,
    request_id: &str,
    actor_ref: Option<&str>,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    if !is_valid_project_key(project_key)
        || candidate_id.trim().is_empty()
        || candidate_id.len() > 128
        || request_id.trim().is_empty()
        || request_id.len() > 128
        || actor_ref.is_some_and(|value| value.trim().is_empty() || value.len() > 256)
        || reason.is_some_and(|value| value.trim().is_empty() || value.len() > 4_096)
    {
        return Err(StoreError::InvalidLineage(
            "project/candidate/request/actor/reason 参数无效".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lineage_request_hash(
    project_key: &str,
    candidate_id: &str,
    request_id: &str,
    action: LineageDecisionAction,
    related_candidate_id: Option<&str>,
    actor_ref: Option<&str>,
    reason: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    for part in [
        project_key,
        candidate_id,
        request_id,
        action.as_str(),
        related_candidate_id.unwrap_or_default(),
        actor_ref.unwrap_or_default(),
        reason.unwrap_or_default(),
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("sha256_{:x}", digest.finalize())
}

fn lineage_candidate_by_id(
    connection: &Connection,
    project_key: &str,
    candidate_id: &str,
) -> Result<LineageCandidateRecord, StoreError> {
    connection
        .query_row(
            "SELECT c.candidate_id, c.project_key, c.provider_profile_id,
                    c.provider_contract_id, c.language_id,
                    c.from_snapshot_fingerprint, c.from_symbol_id,
                    c.to_snapshot_fingerprint, c.to_symbol_id, c.state,
                    c.ambiguity_group_id, c.revision,
                    (SELECT COUNT(*) FROM semantic_lineage_evidence e
                     WHERE e.candidate_id = c.candidate_id),
                    c.created_at_unix_seconds, c.updated_at_unix_seconds
             FROM semantic_lineage_candidates c
             WHERE c.project_key = ?1 AND c.candidate_id = ?2",
            params![project_key, candidate_id],
            decode_lineage_candidate_row,
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::LineageConflict(format!(
                "项目 {project_key:?} 中不存在 candidate={candidate_id:?}"
            ))
        })
}

fn same_lineage_boundary(left: &LineageCandidateRecord, right: &LineageCandidateRecord) -> bool {
    left.project_key == right.project_key
        && left.provider_profile_id == right.provider_profile_id
        && left.provider_contract_id == right.provider_contract_id
        && left.language == right.language
        && left.from_snapshot_fingerprint == right.from_snapshot_fingerprint
        && left.to_snapshot_fingerprint == right.to_snapshot_fingerprint
}

fn cas_lineage_state(
    transaction: &Transaction<'_>,
    before: &LineageCandidateRecord,
    to_state: LineageState,
    now: i64,
) -> Result<(), StoreError> {
    let changed = transaction
        .execute(
            "UPDATE semantic_lineage_candidates
             SET state = ?1, revision = revision + 1, updated_at_unix_seconds = ?2
             WHERE project_key = ?3 AND candidate_id = ?4
               AND state = ?5 AND revision = ?6",
            params![
                to_state.as_str(),
                now,
                before.project_key,
                before.candidate_id,
                before.state.as_str(),
                i64::try_from(before.revision).unwrap_or(i64::MAX),
            ],
        )
        .map_err(|error| StoreError::LineageConflict(error.to_string()))?;
    if changed != 1 {
        return Err(StoreError::LineageConflict(format!(
            "candidate={} 已被并发修改",
            before.candidate_id
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_lineage_decision(
    transaction: &Transaction<'_>,
    project_key: &str,
    request_id: &str,
    request_hash: &str,
    candidate_id: &str,
    action: LineageDecisionAction,
    from_state: LineageState,
    to_state: LineageState,
    related_candidate_id: Option<&str>,
    actor_ref: Option<&str>,
    reason: Option<&str>,
    now: i64,
) -> Result<LineageDecisionRecord, StoreError> {
    transaction.execute(
        "INSERT INTO semantic_lineage_decisions(
             project_key, request_id, request_hash, candidate_id, action,
             from_state, to_state, related_candidate_id, actor_kind,
             actor_ref, reason, created_at_unix_seconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                   'explicit_user', ?9, ?10, ?11)",
        params![
            project_key,
            request_id,
            request_hash,
            candidate_id,
            action.as_str(),
            from_state.as_str(),
            to_state.as_str(),
            related_candidate_id,
            actor_ref,
            reason,
            now,
        ],
    )?;
    decision_by_request(transaction, project_key, request_id)?
        .ok_or_else(|| StoreError::Integrity("lineage decision 插入后无法读取".to_owned()))
}

fn replay_lineage_decision(
    transaction: &Transaction<'_>,
    project_key: &str,
    request_id: &str,
    request_hash: &str,
) -> Result<Option<LineageAdjudicationResult>, StoreError> {
    let stored_hash = transaction
        .query_row(
            "SELECT request_hash FROM semantic_lineage_decisions
             WHERE project_key = ?1 AND request_id = ?2",
            params![project_key, request_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(stored_hash) = stored_hash else {
        return Ok(None);
    };
    if stored_hash != request_hash {
        return Err(StoreError::LineageIdempotencyConflict(
            request_id.to_owned(),
        ));
    }
    let decision = decision_by_request(transaction, project_key, request_id)?
        .ok_or_else(|| StoreError::Integrity("lineage decision 缺失".to_owned()))?;
    let (candidate_id, superseded_id) = if decision.action == LineageDecisionAction::Supersede {
        (
            decision.related_candidate_id.as_deref().ok_or_else(|| {
                StoreError::Integrity("supersede decision 缺少 related".to_owned())
            })?,
            Some(decision.candidate_id.as_str()),
        )
    } else {
        (decision.candidate_id.as_str(), None)
    };
    let candidate = lineage_candidate_by_id(transaction, project_key, candidate_id)?;
    let superseded_candidate = superseded_id
        .map(|id| lineage_candidate_by_id(transaction, project_key, id))
        .transpose()?;
    Ok(Some(LineageAdjudicationResult {
        decision,
        candidate,
        superseded_candidate,
        replayed: true,
    }))
}

fn decision_by_request(
    connection: &Connection,
    project_key: &str,
    request_id: &str,
) -> Result<Option<LineageDecisionRecord>, StoreError> {
    connection
        .query_row(
            "SELECT decision_id, project_key, request_id, candidate_id, action,
                    from_state, to_state, related_candidate_id, actor_kind,
                    actor_ref, reason, created_at_unix_seconds
             FROM semantic_lineage_decisions
             WHERE project_key = ?1 AND request_id = ?2",
            params![project_key, request_id],
            decode_lineage_decision_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn decode_lineage_decision_row(
    row: &rusqlite::Row<'_>,
) -> Result<LineageDecisionRecord, rusqlite::Error> {
    let raw_action: String = row.get(4)?;
    let action = LineageDecisionAction::parse(&raw_action)
        .ok_or_else(|| invalid_lineage_sql(4, format!("action={raw_action:?}")))?;
    let raw_from: String = row.get(5)?;
    let from_state = LineageState::parse(&raw_from)
        .ok_or_else(|| invalid_lineage_sql(5, format!("from_state={raw_from:?}")))?;
    let raw_to: String = row.get(6)?;
    let to_state = LineageState::parse(&raw_to)
        .ok_or_else(|| invalid_lineage_sql(6, format!("to_state={raw_to:?}")))?;
    Ok(LineageDecisionRecord {
        decision_id: row.get(0)?,
        project_key: row.get(1)?,
        request_id: row.get(2)?,
        candidate_id: row.get(3)?,
        action,
        from_state,
        to_state,
        related_candidate_id: row.get(7)?,
        actor_kind: row.get(8)?,
        actor_ref: row.get(9)?,
        reason: row.get(10)?,
        created_at_unix_seconds: row.get(11)?,
    })
}

fn escape_like_pattern(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

fn validate_snapshot(snapshot: &SymbolSnapshot) -> Result<(), StoreError> {
    if snapshot.protocol_version != SYMBOL_PROTOCOL_VERSION {
        return Err(StoreError::InvalidSnapshot(format!(
            "protocol_version={}，期望 {}",
            snapshot.protocol_version, SYMBOL_PROTOCOL_VERSION
        )));
    }
    if !is_valid_project_key(&snapshot.project_key)
        || snapshot.provider.id.trim().is_empty()
        || snapshot.provider.version.trim().is_empty()
        || snapshot.source_revision.trim().is_empty()
    {
        return Err(StoreError::InvalidSnapshot(
            "project_key 必须有效，provider.id、provider.version 与 source_revision 不能为空"
                .to_owned(),
        ));
    }
    let ids = snapshot
        .symbols
        .iter()
        .map(|symbol| symbol.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != snapshot.symbols.len() {
        return Err(StoreError::InvalidSnapshot("包含重复 symbol id".to_owned()));
    }
    let source_paths = snapshot
        .sources
        .iter()
        .map(|source| source.path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if source_paths.len() != snapshot.sources.len()
        || snapshot.sources.iter().any(|source| {
            !is_normalized_symbol_path(&source.path)
                || !is_sha256_fingerprint(&source.content_fingerprint)
        })
    {
        return Err(StoreError::InvalidSnapshot(
            "源文件清单包含重复路径、非规范路径或无效摘要".to_owned(),
        ));
    }
    for symbol in &snapshot.symbols {
        if symbol.project_key != snapshot.project_key
            || symbol.provider_id != snapshot.provider.id
            || symbol.identity_quality != snapshot.provider.identity_quality
            || symbol.status != SymbolStatus::Active
            || symbol.id
                != symbol_id(
                    &symbol.project_key,
                    &symbol.provider_id,
                    &symbol.provider_key,
                )
            || symbol.provider_key.contains('\0')
            || !is_normalized_symbol_path(&symbol.path)
            || !source_paths.contains(symbol.path.as_str())
            || !is_sha256_fingerprint(&symbol.content_fingerprint)
            || symbol.start_line == 0
            || symbol.end_line < symbol.start_line
        {
            return Err(StoreError::InvalidSnapshot(format!(
                "符号 {} 越过 Provider 或状态边界",
                symbol.id
            )));
        }
    }
    let mut edge_keys = std::collections::BTreeSet::new();
    for edge in &snapshot.edges {
        if edge.project_key != snapshot.project_key
            || edge.provider_id != snapshot.provider.id
            || !ids.contains(edge.source_id.as_str())
            || !ids.contains(edge.target_id.as_str())
            || !edge_keys.insert((
                edge.provider_id.as_str(),
                edge.source_id.as_str(),
                edge.target_id.as_str(),
                edge.kind,
            ))
        {
            return Err(StoreError::InvalidSnapshot(
                "边引用了快照外符号或错误 Provider".to_owned(),
            ));
        }
    }
    Ok(())
}

fn is_valid_project_key(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256_")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_raw_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_normalized_symbol_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.split('/').enumerate().all(|(index, component)| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && (index != 0 || !component.contains(':'))
        })
}

fn apply_snapshot_transaction(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
) -> Result<GraphDelta, StoreError> {
    let delta = apply_symbol_nodes(transaction, snapshot)?;
    apply_symbol_edges(transaction, snapshot)?;
    Ok(delta)
}

fn apply_symbol_nodes(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
) -> Result<GraphDelta, StoreError> {
    let mut delta = GraphDelta::default();
    let existing_active = {
        let mut statement = transaction.prepare(
            "SELECT id FROM symbol_nodes
             WHERE project_key = ?1 AND provider_id = ?2 AND status = 'active'",
        )?;
        statement
            .query_map(params![snapshot.project_key, snapshot.provider.id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?
    };
    let incoming = snapshot
        .symbols
        .iter()
        .map(|symbol| symbol.id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    for symbol in &snapshot.symbols {
        let previous = transaction
            .query_row(
                "SELECT project_key, id, provider_id, identity_quality, language, kind, provider_key,
                        display_name, path, start_line, end_line, content_fingerprint, status
                 FROM symbol_nodes WHERE project_key = ?1 AND id = ?2",
                params![snapshot.project_key, symbol.id],
                decode_symbol_row,
            )
            .optional()?;
        match previous {
            None => delta.inserted += 1,
            Some(previous) if same_symbol_observation(&previous, symbol) => delta.unchanged += 1,
            Some(_) => delta.updated += 1,
        }
        transaction.execute(
            "INSERT INTO symbol_nodes(
                 project_key, id, provider_id, identity_quality, language, kind, provider_key,
                 display_name, path, start_line, end_line, content_fingerprint, status,
                 first_seen_revision, last_seen_revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'active', ?13, ?13)
             ON CONFLICT(project_key, id) DO UPDATE SET
                 provider_id = excluded.provider_id,
                 identity_quality = excluded.identity_quality,
                 language = excluded.language,
                 kind = excluded.kind,
                 provider_key = excluded.provider_key,
                 display_name = excluded.display_name,
                 path = excluded.path,
                 start_line = excluded.start_line,
                 end_line = excluded.end_line,
                 content_fingerprint = excluded.content_fingerprint,
                 status = 'active',
                 last_seen_revision = excluded.last_seen_revision",
            params![
                snapshot.project_key,
                symbol.id,
                symbol.provider_id,
                symbol.identity_quality.as_str(),
                symbol.language.as_str(),
                symbol.kind,
                symbol.provider_key,
                symbol.display_name,
                symbol.path,
                i64::try_from(symbol.start_line).unwrap_or(i64::MAX),
                i64::try_from(symbol.end_line).unwrap_or(i64::MAX),
                symbol.content_fingerprint,
                snapshot.source_revision,
            ],
        )?;
    }

    for removed_id in existing_active.difference(&incoming) {
        transaction.execute(
            "UPDATE symbol_nodes SET status = 'removed', last_seen_revision = ?3
             WHERE project_key = ?1 AND id = ?2",
            params![snapshot.project_key, removed_id, snapshot.source_revision],
        )?;
        delta.removed += 1;
    }

    Ok(delta)
}

fn apply_symbol_edges(
    transaction: &Transaction<'_>,
    snapshot: &SymbolSnapshot,
) -> Result<(), StoreError> {
    transaction.execute(
        "UPDATE symbol_edges SET status = 'removed', last_seen_revision = ?3
         WHERE project_key = ?1 AND provider_id = ?2 AND status = 'active'",
        params![
            snapshot.project_key,
            snapshot.provider.id,
            snapshot.source_revision
        ],
    )?;
    for edge in &snapshot.edges {
        transaction.execute(
            "INSERT INTO symbol_edges(
                 project_key, provider_id, source_id, target_id, kind, status,
                 first_seen_revision, last_seen_revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)
             ON CONFLICT(project_key, provider_id, source_id, target_id, kind) DO UPDATE SET
                 status = 'active', last_seen_revision = excluded.last_seen_revision",
            params![
                edge.project_key,
                edge.provider_id,
                edge.source_id,
                edge.target_id,
                edge.kind.as_str(),
                snapshot.source_revision,
            ],
        )?;
    }
    Ok(())
}

fn decode_symbol_row(row: &rusqlite::Row<'_>) -> Result<SymbolNode, rusqlite::Error> {
    let quality: String = row.get(3)?;
    let language: String = row.get(4)?;
    let status: String = row.get(12)?;
    let identity_quality = IdentityQuality::parse(&quality).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "identity_quality",
                value: quality,
            }),
        )
    })?;
    let language = SourceLanguage::parse(&language).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "language",
                value: language,
            }),
        )
    })?;
    let status = SymbolStatus::parse(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            12,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "status",
                value: status,
            }),
        )
    })?;
    let start_line: i64 = row.get(9)?;
    let end_line: i64 = row.get(10)?;
    Ok(SymbolNode {
        project_key: row.get(0)?,
        id: row.get(1)?,
        provider_id: row.get(2)?,
        identity_quality,
        language,
        kind: row.get(5)?,
        provider_key: row.get(6)?,
        display_name: row.get(7)?,
        path: row.get(8)?,
        start_line: usize::try_from(start_line).unwrap_or_default(),
        end_line: usize::try_from(end_line).unwrap_or_default(),
        content_fingerprint: row.get(11)?,
        status,
    })
}

fn same_symbol_observation(left: &SymbolNode, right: &SymbolNode) -> bool {
    left.id == right.id
        && left.project_key == right.project_key
        && left.provider_id == right.provider_id
        && left.identity_quality == right.identity_quality
        && left.language == right.language
        && left.kind == right.kind
        && left.provider_key == right.provider_key
        && left.display_name == right.display_name
        && left.path == right.path
        && left.start_line == right.start_line
        && left.end_line == right.end_line
        && left.content_fingerprint == right.content_fingerprint
        && left.status == SymbolStatus::Active
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        sync::{Arc, Barrier},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_core::{
        ActionDescriptor, ActionKind, AdapterIdentity, AdapterKind, CURRENT_SCHEMA_VERSION,
        ContextItem, Decision, DecisionKind, EventIdentityQuality, HOOK_PROTOCOL_VERSION,
        HookEventPayload, HookOutcomePayload, IdempotencyMetadata, InternalHookEvent,
        InternalHookOutcome, SessionOpenReason, SessionOpened,
    };
    use brain_evidence::{
        EvidenceAuthority, EvidenceCoverage, EvidenceFreshness, EvidencePlane, EvidenceProvider,
        EvidenceSnapshot,
    };

    use brain_symbols::{
        GraphDelta, IdentityQuality, LineageState, LineageSymbolObservation, ProviderDescriptor,
        SYMBOL_PROTOCOL_VERSION, SourceFileState, SourceLanguage, SymbolNode, SymbolNodeInput,
        SymbolSnapshot, SymbolStatus, encode_provider_key,
    };
    use rusqlite::{Connection, params};

    use super::{
        AdapterRecordResult, BrainStore, LineageCandidateRecord, SemanticSnapshotSource,
        StoreError, semantic_source_manifest_hash,
    };

    const PROJECT_KEY: &str = "project_test";

    fn evidence_snapshot(project_key: &str) -> EvidenceSnapshot {
        EvidenceSnapshot::new(
            project_key,
            EvidencePlane::Engine,
            EvidenceProvider {
                id: "test-engine".to_owned(),
                version: "4.6+sha256.test".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            "sha256_source-test",
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn evidence_snapshots_are_deduplicated_while_attestations_and_staleness_are_audited() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = evidence_snapshot(PROJECT_KEY);

        let first = store.apply_evidence_snapshot(&snapshot).unwrap();
        let second = store.apply_evidence_snapshot(&snapshot).unwrap();
        assert!(first.snapshot_inserted);
        assert!(!second.snapshot_inserted);
        assert!(second.attestation_sequence > first.attestation_sequence);
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM evidence_snapshots", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM evidence_attestations", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
        store
            .apply_evidence_snapshot(&evidence_snapshot("project_other"))
            .unwrap();

        let stale = store
            .mark_evidence_plane_stale(
                PROJECT_KEY,
                EvidencePlane::Engine,
                "event-1",
                "successful source mutation",
                &["src/main.rs".to_owned(), "src/main.rs".to_owned()],
            )
            .unwrap();
        assert_eq!(stale.heads_marked, 1);
        assert!(!stale.replayed);
        let head = store
            .list_evidence_heads(PROJECT_KEY)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(head.freshness, EvidenceFreshness::Stale);
        assert_eq!(head.stale_event_id.as_deref(), Some("event-1"));
        let other = store
            .list_evidence_head_summaries("project_other")
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(other.freshness, EvidenceFreshness::Fresh);
        assert_eq!(other.provider_id, "test-engine");

        let replay = store
            .mark_evidence_plane_stale(
                PROJECT_KEY,
                EvidencePlane::Engine,
                "event-1",
                "successful source mutation",
                &["src/main.rs".to_owned()],
            )
            .unwrap();
        assert!(replay.replayed);
        assert!(matches!(
            store.mark_evidence_plane_stale(
                PROJECT_KEY,
                EvidencePlane::Engine,
                "event-1",
                "different event",
                &[]
            ),
            Err(StoreError::EvidenceIdempotencyConflict(_))
        ));

        store.apply_evidence_snapshot(&snapshot).unwrap();
        let head = store
            .list_evidence_heads(PROJECT_KEY)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(head.freshness, EvidenceFreshness::Fresh);
        assert!(head.stale_event_id.is_none());
        assert_eq!(head.snapshot, snapshot);
    }

    #[test]
    fn semantic_source_manifest_hash_is_independent_of_input_order() {
        let first =
            SourceFileState::from_source("src/a.rs", SourceLanguage::rust(), b"fn a() {}", false);
        let second =
            SourceFileState::from_source("src/b.rs", SourceLanguage::rust(), b"fn b() {}", false);
        assert_eq!(
            semantic_source_manifest_hash(&[first.clone(), second.clone()]),
            semantic_source_manifest_hash(&[second, first])
        );
    }

    fn provider() -> ProviderDescriptor {
        ProviderDescriptor {
            id: "test-syntax".to_owned(),
            version: "1".to_owned(),
            identity_quality: IdentityQuality::SyntaxFallback,
        }
    }

    fn symbol(name: &str) -> SymbolNode {
        symbol_for(PROJECT_KEY, name)
    }

    fn symbol_for(project_key: &str, name: &str) -> SymbolNode {
        let provider_key = encode_provider_key(&["src/lib.rs", "function_item", name, "0"]);
        let content = format!("fn {name}() {{}}");
        SymbolNode::from_provider_key(
            project_key,
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function_item",
                provider_key: &provider_key,
                display_name: name,
                path: "src/lib.rs",
                start_line: 1,
                end_line: 1,
                content: content.as_bytes(),
            },
        )
    }

    fn snapshot(revision: &str, symbols: Vec<SymbolNode>) -> SymbolSnapshot {
        snapshot_for(PROJECT_KEY, revision, symbols)
    }

    fn snapshot_for(project_key: &str, revision: &str, symbols: Vec<SymbolNode>) -> SymbolSnapshot {
        let sources = symbols
            .iter()
            .map(|symbol| (symbol.path.clone(), symbol.language.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(path, language)| {
                SourceFileState::from_source(&path, language, b"test source", false)
            })
            .collect();
        SymbolSnapshot {
            protocol_version: SYMBOL_PROTOCOL_VERSION,
            project_key: project_key.to_owned(),
            provider: provider(),
            source_revision: revision.to_owned(),
            sources,
            symbols,
            edges: Vec::new(),
        }
    }

    fn semantic_provider() -> ProviderDescriptor {
        ProviderDescriptor {
            id: "scip-test-main-test-contract-1".to_owned(),
            version: "contract-1".to_owned(),
            identity_quality: IdentityQuality::Semantic,
        }
    }

    fn semantic_symbol(name: &str, path: &str, provider_key: &str) -> SymbolNode {
        SymbolNode::from_provider_key(
            PROJECT_KEY,
            &semantic_provider(),
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function",
                provider_key,
                display_name: name,
                path,
                start_line: 1,
                end_line: 1,
                content: format!("fn {name}() {{ same_body(); }}").as_bytes(),
            },
        )
    }

    fn semantic_snapshot(revision: &str, symbols: Vec<SymbolNode>) -> SymbolSnapshot {
        let mut snapshot = snapshot(revision, symbols);
        snapshot.provider = semantic_provider();
        snapshot
    }

    fn semantic_observations(snapshot: &SymbolSnapshot) -> Vec<LineageSymbolObservation> {
        snapshot
            .symbols
            .iter()
            .map(|symbol| LineageSymbolObservation {
                project_key: snapshot.project_key.clone(),
                provider_profile_id: "test-main".to_owned(),
                provider_contract_id: snapshot.provider.id.clone(),
                language: symbol.language.clone(),
                snapshot_revision: snapshot.source_revision.clone(),
                symbol_id: symbol.id.clone(),
                provider_symbol: Some("test package demo 1 stable().".to_owned()),
                is_local: false,
                kind: symbol.kind.clone(),
                display_name: symbol.display_name.clone(),
                path: symbol.path.clone(),
                normalized_definition_fingerprint: format!("sha256_{}", "a".repeat(64)),
            })
            .collect()
    }

    fn apply_semantic(store: &BrainStore, snapshot: &SymbolSnapshot) -> super::SemanticApplyResult {
        store
            .apply_semantic_snapshot(
                snapshot,
                "test-main",
                &semantic_observations(snapshot),
                &[],
                &SemanticSnapshotSource::offline("b".repeat(64), "test-head".to_owned(), true),
            )
            .unwrap()
    }

    fn materialize_all_group_pairs(store: &BrainStore) -> Vec<LineageCandidateRecord> {
        let group_id: String = store
            .connection
            .query_row(
                "SELECT group_id FROM semantic_lineage_groups ORDER BY created_at_unix_seconds DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let members = |side: &str| {
            store
                .connection
                .prepare(
                    "SELECT symbol_id FROM semantic_lineage_group_members
                     WHERE group_id = ?1 AND side = ?2 ORDER BY symbol_id",
                )
                .unwrap()
                .query_map(params![group_id, side], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let from_members = members("from");
        let to_members = members("to");
        let mut candidates = Vec::new();
        for from in &from_members {
            for to in &to_members {
                candidates.push(
                    store
                        .materialize_lineage_group_pair(PROJECT_KEY, &group_id, from, to)
                        .unwrap(),
                );
            }
        }
        candidates
    }

    fn insert_legacy_cartesian_fixture(store: &BrainStore, complete: bool) {
        let first = semantic_snapshot(
            "legacy-from",
            vec![
                semantic_symbol("old_a", "src/old_a.rs", "legacy-old-a"),
                semantic_symbol("old_b", "src/old_b.rs", "legacy-old-b"),
            ],
        );
        let second = semantic_snapshot(
            "legacy-to",
            vec![
                semantic_symbol("new_a", "src/new_a.rs", "legacy-new-a"),
                semantic_symbol("new_b", "src/new_b.rs", "legacy-new-b"),
            ],
        );
        apply_semantic(store, &first);
        apply_semantic(store, &second);
        store
            .connection
            .execute_batch(
                "DELETE FROM semantic_lineage_group_members;
                 DELETE FROM semantic_lineage_groups;
                 DELETE FROM semantic_lineage_generation_runs;",
            )
            .unwrap();
        let mut pair_index = 0_u32;
        for from in &first.symbols {
            for to in &second.symbols {
                pair_index += 1;
                if !complete && pair_index == 4 {
                    continue;
                }
                let candidate_id = format!("legacy-candidate-{pair_index}");
                store
                    .connection
                    .execute(
                        "INSERT INTO semantic_lineage_candidates(
                             candidate_id, project_key, provider_profile_id,
                             provider_contract_id, language_id,
                             from_snapshot_fingerprint, from_symbol_id,
                             to_snapshot_fingerprint, to_symbol_id, state,
                             ambiguity_group_id, revision, created_at_unix_seconds,
                             updated_at_unix_seconds, origin_group_id, proposal_origin
                         ) VALUES (?1, ?2, 'test-main', ?3, 'rust', ?4, ?5, ?6, ?7,
                                   'proposed', 'legacy-ambiguity', 0, 1, 1, NULL, 'legacy_v7')",
                        params![
                            candidate_id,
                            PROJECT_KEY,
                            semantic_provider().id,
                            first.source_revision,
                            from.id,
                            second.source_revision,
                            to.id,
                        ],
                    )
                    .unwrap();
                store
                    .connection
                    .execute(
                        "INSERT INTO semantic_lineage_evidence(
                             evidence_id, candidate_id, algorithm_id, algorithm_version,
                             evidence_schema_version, input_fingerprint, confidence_band,
                             evidence_json, evidence_hash, created_at_unix_seconds
                         ) VALUES (?1, ?2, 'project-brain-lineage', '1', 1, ?3, 'low',
                                   '[{\"kind\":\"kind_equal\"}]', ?4, 1)",
                        params![
                            format!("legacy-evidence-{pair_index}"),
                            candidate_id,
                            format!("sha256_{pair_index:064x}"),
                            format!("sha256_{:064x}", pair_index + 8),
                        ],
                    )
                    .unwrap();
            }
        }
    }

    fn hook_event(project_key: &str, event_id: &str) -> InternalHookEvent {
        InternalHookEvent {
            protocol_version: HOOK_PROTOCOL_VERSION,
            project_key: project_key.to_owned(),
            event_id: event_id.to_owned(),
            idempotency: IdempotencyMetadata {
                identity_quality: EventIdentityQuality::VendorStable,
            },
            adapter: AdapterIdentity {
                kind: AdapterKind::Codex,
                adapter_version: 1,
            },
            session_key: "session".to_owned(),
            cwd: "/repo".to_owned(),
            turn_key: None,
            payload: HookEventPayload::SessionOpened(SessionOpened {
                reason: SessionOpenReason::Startup,
                previous_session_key: None,
            }),
        }
    }

    fn hook_outcome(event_id: &str) -> InternalHookOutcome {
        InternalHookOutcome {
            protocol_version: HOOK_PROTOCOL_VERSION,
            event_id: event_id.to_owned(),
            payload: HookOutcomePayload::SessionOpened { inject: Vec::new() },
        }
    }

    fn temporary_database(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = root.join("brain.db");
        (root, database)
    }

    #[test]
    fn records_and_reads_audit_events() {
        let store = BrainStore::open_in_memory().unwrap();
        let action = ActionDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            event_id: "event-1".to_owned(),
            session_id: "session-1".to_owned(),
            cwd: "/repo".to_owned(),
            action: ActionKind::Modify,
            operation: "apply_patch".to_owned(),
            target_files: vec!["src/main.rs".to_owned()],
            command: None,
            metadata: BTreeMap::new(),
        };
        let decision = Decision {
            schema_version: CURRENT_SCHEMA_VERSION,
            decision: DecisionKind::Allow,
            summary: "ok".to_owned(),
            context: Vec::new(),
            evidence: Vec::new(),
        };

        let id = store.record("pre_tool_use", &action, &decision).unwrap();

        assert_eq!(id, 1);
        assert_eq!(store.audit_count().unwrap(), 1);
        let records = store.recent_audit(10).unwrap();
        assert_eq!(records[0].event_id, "event-1");
        assert_eq!(records[0].hook_event, "pre_tool_use");
    }

    #[test]
    fn provider_qualification_is_append_only_and_latest_wins() {
        let store = BrainStore::open_in_memory().unwrap();
        let unstable = store
            .record_provider_qualification(
                PROJECT_KEY,
                "rust-main",
                "nondeterministic",
                5,
                "registration-1",
                1,
                &"a".repeat(64),
                &"b".repeat(64),
                &format!("sha256_{}", "c".repeat(64)),
            )
            .unwrap();
        let stable = store
            .record_provider_qualification(
                PROJECT_KEY,
                "rust-main",
                "stable_complete",
                3,
                "registration-1",
                1,
                &"a".repeat(64),
                &"d".repeat(64),
                &format!("sha256_{}", "e".repeat(64)),
            )
            .unwrap();
        assert!(stable.sequence > unstable.sequence);
        assert_eq!(
            store
                .latest_provider_qualification(PROJECT_KEY, "rust-main")
                .unwrap(),
            Some(stable)
        );
        let event_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_provider_qualification_events",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 2);
        assert!(matches!(
            store.record_provider_qualification(
                PROJECT_KEY,
                "rust-main",
                "stable_complete",
                1,
                "registration-1",
                1,
                &"a".repeat(64),
                &"b".repeat(64),
                &format!("sha256_{}", "c".repeat(64)),
            ),
            Err(StoreError::InvalidProviderQualification(_))
        ));
    }

    #[test]
    fn a_successful_retry_replaces_a_failure_for_the_same_project_event() {
        let store = BrainStore::open_in_memory().unwrap();
        let event = hook_event("project_a", "event-1");
        store
            .record_adapter_failure(&event, 3, "temporary failure")
            .unwrap();
        store
            .record_adapter_event(&event, &hook_outcome("event-1"), 4)
            .unwrap();

        let records = store.recent_adapter_audit("project_a", 10).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].failure.is_none());
        assert!(records[0].outcome_json.is_some());
    }

    #[test]
    fn rejects_an_outcome_for_a_different_event() {
        let store = BrainStore::open_in_memory().unwrap();
        let result = store.record_adapter_event(
            &hook_event("project_a", "event-1"),
            &hook_outcome("event-2"),
            1,
        );
        assert!(matches!(
            result,
            Err(super::StoreError::InvalidHookEvent(_))
        ));
    }

    #[test]
    fn concurrent_connections_converge_on_one_project_event() {
        let (root, database) = temporary_database("concurrent-audit-test");
        drop(BrainStore::open(&database).unwrap());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let store = BrainStore::open(&database).unwrap();
                    barrier.wait();
                    store
                        .record_adapter_event(
                            &hook_event("project_a", "event-1"),
                            &hook_outcome("event-1"),
                            1,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, AdapterRecordResult::Inserted(_)))
                .count(),
            1
        );
        let store = BrainStore::open(&database).unwrap();
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            1
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_database_recovers_failure_then_preserves_first_success() {
        let (root, database) = temporary_database("audit-reopen-test");
        let event = hook_event("project_a", "event-1");
        let first = hook_outcome("event-1");
        let store = BrainStore::open(&database).unwrap();
        store
            .record_adapter_failure(&event, 1, "interrupted")
            .unwrap();
        drop(store);

        let store = BrainStore::open(&database).unwrap();
        store.record_adapter_event(&event, &first, 2).unwrap();
        drop(store);

        let mut changed = first.clone();
        changed.payload = HookOutcomePayload::SessionOpened {
            inject: vec![ContextItem {
                text: "later".to_owned(),
            }],
        };
        let store = BrainStore::open(&database).unwrap();
        assert_eq!(
            store.record_adapter_event(&event, &changed, 3).unwrap(),
            AdapterRecordResult::Duplicate(first)
        );
        let records = store.recent_adapter_audit("project_a", 10).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].failure.is_none());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupted_schema_version_is_not_silently_upgraded() {
        let (root, database) = temporary_database("corrupt-schema-test");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO metadata(key, value) VALUES('schema_version', 'broken');",
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            BrainStore::open(&database),
            Err(StoreError::Integrity(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_metadata_without_schema_version_is_rejected() {
        let (root, database) = temporary_database("missing-schema-test");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        drop(connection);

        assert!(matches!(
            BrainStore::open(&database),
            Err(StoreError::Integrity(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_full_snapshots_and_preserves_removed_history() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = symbol("first");
        let second = symbol("second");
        assert_eq!(
            store
                .apply_symbol_snapshot(&snapshot("rev-1", vec![first.clone(), second.clone()]))
                .unwrap(),
            GraphDelta {
                inserted: 2,
                ..GraphDelta::default()
            }
        );
        assert_eq!(
            store
                .apply_symbol_snapshot(&snapshot("rev-2", vec![second]))
                .unwrap(),
            GraphDelta {
                unchanged: 1,
                removed: 1,
                ..GraphDelta::default()
            }
        );
        assert_eq!(
            store
                .list_symbols(PROJECT_KEY, None, false, 100)
                .unwrap()
                .len(),
            1
        );
        let history = store.list_symbols(PROJECT_KEY, None, true, 100).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            history
                .iter()
                .find(|entry| entry.id == first.id)
                .unwrap()
                .status,
            SymbolStatus::Removed
        );
    }

    #[test]
    fn symbol_snapshots_and_tombstones_are_isolated_by_project() {
        let store = BrainStore::open_in_memory().unwrap();
        let alpha = symbol_for("project_alpha", "run");
        let beta = symbol_for("project_beta", "run");

        store
            .apply_symbol_snapshot(&snapshot_for(
                "project_alpha",
                "alpha-1",
                vec![alpha.clone()],
            ))
            .unwrap();
        store
            .apply_symbol_snapshot(&snapshot_for("project_beta", "beta-1", vec![beta.clone()]))
            .unwrap();
        store
            .apply_symbol_snapshot(&snapshot_for("project_alpha", "alpha-2", Vec::new()))
            .unwrap();

        assert!(
            store
                .list_symbols("project_alpha", None, false, 10)
                .unwrap()
                .is_empty()
        );
        let beta_symbols = store.list_symbols("project_beta", None, false, 10).unwrap();
        assert_eq!(beta_symbols, vec![beta]);
        assert_ne!(alpha.id, beta_symbols[0].id);
    }

    #[test]
    fn migrates_a_v1_database_without_losing_audit_schema() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-store-migration-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = root.join("brain.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO metadata(key, value) VALUES('schema_version', '1');
                 CREATE TABLE audit_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     event_id TEXT NOT NULL,
                     session_id TEXT NOT NULL,
                     hook_event TEXT NOT NULL,
                     action_json TEXT NOT NULL,
                     decision_json TEXT NOT NULL,
                     created_at_unix_seconds INTEGER NOT NULL
                 );",
            )
            .unwrap();
        drop(connection);

        let store = BrainStore::open(&database).unwrap();
        assert_eq!(store.database_schema_version().unwrap(), 12);
        assert!(
            store
                .list_symbols(PROJECT_KEY, None, false, 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .recent_adapter_audit("project_a", 10)
                .unwrap()
                .is_empty()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v3_migration_discards_unscoped_graph_but_preserves_adapter_audit() {
        let (root, database) = temporary_database("v3-project-scope-migration");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO metadata(key, value) VALUES('schema_version', '3');
                 CREATE TABLE symbol_nodes (
                     id TEXT PRIMARY KEY,
                     provider_id TEXT NOT NULL,
                     identity_quality TEXT NOT NULL,
                     language TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     provider_key TEXT NOT NULL,
                     display_name TEXT NOT NULL,
                     path TEXT NOT NULL,
                     start_line INTEGER NOT NULL,
                     end_line INTEGER NOT NULL,
                     content_fingerprint TEXT NOT NULL,
                     status TEXT NOT NULL,
                     first_seen_revision TEXT NOT NULL,
                     last_seen_revision TEXT NOT NULL
                 );
                 INSERT INTO symbol_nodes VALUES(
                     'legacy-id', 'legacy-provider', 'syntax_fallback', 'rust',
                     'function_item', 'legacy-key', 'run', 'src/lib.rs', 1, 1,
                     'sha256_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'active', 'legacy-revision', 'legacy-revision'
                 );
                 CREATE TABLE symbol_edges (
                     provider_id TEXT NOT NULL,
                     source_id TEXT NOT NULL,
                     target_id TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     status TEXT NOT NULL,
                     first_seen_revision TEXT NOT NULL,
                     last_seen_revision TEXT NOT NULL,
                     PRIMARY KEY(provider_id, source_id, target_id, kind)
                 );
                 CREATE TABLE adapter_audit_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_key TEXT NOT NULL,
                     adapter_kind TEXT NOT NULL,
                     adapter_version INTEGER NOT NULL,
                     event_id TEXT NOT NULL,
                     session_key TEXT NOT NULL,
                     event_kind TEXT NOT NULL,
                     event_json TEXT NOT NULL,
                     outcome_json TEXT,
                     latency_ms INTEGER NOT NULL,
                     failure TEXT,
                     created_at_unix_seconds INTEGER NOT NULL,
                     UNIQUE(project_key, adapter_kind, event_id)
                 );
                 INSERT INTO adapter_audit_events(
                     project_key, adapter_kind, adapter_version, event_id, session_key,
                     event_kind, event_json, outcome_json, latency_ms, failure,
                     created_at_unix_seconds
                 ) VALUES(
                     'project_test', 'codex', 1, 'event-1', 'session-1',
                     'session_opened', '{}', NULL, 1, 'legacy failure', 1
                 );",
            )
            .unwrap();
        drop(connection);

        let store = BrainStore::open(&database).unwrap();
        assert_eq!(store.database_schema_version().unwrap(), 12);
        assert!(
            store
                .list_symbols(PROJECT_KEY, None, true, 10)
                .unwrap()
                .is_empty()
        );
        let audit = store.recent_adapter_audit(PROJECT_KEY, 10).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].event_id, "event-1");
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v5_migration_adds_stale_source_columns_without_inventing_attestation() {
        let (root, database) = temporary_database("v5-semantic-source-migration");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO metadata(key, value) VALUES('schema_version', '5');
                 CREATE TABLE semantic_snapshots (
                     sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_key TEXT NOT NULL,
                     provider_profile_id TEXT NOT NULL,
                     provider_contract_id TEXT NOT NULL,
                     snapshot_fingerprint TEXT NOT NULL,
                     created_at_unix_seconds INTEGER NOT NULL,
                     UNIQUE(project_key, provider_profile_id, provider_contract_id, snapshot_fingerprint)
                 );
                 INSERT INTO semantic_snapshots(
                     project_key, provider_profile_id, provider_contract_id,
                     snapshot_fingerprint, created_at_unix_seconds
                 ) VALUES('project_test', 'test-main', 'contract', 'old-snapshot', 1);",
            )
            .unwrap();
        drop(connection);

        let migrated = BrainStore::open(&database).unwrap();
        assert_eq!(migrated.database_schema_version().unwrap(), 12);
        let legacy_source = migrated
            .connection
            .query_row(
                "SELECT worktree_fingerprint, head_revision, worktree_clean
                 FROM semantic_snapshots WHERE snapshot_fingerprint = 'old-snapshot'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(legacy_source, (String::new(), String::new(), false));
        let attestations: i64 = migrated
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_snapshot_attestations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attestations, 0);
        let manifest = migrated
            .latest_semantic_source_manifest("project_test", "test-main", "contract")
            .unwrap()
            .unwrap();
        assert!(!manifest.recorded);
        assert!(manifest.sources.is_empty());
        drop(migrated);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_a_symbol_or_edge_from_another_project() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut wrong_symbol = symbol("run");
        wrong_symbol.project_key = "project_other".to_owned();
        assert!(matches!(
            store.apply_symbol_snapshot(&snapshot("rev-symbol", vec![wrong_symbol])),
            Err(StoreError::InvalidSnapshot(_))
        ));

        let source = symbol("source");
        let target = symbol("target");
        let mut wrong_edge_snapshot = snapshot("rev-edge", vec![source.clone(), target.clone()]);
        wrong_edge_snapshot.edges.push(brain_symbols::SymbolEdge {
            project_key: "project_other".to_owned(),
            provider_id: provider().id,
            source_id: source.id,
            target_id: target.id,
            kind: brain_symbols::EdgeKind::Calls,
        });
        assert!(matches!(
            store.apply_symbol_snapshot(&wrong_edge_snapshot),
            Err(StoreError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn path_queries_treat_like_wildcards_as_literal_characters() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut unusual = symbol("unusual");
        unusual.path = "src/a%/lib.rs".to_owned();
        store
            .apply_symbol_snapshot(&snapshot("rev-1", vec![unusual]))
            .unwrap();
        assert_eq!(
            store
                .list_symbols(PROJECT_KEY, Some("src/a%"), false, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .list_symbols(PROJECT_KEY, Some("src/a_"), false, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fallback_rename_is_insert_plus_remove_not_an_implicit_update() {
        let store = BrainStore::open_in_memory().unwrap();
        store
            .apply_symbol_snapshot(&snapshot("rev-1", vec![symbol("before")]))
            .unwrap();
        let delta = store
            .apply_symbol_snapshot(&snapshot("rev-2", vec![symbol("after")]))
            .unwrap();
        assert_eq!(
            delta,
            GraphDelta {
                inserted: 1,
                removed: 1,
                ..GraphDelta::default()
            }
        );
    }

    #[test]
    fn rejects_invalid_symbol_coordinates_before_writing() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut invalid = symbol("invalid");
        invalid.start_line = 0;
        assert!(matches!(
            store.apply_symbol_snapshot(&snapshot("rev-1", vec![invalid])),
            Err(super::StoreError::InvalidSnapshot(_))
        ));
        assert!(
            store
                .list_symbols(PROJECT_KEY, None, true, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn semantic_snapshots_create_idempotent_candidates_without_auto_adjudication() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![semantic_symbol("after", "src/b.rs", "stable-new")],
        );
        assert_eq!(apply_semantic(&store, &first).candidates_inserted, 0);
        let applied = apply_semantic(&store, &second);
        assert!(applied.snapshot_inserted);
        assert_eq!(applied.candidates_inserted, 1);
        assert_eq!(applied.evidence_inserted, 1);

        let candidates = store
            .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].state, LineageState::Proposed);
        assert_eq!(candidates[0].evidence_count, 1);

        let replay = apply_semantic(&store, &second);
        assert!(!replay.snapshot_inserted);
        assert_eq!(replay.candidates_inserted, 0);
        assert_eq!(replay.evidence_inserted, 0);
        assert_eq!(
            store
                .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            store.apply_semantic_snapshot(
                &first,
                "test-main",
                &semantic_observations(&first),
                &[],
                &SemanticSnapshotSource::offline("b".repeat(64), "test-head".to_owned(), true,),
            ),
            Err(StoreError::InvalidLineage(_))
        ));
    }

    #[test]
    fn semantic_scope_resolves_direct_identity_to_latest_snapshot() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-direct-1",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        let second = semantic_snapshot(
            "semantic-direct-2",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        let anchor = first.symbols[0].id.clone();
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);

        let resolved = store
            .resolve_semantic_scope(
                PROJECT_KEY,
                "test-main",
                &semantic_provider().id,
                "rust",
                &first.source_revision,
                &anchor,
            )
            .unwrap();
        assert_eq!(resolved.kind, super::SemanticResolutionKind::DirectSemantic);
        assert_eq!(
            resolved.latest_snapshot_fingerprint.as_deref(),
            Some(second.source_revision.as_str())
        );
        assert_eq!(resolved.resolved_symbol.unwrap().id, anchor);
        assert!(resolved.lineage_decision_ids.is_empty());
        assert_eq!(resolved.source.unwrap().head_revision, "test-head");
    }

    #[test]
    fn repeated_identical_snapshot_appends_a_fresh_source_attestation() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = semantic_snapshot(
            "semantic-attestation",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        let anchor = snapshot.symbols[0].id.clone();
        apply_semantic(&store, &snapshot);
        let refreshed =
            SemanticSnapshotSource::offline("c".repeat(64), "new-clean-head".to_owned(), true);
        let replay = store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &refreshed,
            )
            .unwrap();
        assert!(!replay.snapshot_inserted);
        let resolved = store
            .resolve_semantic_scope(
                PROJECT_KEY,
                "test-main",
                &semantic_provider().id,
                "rust",
                &snapshot.source_revision,
                &anchor,
            )
            .unwrap();
        assert_eq!(resolved.source, Some(refreshed));
        let manifest = store
            .latest_semantic_source_manifest(PROJECT_KEY, "test-main", &semantic_provider().id)
            .unwrap()
            .unwrap();
        assert!(manifest.recorded);
        assert_eq!(manifest.snapshot_fingerprint, snapshot.source_revision);
        assert_eq!(manifest.sources, snapshot.sources);
        assert_eq!(manifest.source.head_revision, "new-clean-head");
        let count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_snapshot_attestations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn identical_snapshot_accepts_new_trusted_registration_attestation() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = semantic_snapshot(
            "semantic-registration-refresh",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        let first = SemanticSnapshotSource::trusted_provider(
            "d".repeat(64),
            "same-head".to_owned(),
            true,
            "registration-one".to_owned(),
            "e".repeat(64),
            "f".repeat(64),
        );
        store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &first,
            )
            .unwrap();
        let refreshed = SemanticSnapshotSource::trusted_provider(
            "d".repeat(64),
            "same-head".to_owned(),
            true,
            "registration-two".to_owned(),
            "e".repeat(64),
            "f".repeat(64),
        );
        let replay = store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &refreshed,
            )
            .unwrap();
        assert!(!replay.snapshot_inserted);
        let latest = store
            .latest_semantic_source_manifest(PROJECT_KEY, "test-main", &semantic_provider().id)
            .unwrap()
            .unwrap();
        assert_eq!(latest.source, refreshed);
        store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &refreshed,
            )
            .unwrap();
        let count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_snapshot_attestations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn v10_attestation_identity_migrates_without_losing_history() {
        let (root, database) = temporary_database("v10-attestation-migration");
        let snapshot = semantic_snapshot(
            "semantic-v10-attestation",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        let first = SemanticSnapshotSource::trusted_provider(
            "d".repeat(64),
            "same-head".to_owned(),
            true,
            "registration-one".to_owned(),
            "e".repeat(64),
            "f".repeat(64),
        );
        let store = BrainStore::open(&database).unwrap();
        store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &first,
            )
            .unwrap();
        drop(store);

        let legacy = Connection::open(&database).unwrap();
        legacy
            .execute_batch(
                "DROP INDEX idx_semantic_attestation_identity;
                 DROP INDEX idx_semantic_attestation_latest;
                 ALTER TABLE semantic_snapshot_attestations
                     RENAME TO semantic_snapshot_attestations_v11_source;
                 CREATE TABLE semantic_snapshot_attestations (
                     sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_key TEXT NOT NULL,
                     provider_profile_id TEXT NOT NULL,
                     provider_contract_id TEXT NOT NULL,
                     snapshot_fingerprint TEXT NOT NULL,
                     worktree_fingerprint TEXT NOT NULL,
                     head_revision TEXT NOT NULL,
                     worktree_clean INTEGER NOT NULL CHECK(worktree_clean IN (0, 1)),
                     source_trust TEXT NOT NULL
                         CHECK(source_trust IN ('offline_import', 'trusted_provider')),
                     provider_registration_id TEXT,
                     executable_sha256 TEXT,
                     artifact_sha256 TEXT,
                     created_at_unix_seconds INTEGER NOT NULL,
                     UNIQUE(project_key, provider_profile_id, provider_contract_id,
                            snapshot_fingerprint, worktree_fingerprint, head_revision,
                            worktree_clean)
                 );
                 INSERT INTO semantic_snapshot_attestations
                 SELECT * FROM semantic_snapshot_attestations_v11_source;
                 DROP TABLE semantic_snapshot_attestations_v11_source;
                 CREATE INDEX idx_semantic_attestation_latest
                     ON semantic_snapshot_attestations(
                        project_key, provider_profile_id, provider_contract_id,
                        snapshot_fingerprint, sequence DESC
                     );
                 UPDATE metadata SET value = '10' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(legacy);

        let migrated = BrainStore::open(&database).unwrap();
        assert_eq!(migrated.database_schema_version().unwrap(), 12);
        let refreshed = SemanticSnapshotSource::trusted_provider(
            "d".repeat(64),
            "same-head".to_owned(),
            true,
            "registration-two".to_owned(),
            "e".repeat(64),
            "f".repeat(64),
        );
        migrated
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &refreshed,
            )
            .unwrap();
        let latest = migrated
            .latest_semantic_source_manifest(PROJECT_KEY, "test-main", &semantic_provider().id)
            .unwrap()
            .unwrap();
        assert_eq!(latest.source, refreshed);
        let count: i64 = migrated
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_snapshot_attestations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        drop(migrated);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaying_a_legacy_snapshot_records_manifest_from_real_input() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = semantic_snapshot(
            "semantic-legacy-replay",
            vec![semantic_symbol("stable", "src/lib.rs", "stable-key")],
        );
        apply_semantic(&store, &snapshot);
        store
            .connection
            .execute("DELETE FROM semantic_source_observations", [])
            .unwrap();
        store
            .connection
            .execute("DELETE FROM semantic_source_manifests", [])
            .unwrap();
        assert!(
            !store
                .latest_semantic_source_manifest(PROJECT_KEY, "test-main", &semantic_provider().id,)
                .unwrap()
                .unwrap()
                .recorded
        );

        let replay = store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &semantic_observations(&snapshot),
                &[],
                &SemanticSnapshotSource::offline(
                    "d".repeat(64),
                    "legacy-replay-head".to_owned(),
                    true,
                ),
            )
            .unwrap();
        assert!(!replay.snapshot_inserted);
        let manifest = store
            .latest_semantic_source_manifest(PROJECT_KEY, "test-main", &semantic_provider().id)
            .unwrap()
            .unwrap();
        assert!(manifest.recorded);
        assert_eq!(manifest.sources, snapshot.sources);
    }

    #[test]
    fn semantic_scope_requires_confirmed_lineage_for_rename() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-lineage-1",
            vec![semantic_symbol("before", "src/a.rs", "old-key")],
        );
        let second = semantic_snapshot(
            "semantic-lineage-2",
            vec![semantic_symbol("after", "src/b.rs", "new-key")],
        );
        let anchor = first.symbols[0].id.clone();
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);

        let unresolved = store
            .resolve_semantic_scope(
                PROJECT_KEY,
                "test-main",
                &semantic_provider().id,
                "rust",
                &first.source_revision,
                &anchor,
            )
            .unwrap();
        assert_eq!(unresolved.kind, super::SemanticResolutionKind::Unresolved);

        let candidate = store
            .list_lineage_candidates(PROJECT_KEY, Some(LineageState::Proposed), None, None, 10)
            .unwrap()
            .pop()
            .unwrap();
        let adjudicated = store
            .confirm_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "human-confirm-rename",
                Some("operator@example"),
                Some("verified rename"),
                None,
            )
            .unwrap();
        let resolved = store
            .resolve_semantic_scope(
                PROJECT_KEY,
                "test-main",
                &semantic_provider().id,
                "rust",
                &first.source_revision,
                &anchor,
            )
            .unwrap();
        assert_eq!(
            resolved.kind,
            super::SemanticResolutionKind::ConfirmedLineage
        );
        assert_eq!(resolved.resolved_symbol.unwrap().id, second.symbols[0].id);
        assert_eq!(
            resolved.lineage_decision_ids,
            vec![adjudicated.decision.decision_id]
        );
    }

    #[test]
    fn ambiguous_provider_symbol_cannot_anchor_a_hard_scope() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = semantic_snapshot(
            "semantic-ambiguous",
            vec![
                semantic_symbol("one", "src/a.rs", "one-key"),
                semantic_symbol("two", "src/b.rs", "two-key"),
            ],
        );
        let anchor = snapshot.symbols[0].id.clone();
        apply_semantic(&store, &snapshot);
        let resolved = store
            .resolve_semantic_scope(
                PROJECT_KEY,
                "test-main",
                &semantic_provider().id,
                "rust",
                &snapshot.source_revision,
                &anchor,
            )
            .unwrap();
        assert_eq!(resolved.kind, super::SemanticResolutionKind::Unresolved);
        assert!(resolved.reason.unwrap().contains("不唯一"));
    }

    #[test]
    fn algorithm_upgrade_appends_evidence_without_rewriting_manual_state() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![semantic_symbol("after", "src/b.rs", "stable-new")],
        );
        let before_observations = semantic_observations(&first);
        let after_observations = semantic_observations(&second);
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);
        let candidate = store
            .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
            .unwrap()
            .remove(0);
        store
            .reject_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "request-reject-before-upgrade",
                Some("test-user"),
                Some("manual rejection must survive generator upgrades"),
            )
            .unwrap();

        let mut proposals = brain_symbols::propose_lineage_candidates(
            &before_observations,
            &after_observations,
            &[],
        );
        assert_eq!(proposals.len(), 1);
        proposals[0].algorithm_version = "3".to_owned();
        let transaction = store.connection.unchecked_transaction().unwrap();
        let persisted = super::persist_lineage_proposals(
            &transaction,
            &proposals,
            super::unix_seconds().unwrap(),
        )
        .unwrap();
        transaction.commit().unwrap();

        assert_eq!(persisted, (0, 1));
        let updated = store
            .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
            .unwrap()
            .remove(0);
        assert_eq!(updated.candidate_id, candidate.candidate_id);
        assert_eq!(updated.state, LineageState::Rejected);
        assert_eq!(updated.evidence_count, 2);
    }

    #[test]
    fn unchanged_semantic_symbols_do_not_create_self_lineage() {
        let store = BrainStore::open_in_memory().unwrap();
        let stable = semantic_symbol("stable", "src/lib.rs", "stable-key");
        let first = semantic_snapshot("semantic-1", vec![stable.clone()]);
        let second = semantic_snapshot("semantic-2", vec![stable]);
        apply_semantic(&store, &first);
        let result = apply_semantic(&store, &second);
        assert_eq!(result.candidates_inserted, 0);
        assert!(
            store
                .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn explicit_lineage_decisions_are_append_only_and_idempotent() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![semantic_symbol("after", "src/b.rs", "stable-new")],
        );
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);
        let candidate = store
            .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
            .unwrap()
            .remove(0);

        let rejected = store
            .reject_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "request-reject",
                Some("test-user"),
                Some("not the same symbol"),
            )
            .unwrap();
        assert_eq!(rejected.candidate.state, LineageState::Rejected);
        let confirmed = store
            .confirm_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "request-confirm",
                Some("test-user"),
                Some("reconsidered with explicit evidence"),
                None,
            )
            .unwrap();
        assert_eq!(confirmed.candidate.state, LineageState::Confirmed);
        let replay = store
            .confirm_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "request-confirm",
                Some("test-user"),
                Some("reconsidered with explicit evidence"),
                None,
            )
            .unwrap();
        assert!(replay.replayed);
        assert!(matches!(
            store.reject_lineage(
                PROJECT_KEY,
                &candidate.candidate_id,
                "request-confirm",
                Some("test-user"),
                Some("different payload"),
            ),
            Err(StoreError::LineageIdempotencyConflict(_))
        ));
        let decision_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_decisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decision_count, 2);

        let third = semantic_snapshot(
            "semantic-3",
            vec![semantic_symbol("latest", "src/c.rs", "stable-latest")],
        );
        apply_semantic(&store, &third);
        let old = store
            .list_lineage_candidates(PROJECT_KEY, None, Some("semantic-2"), None, 10)
            .unwrap()
            .into_iter()
            .find(|item| item.candidate_id == candidate.candidate_id)
            .unwrap();
        assert_eq!(old.state, LineageState::Confirmed);
    }

    #[test]
    fn competing_confirmations_require_atomic_explicit_supersede() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![
                semantic_symbol("after_a", "src/b.rs", "stable-new-a"),
                semantic_symbol("after_b", "src/c.rs", "stable-new-b"),
            ],
        );
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);
        let candidates = materialize_all_group_pairs(&store);
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.ambiguity_group_id.is_none())
        );
        let first_id = &candidates[0].candidate_id;
        let second_id = &candidates[1].candidate_id;
        store
            .confirm_lineage(PROJECT_KEY, first_id, "confirm-a", None, None, None)
            .unwrap();
        assert!(matches!(
            store.confirm_lineage(PROJECT_KEY, second_id, "confirm-b", None, None, None),
            Err(StoreError::LineageConflict(_))
        ));
        let replacement = store
            .confirm_lineage(
                PROJECT_KEY,
                second_id,
                "supersede-a-with-b",
                Some("test-user"),
                Some("selected the other successor"),
                Some(first_id),
            )
            .unwrap();
        assert_eq!(replacement.candidate.state, LineageState::Confirmed);
        assert_eq!(
            replacement.superseded_candidate.unwrap().state,
            LineageState::Superseded
        );
        assert!(matches!(
            store.confirm_lineage(
                "project_other",
                second_id,
                "cross-project",
                None,
                None,
                None,
            ),
            Err(StoreError::LineageConflict(_))
        ));
    }

    #[test]
    fn ambiguous_group_stays_linear_and_requires_explicit_pair_materialization() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![
                semantic_symbol("after_a", "src/b.rs", "stable-new-a"),
                semantic_symbol("after_b", "src/c.rs", "stable-new-b"),
            ],
        );
        apply_semantic(&store, &first);
        let applied = apply_semantic(&store, &second);

        assert_eq!(applied.lineage_groups_inserted, 1);
        assert_eq!(applied.lineage_group_members_inserted, 3);
        assert_eq!(applied.potential_lineage_pairs, 2);
        assert_eq!(applied.candidates_inserted, 0);
        let group = store
            .list_lineage_groups(PROJECT_KEY, 10)
            .unwrap()
            .remove(0);
        let detail = store
            .lineage_group(PROJECT_KEY, &group.group_id)
            .unwrap()
            .unwrap();
        assert_eq!(detail.from_members.len(), 1);
        assert_eq!(detail.to_members.len(), 2);
        assert!(matches!(
            store.materialize_lineage_group_pair(
                PROJECT_KEY,
                &group.group_id,
                "not-a-member",
                &detail.to_members[0],
            ),
            Err(StoreError::InvalidLineage(_))
        ));
        let first_candidate = store
            .materialize_lineage_group_pair(
                PROJECT_KEY,
                &group.group_id,
                &detail.from_members[0],
                &detail.to_members[0],
            )
            .unwrap();
        let replay = store
            .materialize_lineage_group_pair(
                PROJECT_KEY,
                &group.group_id,
                &detail.from_members[0],
                &detail.to_members[0],
            )
            .unwrap();
        assert_eq!(first_candidate.candidate_id, replay.candidate_id);
        assert_eq!(replay.state, LineageState::Proposed);
    }

    #[test]
    fn legacy_compaction_preview_is_read_only_and_protects_incomplete_groups() {
        let store = BrainStore::open_in_memory().unwrap();
        insert_legacy_cartesian_fixture(&store, false);

        let before_candidates: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_candidates",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let report = store
            .preview_legacy_lineage_compaction(PROJECT_KEY)
            .unwrap();
        assert_eq!(report.mode, "dry_run");
        assert!(!report.applied);
        assert_eq!(report.legacy_ambiguous_candidate_count, 3);
        assert_eq!(report.compactable_group_count, 0);
        assert_eq!(report.protected_candidate_count, 3);
        assert_eq!(report.groups, Vec::new());
        let after_candidates: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_candidates",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before_candidates, after_candidates);
    }

    #[test]
    fn legacy_compaction_protects_a_whole_group_with_additional_evidence() {
        let store = BrainStore::open_in_memory().unwrap();
        insert_legacy_cartesian_fixture(&store, true);
        store
            .connection
            .execute(
                "INSERT INTO semantic_lineage_evidence(
                     evidence_id, candidate_id, algorithm_id, algorithm_version,
                     evidence_schema_version, input_fingerprint, confidence_band,
                     evidence_json, evidence_hash, created_at_unix_seconds
                 ) VALUES(
                     'extra-evidence', 'legacy-candidate-1', 'manual-observation', '1',
                     1, ?1, 'low', '[]', ?2, 2
                 )",
                params![
                    format!("sha256_{}", "b".repeat(64)),
                    format!("sha256_{}", "c".repeat(64))
                ],
            )
            .unwrap();

        let report = store
            .preview_legacy_lineage_compaction(PROJECT_KEY)
            .unwrap();
        assert_eq!(report.compactable_group_count, 0);
        assert_eq!(report.protected_candidate_count, 4);
    }

    #[test]
    fn legacy_compaction_is_audited_atomic_and_idempotent() {
        let store = BrainStore::open_in_memory().unwrap();
        insert_legacy_cartesian_fixture(&store, true);

        let preview = store
            .preview_legacy_lineage_compaction(PROJECT_KEY)
            .unwrap();
        assert_eq!(preview.compactable_group_count, 1);
        assert_eq!(preview.compactable_candidate_count, 4);
        assert_eq!(preview.compactable_evidence_count, 4);
        assert_eq!(preview.protected_candidate_count, 0);
        assert_eq!(preview.group_member_count, 4);
        let applied = store
            .apply_legacy_lineage_compaction(PROJECT_KEY, "compact-request-1")
            .unwrap();
        assert!(applied.applied);
        assert!(!applied.replayed);
        assert_eq!(
            applied.compaction_manifest_hash,
            preview.compaction_manifest_hash
        );
        let remaining_legacy: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_candidates
                 WHERE proposal_origin = 'legacy_v7'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_legacy, 0);
        let remaining_evidence: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_evidence",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_evidence, 0);
        let audit_groups: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM semantic_lineage_compaction_groups",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_groups, 1);
        let replay = store
            .apply_legacy_lineage_compaction(PROJECT_KEY, "compact-request-1")
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            replay.compaction_manifest_hash,
            applied.compaction_manifest_hash
        );
        let group_id = &applied.groups[0].group_id;
        let detail = store.lineage_group(PROJECT_KEY, group_id).unwrap().unwrap();
        assert_eq!(detail.from_members.len(), 2);
        assert_eq!(detail.to_members.len(), 2);
        assert!(matches!(
            store.materialize_lineage_group_pair(
                PROJECT_KEY,
                group_id,
                &detail.from_members[0],
                &detail.to_members[0],
            ),
            Err(StoreError::InvalidLineage(_))
        ));
    }

    #[test]
    fn v4_migration_preserves_project_scoped_graph_and_adds_lineage_ledger() {
        let (root, database) = temporary_database("v4-lineage-migration");
        let store = BrainStore::open(&database).unwrap();
        store
            .apply_symbol_snapshot(&snapshot("rev-1", vec![symbol("preserved")]))
            .unwrap();
        store
            .connection
            .execute_batch(
                "DROP TABLE semantic_lineage_decisions;
                 DROP TABLE semantic_lineage_evidence;
                 DROP TABLE semantic_lineage_candidates;
                 DROP TABLE semantic_symbol_observations;
                 DROP TABLE semantic_snapshots;
                 UPDATE metadata SET value = '4' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(store);

        let migrated = BrainStore::open(&database).unwrap();
        assert_eq!(migrated.database_schema_version().unwrap(), 12);
        assert_eq!(
            migrated
                .list_symbols(PROJECT_KEY, None, false, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            migrated
                .list_lineage_candidates(PROJECT_KEY, None, None, None, 10)
                .unwrap()
                .is_empty()
        );
        drop(migrated);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_competing_confirmations_have_one_winner() {
        let (root, database) = temporary_database("concurrent-lineage-confirm");
        let store = BrainStore::open(&database).unwrap();
        let first = semantic_snapshot(
            "semantic-1",
            vec![semantic_symbol("before", "src/a.rs", "stable-old")],
        );
        let second = semantic_snapshot(
            "semantic-2",
            vec![
                semantic_symbol("after_a", "src/b.rs", "stable-new-a"),
                semantic_symbol("after_b", "src/c.rs", "stable-new-b"),
            ],
        );
        apply_semantic(&store, &first);
        apply_semantic(&store, &second);
        let ids = materialize_all_group_pairs(&store)
            .into_iter()
            .map(|candidate| candidate.candidate_id)
            .collect::<Vec<_>>();
        drop(store);

        let barrier = Arc::new(Barrier::new(3));
        let handles = ids
            .into_iter()
            .enumerate()
            .map(|(index, candidate_id)| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let store = BrainStore::open(&database).unwrap();
                    barrier.wait();
                    store.confirm_lineage(
                        PROJECT_KEY,
                        &candidate_id,
                        &format!("concurrent-{index}"),
                        None,
                        None,
                        None,
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().is_ok())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        let store = BrainStore::open(&database).unwrap();
        assert_eq!(
            store
                .list_lineage_candidates(
                    PROJECT_KEY,
                    Some(LineageState::Confirmed),
                    None,
                    None,
                    10,
                )
                .unwrap()
                .len(),
            1
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
