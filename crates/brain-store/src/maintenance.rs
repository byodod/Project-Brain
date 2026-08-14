use std::path::Path;

use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DATABASE_SCHEMA_VERSION, DatabaseStorageStats, StoreError};

#[derive(Debug)]
struct TableColumn {
    name: String,
    primary_key_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatabaseLogicalVerification {
    pub schema_version: i64,
    pub logical_manifest_sha256: String,
    pub schema_object_count: u64,
    pub table_count: u64,
    pub row_count: u64,
    pub quick_check: String,
    pub integrity_check: Option<String>,
    pub foreign_key_violation_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalCheckpointReport {
    pub busy: u64,
    pub log_frames: Option<u64>,
    pub checkpointed_frames: Option<u64>,
}

/// 严格只读地检查数据库页面占用和关键 ledger 行数；不会初始化或迁移 schema。
///
/// # Errors
///
/// 当数据库不可读、schema 不是当前版本或统计无效时返回错误。
pub fn inspect_database_storage(path: &Path) -> Result<DatabaseStorageStats, StoreError> {
    let connection = open_read_only(path)?;
    connection.execute_batch("BEGIN DEFERRED TRANSACTION")?;
    let result = storage_stats(&connection);
    finish_read_transaction(&connection, result.is_ok())?;
    result
}

/// 对 schema 与全部表内容生成顺序稳定的逻辑清单，并执行完整性验证。
///
/// # Errors
///
/// 当数据库不可读、schema 不受支持、完整性查询失败或计数溢出时返回错误。
pub fn inspect_database_logical_content(
    path: &Path,
    full_check: bool,
) -> Result<DatabaseLogicalVerification, StoreError> {
    let connection = open_read_only(path)?;
    connection.execute_batch("BEGIN DEFERRED TRANSACTION")?;
    let result = logical_verification(&connection, full_check);
    finish_read_transaction(&connection, result.is_ok())?;
    result
}

/// 在 WAL 完整 checkpoint 后，以 `SQLite` `VACUUM INTO` 生成同目录候选数据库。
/// 调用者必须持有项目级独占维护锁，并保证 `target` 不存在。
///
/// # Errors
///
/// 当 checkpoint 被占用、目标已存在、路径不是 UTF-8 或 `SQLite` 操作失败时返回错误。
pub fn vacuum_database_into(
    source: &Path,
    target: &Path,
) -> Result<WalCheckpointReport, StoreError> {
    if target.exists() {
        return Err(StoreError::Integrity(format!(
            "VACUUM INTO 目标已存在：{}",
            target.display()
        )));
    }
    let target_text = target.to_str().ok_or_else(|| {
        StoreError::Integrity(format!(
            "VACUUM INTO 目标不是有效 UTF-8：{}",
            target.display()
        ))
    })?;
    let report = checkpoint_database_wal(source)?;
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute("VACUUM INTO ?1", params![target_text])?;
    Ok(report)
}

/// 将 WAL 完整 checkpoint 并截断；若存在忙碌 reader/writer 则失败关闭。
///
/// # Errors
///
/// 当数据库不可写、checkpoint 被占用或统计值无效时返回错误。
pub fn checkpoint_database_wal(source: &Path) -> Result<WalCheckpointReport, StoreError> {
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let checkpoint = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let report = WalCheckpointReport {
        busy: non_negative(checkpoint.0, "wal_checkpoint.busy")?,
        log_frames: non_negative_or_unavailable(checkpoint.1, "wal_checkpoint.log_frames")?,
        checkpointed_frames: non_negative_or_unavailable(
            checkpoint.2,
            "wal_checkpoint.checkpointed_frames",
        )?,
    };
    if report.busy != 0 {
        return Err(StoreError::Integrity(format!(
            "WAL checkpoint 被占用，拒绝压缩：busy={} log_frames={:?} checkpointed_frames={:?}",
            report.busy, report.log_frames, report.checkpointed_frames
        )));
    }
    Ok(report)
}

pub(super) fn storage_stats(connection: &Connection) -> Result<DatabaseStorageStats, StoreError> {
    require_current_schema(connection)?;
    let page_size_bytes = read_non_negative(connection, "PRAGMA page_size", "page_size")?;
    let page_count = read_non_negative(connection, "PRAGMA page_count", "page_count")?;
    let freelist_page_count =
        read_non_negative(connection, "PRAGMA freelist_count", "freelist_count")?;
    let database_bytes = page_size_bytes
        .checked_mul(page_count)
        .ok_or_else(|| StoreError::Integrity("数据库页面字节计数溢出".to_owned()))?;
    let reclaimable_bytes = page_size_bytes
        .checked_mul(freelist_page_count)
        .ok_or_else(|| StoreError::Integrity("数据库可回收字节计数溢出".to_owned()))?;
    let reclaimable_basis_points = if database_bytes == 0 {
        0
    } else {
        u16::try_from(
            u128::from(reclaimable_bytes)
                .checked_mul(10_000)
                .ok_or_else(|| StoreError::Integrity("可回收比例计数溢出".to_owned()))?
                / u128::from(database_bytes),
        )
        .map_err(|_| StoreError::Integrity("可回收比例超出 10000 basis points".to_owned()))?
    };
    let journal_mode = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let quick_check = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    Ok(DatabaseStorageStats {
        schema_version: DATABASE_SCHEMA_VERSION,
        page_size_bytes,
        page_count,
        freelist_page_count,
        database_bytes,
        reclaimable_bytes,
        reclaimable_basis_points,
        journal_mode,
        quick_check,
        foreign_key_violation_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM pragma_foreign_key_check",
            "foreign_key_violation_count",
        )?,
        lineage_candidate_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM semantic_lineage_candidates",
            "lineage_candidate_count",
        )?,
        lineage_evidence_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM semantic_lineage_evidence",
            "lineage_evidence_count",
        )?,
        lineage_group_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM semantic_lineage_groups",
            "lineage_group_count",
        )?,
        lineage_group_member_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM semantic_lineage_group_members",
            "lineage_group_member_count",
        )?,
        lineage_materialization_request_count: read_non_negative(
            connection,
            "SELECT COUNT(*) FROM semantic_lineage_materialization_requests",
            "lineage_materialization_request_count",
        )?,
    })
}

fn open_read_only(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(1))?;
    connection.execute_batch("PRAGMA query_only = ON; PRAGMA foreign_keys = ON;")?;
    Ok(connection)
}

fn finish_read_transaction(connection: &Connection, commit: bool) -> Result<(), StoreError> {
    connection.execute_batch(if commit { "COMMIT" } else { "ROLLBACK" })?;
    Ok(())
}

fn require_current_schema(connection: &Connection) -> Result<(), StoreError> {
    let actual = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )?
        .parse::<i64>()
        .map_err(|_| StoreError::Integrity("schema_version 不是整数".to_owned()))?;
    if actual != DATABASE_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            actual,
            expected: DATABASE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "完整逻辑清单必须在同一读取事务语义中覆盖 schema、表、行与完整性门禁"
)]
fn logical_verification(
    connection: &Connection,
    full_check: bool,
) -> Result<DatabaseLogicalVerification, StoreError> {
    require_current_schema(connection)?;
    let quick_check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(StoreError::Integrity(format!(
            "PRAGMA quick_check 返回：{quick_check}"
        )));
    }
    let integrity_check = if full_check {
        let result: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            return Err(StoreError::Integrity(format!(
                "PRAGMA integrity_check 返回：{result}"
            )));
        }
        Some(result)
    } else {
        None
    };
    let foreign_key_violation_count = read_non_negative(
        connection,
        "SELECT COUNT(*) FROM pragma_foreign_key_check",
        "foreign_key_violation_count",
    )?;
    if foreign_key_violation_count != 0 {
        return Err(StoreError::Integrity(format!(
            "foreign_key_check 发现 {foreign_key_violation_count} 条违规"
        )));
    }

    let mut hasher = Sha256::new();
    let mut schema_object_count = 0_u64;
    let mut schema_statement = connection.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema
         ORDER BY type, name, tbl_name, COALESCE(sql, '')",
    )?;
    let mut schema_rows = schema_statement.query([])?;
    while let Some(row) = schema_rows.next()? {
        hash_field(&mut hasher, row.get_ref(0)?);
        hash_field(&mut hasher, row.get_ref(1)?);
        hash_field(&mut hasher, row.get_ref(2)?);
        hash_field(&mut hasher, row.get_ref(3)?);
        hasher.update([0xFE]);
        schema_object_count = schema_object_count
            .checked_add(1)
            .ok_or_else(|| StoreError::Integrity("schema 对象计数溢出".to_owned()))?;
    }
    drop(schema_rows);
    drop(schema_statement);

    let mut tables = Vec::new();
    let mut table_statement = connection.prepare(
        "SELECT name FROM sqlite_schema
         WHERE type = 'table'
           AND (name NOT LIKE 'sqlite_%' OR name = 'sqlite_sequence')
         ORDER BY name",
    )?;
    let names = table_statement.query_map([], |row| row.get::<_, String>(0))?;
    for name in names {
        tables.push(name?);
    }
    drop(table_statement);

    let mut row_count = 0_u64;
    for table in &tables {
        hash_bytes(&mut hasher, table.as_bytes());
        let columns = visible_columns(connection, table)?;
        if columns.is_empty() {
            return Err(StoreError::Integrity(format!(
                "表 {table} 没有可读取列，无法生成逻辑清单"
            )));
        }
        let projection = columns
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        let mut primary_key = columns
            .iter()
            .filter(|column| column.primary_key_order != 0)
            .collect::<Vec<_>>();
        primary_key.sort_by_key(|column| column.primary_key_order);
        let ordering = if primary_key.is_empty() {
            projection.clone()
        } else {
            primary_key
                .iter()
                .map(|column| quote_identifier(&column.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let sql = format!(
            "SELECT {projection} FROM {} ORDER BY {ordering}",
            quote_identifier(table)
        );
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for index in 0..columns.len() {
                hash_field(&mut hasher, row.get_ref(index)?);
            }
            hasher.update([0xFF]);
            row_count = row_count
                .checked_add(1)
                .ok_or_else(|| StoreError::Integrity("数据库逻辑行计数溢出".to_owned()))?;
        }
    }

    Ok(DatabaseLogicalVerification {
        schema_version: DATABASE_SCHEMA_VERSION,
        logical_manifest_sha256: format!("{:x}", hasher.finalize()),
        schema_object_count,
        table_count: u64::try_from(tables.len())
            .map_err(|_| StoreError::Integrity("数据库表计数溢出".to_owned()))?,
        row_count,
        quick_check,
        integrity_check,
        foreign_key_violation_count,
    })
}

fn visible_columns(connection: &Connection, table: &str) -> Result<Vec<TableColumn>, StoreError> {
    let mut statement = connection
        .prepare("SELECT name, pk FROM pragma_table_xinfo(?1) WHERE hidden = 0 ORDER BY cid")?;
    let columns = statement.query_map(params![table], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut output = Vec::new();
    for column in columns {
        let (name, primary_key_order) = column?;
        output.push(TableColumn {
            name,
            primary_key_order: u32::try_from(primary_key_order).map_err(|_| {
                StoreError::Integrity(format!(
                    "表 {table} 的 primary key 次序为负数：{primary_key_order}"
                ))
            })?,
        });
    }
    Ok(output)
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn hash_field(hasher: &mut Sha256, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => hasher.update([0]),
        ValueRef::Integer(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hasher.update([2]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            hasher.update([3]);
            hash_bytes(hasher, value);
        }
        ValueRef::Blob(value) => {
            hasher.update([4]);
            hash_bytes(hasher, value);
        }
    }
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn read_non_negative(connection: &Connection, sql: &str, label: &str) -> Result<u64, StoreError> {
    let value = connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
    non_negative(value, label)
}

fn non_negative(value: i64, label: &str) -> Result<u64, StoreError> {
    u64::try_from(value)
        .map_err(|_| StoreError::Integrity(format!("数据库统计 {label} 返回负数：{value}")))
}

fn non_negative_or_unavailable(value: i64, label: &str) -> Result<Option<u64>, StoreError> {
    if value == -1 {
        Ok(None)
    } else {
        non_negative(value, label).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::{inspect_database_logical_content, inspect_database_storage, vacuum_database_into};
    use crate::BrainStore;

    #[test]
    fn read_only_inspection_and_vacuum_preserve_the_complete_logical_manifest() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-database-maintenance-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("brain.db");
        let target = root.join("brain.compact.db");
        let store = BrainStore::open(&source).unwrap();
        for index in 0..1_000 {
            store
                .connection
                .execute(
                    "INSERT INTO audit_events(
                         event_id, session_id, hook_event, action_json, decision_json,
                         created_at_unix_seconds
                     ) VALUES (?1, 'session', 'test', ?2, '{}', 1)",
                    rusqlite::params![format!("event-{index}"), "x".repeat(1_024)],
                )
                .unwrap();
        }
        store
            .connection
            .execute("DELETE FROM audit_events WHERE id % 2 = 0", [])
            .unwrap();
        drop(store);

        let before_file = fs::read(&source).unwrap();
        let before_stats = inspect_database_storage(&source).unwrap();
        let before = inspect_database_logical_content(&source, true).unwrap();
        assert_eq!(fs::read(&source).unwrap(), before_file);

        let checkpoint = vacuum_database_into(&source, &target).unwrap();
        assert_eq!(checkpoint.busy, 0);
        let after = inspect_database_logical_content(&target, true).unwrap();
        let after_stats = inspect_database_storage(&target).unwrap();
        assert_eq!(
            before.logical_manifest_sha256,
            after.logical_manifest_sha256
        );
        assert_eq!(before.row_count, after.row_count);
        assert!(after_stats.database_bytes <= before_stats.database_bytes);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wal_checkpoint_fails_closed_while_a_writer_holds_the_database() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-wal-busy-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = root.join("brain.db");
        BrainStore::open(&database).unwrap();
        let writer = rusqlite::Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "BEGIN IMMEDIATE;
                 INSERT INTO audit_events(
                     event_id, session_id, hook_event, action_json, decision_json,
                     created_at_unix_seconds
                 ) VALUES ('busy-event', 'session', 'test', '{}', '{}', 1);",
            )
            .unwrap();

        assert!(super::checkpoint_database_wal(&database).is_err());
        writer.execute_batch("ROLLBACK").unwrap();
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }
}
