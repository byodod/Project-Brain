use std::{
    path::Path,
    time::{SystemTime, SystemTimeError, UNIX_EPOCH},
};

use brain_core::{ActionDescriptor, Decision};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DATABASE_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite 操作失败：{0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON 序列化失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error("系统时间无效：{0}")]
    Clock(#[from] SystemTimeError),
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
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
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
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [DATABASE_SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use brain_core::{
        ActionDescriptor, ActionKind, CURRENT_SCHEMA_VERSION, Decision, DecisionKind,
    };

    use super::BrainStore;

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
}
