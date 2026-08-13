use std::{
    path::Path,
    time::{SystemTime, SystemTimeError, UNIX_EPOCH},
};

use brain_core::{
    ActionDescriptor, Decision, HOOK_PROTOCOL_VERSION, InternalHookEvent, InternalHookOutcome,
};
use brain_symbols::{
    GraphDelta, IdentityQuality, SYMBOL_PROTOCOL_VERSION, SourceLanguage, SymbolNode,
    SymbolSnapshot, SymbolStatus, symbol_id,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DATABASE_SCHEMA_VERSION: i64 = 3;

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
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbol_nodes (
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
                 status TEXT NOT NULL CHECK(status IN ('active', 'removed')),
                 first_seen_revision TEXT NOT NULL,
                 last_seen_revision TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_symbol_provider_key
                 ON symbol_nodes(provider_id, provider_key);
             CREATE INDEX IF NOT EXISTS idx_symbol_path_status
                 ON symbol_nodes(path, status, start_line);
             CREATE TABLE IF NOT EXISTS symbol_edges (
                 provider_id TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 target_id TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('active', 'removed')),
                 first_seen_revision TEXT NOT NULL,
                 last_seen_revision TEXT NOT NULL,
                 PRIMARY KEY(provider_id, source_id, target_id, kind),
                 FOREIGN KEY(source_id) REFERENCES symbol_nodes(id),
                 FOREIGN KEY(target_id) REFERENCES symbol_nodes(id)
             );
             CREATE INDEX IF NOT EXISTS idx_symbol_edges_source
                 ON symbol_edges(source_id, status, kind);
             CREATE INDEX IF NOT EXISTS idx_symbol_edges_target
                 ON symbol_edges(target_id, status, kind);",
        )?;
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

    /// 查询当前或历史符号。路径过滤使用项目相对路径边界，而不是任意字符串前缀。
    ///
    /// # Errors
    ///
    /// `SQLite` 查询失败或持久化枚举字段无效时返回错误。
    pub fn list_symbols(
        &self,
        path: Option<&str>,
        include_removed: bool,
        limit: u32,
    ) -> Result<Vec<SymbolNode>, StoreError> {
        let path = path.unwrap_or_default();
        let path_pattern = escape_like_pattern(path);
        let mut statement = self.connection.prepare(
            "SELECT id, provider_id, identity_quality, language, kind, provider_key,
                    display_name, path, start_line, end_line, content_fingerprint, status
             FROM symbol_nodes
             WHERE (?1 = 1 OR status = 'active')
               AND (?2 = '' OR path = ?2 OR path LIKE ?3 || '/%' ESCAPE '!')
             ORDER BY path, start_line, end_line, id
             LIMIT ?4",
        )?;
        statement
            .query_map(
                params![include_removed, path, path_pattern, limit],
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
    if snapshot.provider.id.trim().is_empty()
        || snapshot.provider.version.trim().is_empty()
        || snapshot.source_revision.trim().is_empty()
    {
        return Err(StoreError::InvalidSnapshot(
            "provider.id、provider.version 与 source_revision 不能为空".to_owned(),
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
        if symbol.provider_id != snapshot.provider.id
            || symbol.identity_quality != snapshot.provider.identity_quality
            || symbol.status != SymbolStatus::Active
            || symbol.id != symbol_id(&symbol.provider_id, &symbol.provider_key)
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
        if edge.provider_id != snapshot.provider.id
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

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256_")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
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
    let mut delta = GraphDelta::default();
    let existing_active = {
        let mut statement = transaction
            .prepare("SELECT id FROM symbol_nodes WHERE provider_id = ?1 AND status = 'active'")?;
        statement
            .query_map([&snapshot.provider.id], |row| row.get::<_, String>(0))?
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
                "SELECT id, provider_id, identity_quality, language, kind, provider_key,
                        display_name, path, start_line, end_line, content_fingerprint, status
                 FROM symbol_nodes WHERE id = ?1",
                [&symbol.id],
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
                 id, provider_id, identity_quality, language, kind, provider_key,
                 display_name, path, start_line, end_line, content_fingerprint, status,
                 first_seen_revision, last_seen_revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', ?12, ?12)
             ON CONFLICT(id) DO UPDATE SET
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
            "UPDATE symbol_nodes SET status = 'removed', last_seen_revision = ?2
             WHERE id = ?1",
            params![removed_id, snapshot.source_revision],
        )?;
        delta.removed += 1;
    }

    transaction.execute(
        "UPDATE symbol_edges SET status = 'removed', last_seen_revision = ?2
         WHERE provider_id = ?1 AND status = 'active'",
        params![snapshot.provider.id, snapshot.source_revision],
    )?;
    for edge in &snapshot.edges {
        transaction.execute(
            "INSERT INTO symbol_edges(
                 provider_id, source_id, target_id, kind, status,
                 first_seen_revision, last_seen_revision
             ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)
             ON CONFLICT(provider_id, source_id, target_id, kind) DO UPDATE SET
                 status = 'active', last_seen_revision = excluded.last_seen_revision",
            params![
                edge.provider_id,
                edge.source_id,
                edge.target_id,
                edge.kind.as_str(),
                snapshot.source_revision,
            ],
        )?;
    }
    Ok(delta)
}

fn decode_symbol_row(row: &rusqlite::Row<'_>) -> Result<SymbolNode, rusqlite::Error> {
    let quality: String = row.get(2)?;
    let language: String = row.get(3)?;
    let status: String = row.get(11)?;
    let identity_quality = IdentityQuality::parse(&quality).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "identity_quality",
                value: quality,
            }),
        )
    })?;
    let language = SourceLanguage::parse(&language).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "language",
                value: language,
            }),
        )
    })?;
    let status = SymbolStatus::parse(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(StoreError::InvalidSymbolField {
                field: "status",
                value: status,
            }),
        )
    })?;
    let start_line: i64 = row.get(8)?;
    let end_line: i64 = row.get(9)?;
    Ok(SymbolNode {
        id: row.get(0)?,
        provider_id: row.get(1)?,
        identity_quality,
        language,
        kind: row.get(4)?,
        provider_key: row.get(5)?,
        display_name: row.get(6)?,
        path: row.get(7)?,
        start_line: usize::try_from(start_line).unwrap_or_default(),
        end_line: usize::try_from(end_line).unwrap_or_default(),
        content_fingerprint: row.get(10)?,
        status,
    })
}

fn same_symbol_observation(left: &SymbolNode, right: &SymbolNode) -> bool {
    left.id == right.id
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

    use brain_symbols::{
        GraphDelta, IdentityQuality, ProviderDescriptor, SYMBOL_PROTOCOL_VERSION, SourceFileState,
        SourceLanguage, SymbolNode, SymbolNodeInput, SymbolSnapshot, SymbolStatus,
        encode_provider_key,
    };
    use rusqlite::Connection;

    use super::{AdapterRecordResult, BrainStore, StoreError};

    fn provider() -> ProviderDescriptor {
        ProviderDescriptor {
            id: "test-syntax".to_owned(),
            version: "1".to_owned(),
            identity_quality: IdentityQuality::SyntaxFallback,
        }
    }

    fn symbol(name: &str) -> SymbolNode {
        let provider_key = encode_provider_key(&["src/lib.rs", "function_item", name, "0"]);
        let content = format!("fn {name}() {{}}");
        SymbolNode::from_provider_key(
            &provider(),
            SymbolNodeInput {
                language: SourceLanguage::Rust,
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
        let sources = symbols
            .iter()
            .map(|symbol| (symbol.path.clone(), symbol.language))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(path, language)| {
                SourceFileState::from_source(&path, language, b"test source", false)
            })
            .collect();
        SymbolSnapshot {
            protocol_version: SYMBOL_PROTOCOL_VERSION,
            provider: provider(),
            source_revision: revision.to_owned(),
            sources,
            symbols,
            edges: Vec::new(),
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
        assert_eq!(store.list_symbols(None, false, 100).unwrap().len(), 1);
        let history = store.list_symbols(None, true, 100).unwrap();
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
        assert_eq!(store.database_schema_version().unwrap(), 3);
        assert!(store.list_symbols(None, false, 10).unwrap().is_empty());
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
    fn path_queries_treat_like_wildcards_as_literal_characters() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut unusual = symbol("unusual");
        unusual.path = "src/a%/lib.rs".to_owned();
        store
            .apply_symbol_snapshot(&snapshot("rev-1", vec![unusual]))
            .unwrap();
        assert_eq!(
            store.list_symbols(Some("src/a%"), false, 10).unwrap().len(),
            1
        );
        assert!(
            store
                .list_symbols(Some("src/a_"), false, 10)
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
        assert!(store.list_symbols(None, true, 10).unwrap().is_empty());
    }
}
