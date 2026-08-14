use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use atomic_write_file::AtomicWriteFile;
use brain_store::{
    DatabaseLogicalVerification, DatabaseStorageStats, WalCheckpointReport,
    checkpoint_database_wal, inspect_database_logical_content, inspect_database_storage,
    vacuum_database_into,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{error::AppError, setup::pretty_json_bytes};

const OPERATION_PROTOCOL_VERSION: u32 = 1;
const MAX_LOCK_TIMEOUT_SECONDS: u64 = 300;
const DISK_MARGIN_NUMERATOR: u64 = 6;
const DISK_MARGIN_DENOMINATOR: u64 = 5;
const EXTERNAL_WRITER_PROTECTION: &str = "cooperative_only";
const REPLACEMENT_DURABILITY: &str =
    "temp_file_synced_atomic_replace;power_loss_directory_durability_platform_dependent";
const SWAP_RETRY_DELAYS_MS: [u64; 5] = [50, 100, 250, 500, 1_000];

#[derive(Debug)]
pub(crate) struct DatabaseAccessLock {
    _file: File,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "这些布尔值是独立的显式 CLI 安全开关，不是互斥状态"
)]
pub(crate) struct DatabaseCompactOptions {
    pub apply: bool,
    pub request_id: Option<String>,
    pub human_confirmed: bool,
    pub full_check: bool,
    pub keep_backup: bool,
    pub lock_timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DatabaseCompactReport {
    pub operation_protocol_version: u32,
    pub project_key: String,
    pub mode: String,
    pub applied: bool,
    pub replayed: bool,
    pub external_writer_protection: String,
    pub replacement_durability: String,
    pub request_id: Option<String>,
    pub parameters_sha256: Option<String>,
    pub source: DatabaseStorageStats,
    pub target: Option<DatabaseStorageStats>,
    pub source_logical_manifest_sha256: Option<String>,
    pub target_logical_manifest_sha256: Option<String>,
    pub current_logical_manifest_sha256: Option<String>,
    pub source_file_sha256: Option<String>,
    pub target_file_sha256: Option<String>,
    pub current_file_sha256: Option<String>,
    pub completed_logical_target_still_current: Option<bool>,
    pub completed_file_target_still_current: Option<bool>,
    pub source_bytes: u64,
    pub target_bytes: Option<u64>,
    pub bytes_reclaimed: Option<u64>,
    pub estimated_required_free_space_bytes: u64,
    pub available_space_bytes: u64,
    pub wal_bytes_at_preflight: u64,
    pub keep_backup: bool,
    pub backup_path: Option<String>,
    pub wal_checkpoint: Option<WalCheckpointReport>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OperationState {
    Running,
    Verified,
    Swapped,
    Completed,
    Failed,
}

impl OperationState {
    const fn requires_recovery(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DatabaseCompactJournal {
    operation_protocol_version: u32,
    project_key: String,
    request_id: String,
    request_sha256: String,
    parameters_sha256: String,
    external_writer_protection: String,
    replacement_durability: String,
    state: OperationState,
    database_file_name: String,
    vacuum_file_name: String,
    backup_file_name: Option<String>,
    source: DatabaseStorageStats,
    source_file_bytes: u64,
    source_verification: DatabaseLogicalVerification,
    source_file_sha256: String,
    target: Option<DatabaseStorageStats>,
    target_verification: Option<DatabaseLogicalVerification>,
    target_file_sha256: Option<String>,
    wal_checkpoint: WalCheckpointReport,
    estimated_required_free_space_bytes: u64,
    available_space_bytes: u64,
    wal_bytes_at_preflight: u64,
    atomic_temporary_baseline: Vec<String>,
    keep_backup: bool,
    full_check: bool,
    result: Option<DatabaseCompactReport>,
    failure: Option<String>,
    updated_at_unix_seconds: u64,
}

impl DatabaseAccessLock {
    pub(crate) fn acquire_shared(database: &Path) -> Result<Self, AppError> {
        let path = lock_path(database)?;
        let file = open_lock_file(&path)?;
        file.try_lock_shared().map_err(|error| match error {
            fs::TryLockError::WouldBlock => AppError::DatabaseMaintenance(format!(
                "数据库正处于独占维护窗口：{}",
                database.display()
            )),
            fs::TryLockError::Error(error) => error.into(),
        })?;
        Ok(Self { _file: file })
    }

    pub(crate) fn acquire_exclusive(database: &Path, timeout: Duration) -> Result<Self, AppError> {
        let path = lock_path(database)?;
        let file = open_lock_file(&path)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(AppError::DatabaseMaintenance(format!(
                        "等待数据库独占维护锁超时（{} 秒）：{}",
                        timeout.as_secs(),
                        database.display()
                    )));
                }
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
}

pub(crate) fn ensure_no_pending_operation(database: &Path) -> Result<(), AppError> {
    for journal_path in journal_paths(database)? {
        let journal = read_journal(&journal_path)?;
        if journal.state.requires_recovery() {
            return Err(AppError::DatabaseMaintenance(format!(
                "检测到未完成的数据库压缩 request_id={} state={:?}；请使用相同 request_id 重新执行 database compact --apply 恢复",
                journal.request_id, journal.state
            )));
        }
    }
    Ok(())
}

pub(crate) fn preview_compaction(
    project_key: &str,
    database: &Path,
    options: &DatabaseCompactOptions,
) -> Result<DatabaseCompactReport, AppError> {
    validate_options(options)?;
    let _lock = DatabaseAccessLock::acquire_shared(database)?;
    let source = inspect_database_storage(database)?;
    let source_verification = inspect_database_logical_content(database, options.full_check)?;
    let source_file_sha256 = digest_file(database)?;
    let source_bytes = file_size(database)?;
    let wal_bytes_at_preflight = sqlite_sidecar_bytes(database, "-wal")?;
    let estimated_required_free_space_bytes = estimated_required_space(
        source.database_bytes,
        wal_bytes_at_preflight,
        options.keep_backup,
    )?;
    let available_space_bytes = fs2::available_space(parent_directory(database)?)?;
    Ok(DatabaseCompactReport {
        operation_protocol_version: OPERATION_PROTOCOL_VERSION,
        project_key: project_key.to_owned(),
        mode: "dry_run".to_owned(),
        applied: false,
        replayed: false,
        external_writer_protection: EXTERNAL_WRITER_PROTECTION.to_owned(),
        replacement_durability: REPLACEMENT_DURABILITY.to_owned(),
        request_id: None,
        parameters_sha256: None,
        source,
        target: None,
        source_logical_manifest_sha256: Some(source_verification.logical_manifest_sha256.clone()),
        target_logical_manifest_sha256: None,
        current_logical_manifest_sha256: Some(source_verification.logical_manifest_sha256),
        source_file_sha256: Some(source_file_sha256.clone()),
        target_file_sha256: None,
        current_file_sha256: Some(source_file_sha256.clone()),
        completed_logical_target_still_current: None,
        completed_file_target_still_current: None,
        source_bytes,
        target_bytes: None,
        bytes_reclaimed: None,
        estimated_required_free_space_bytes,
        available_space_bytes,
        wal_bytes_at_preflight,
        keep_backup: options.keep_backup,
        backup_path: None,
        wal_checkpoint: None,
    })
}

#[allow(clippy::too_many_lines, reason = "压缩事务状态按崩溃恢复顺序集中表达")]
pub(crate) fn apply_compaction(
    project_key: &str,
    database: &Path,
    options: &DatabaseCompactOptions,
) -> Result<DatabaseCompactReport, AppError> {
    validate_options(options)?;
    if !options.apply || !options.human_confirmed {
        return Err(AppError::DatabaseMaintenance(
            "物理压缩必须同时提供 --apply 与 --human-confirmed".to_owned(),
        ));
    }
    let request_id = options.request_id.as_deref().ok_or_else(|| {
        AppError::DatabaseMaintenance("物理压缩 --apply 必须提供 --request-id".to_owned())
    })?;
    validate_request_id(request_id)?;
    let timeout = Duration::from_secs(options.lock_timeout_seconds);
    let _lock = DatabaseAccessLock::acquire_exclusive(database, timeout)?;

    let request_sha256 = digest_bytes(request_id.as_bytes());
    let parameters_sha256 = parameters_hash(project_key, options)?;
    let journal_path = journal_path(database, &request_sha256)?;
    reject_other_pending_operations(database, &journal_path)?;

    if journal_path.is_file() {
        let journal = read_journal(&journal_path)?;
        validate_existing_journal(
            &journal,
            database,
            project_key,
            request_id,
            &parameters_sha256,
        )?;
        if journal.state == OperationState::Completed {
            let mut result = journal.result.ok_or_else(|| {
                AppError::DatabaseMaintenance(format!(
                    "已完成的压缩日志缺少 result：{}",
                    journal_path.display()
                ))
            })?;
            inspect_database_storage(database)?;
            let current_verification =
                inspect_database_logical_content(database, journal.full_check)?;
            let current_file_sha256 = digest_file(database)?;
            let file_matches = result
                .target_file_sha256
                .as_deref()
                .is_some_and(|target| target == current_file_sha256);
            let logical_matches = result
                .target_logical_manifest_sha256
                .as_deref()
                .is_some_and(|target| target == current_verification.logical_manifest_sha256);
            result.completed_file_target_still_current = Some(file_matches);
            result.completed_logical_target_still_current = Some(logical_matches);
            result.current_file_sha256 = Some(current_file_sha256);
            result.current_logical_manifest_sha256 =
                Some(current_verification.logical_manifest_sha256);
            result.replayed = true;
            return Ok(result);
        }
        cleanup_uncommitted_atomic_temporaries(database, &journal)?;
        return resume_compaction(database, &journal_path, journal);
    }

    let preliminary_source = inspect_database_storage(database)?;
    let wal_bytes_at_preflight = sqlite_sidecar_bytes(database, "-wal")?;
    let preliminary_required = estimated_required_space(
        preliminary_source.database_bytes,
        wal_bytes_at_preflight,
        options.keep_backup,
    )?;
    let preliminary_available = fs2::available_space(parent_directory(database)?)?;
    ensure_space_available(preliminary_required, preliminary_available)?;
    let checkpoint = checkpoint_database_wal(database)?;
    let source = inspect_database_storage(database)?;
    let source_verification = inspect_database_logical_content(database, options.full_check)?;
    let source_file_sha256 = digest_file(database)?;
    let source_bytes = file_size(database)?;
    let estimated_required_free_space_bytes = preliminary_required.max(estimated_required_space(
        source.database_bytes,
        0,
        options.keep_backup,
    )?);
    let available_space_bytes = fs2::available_space(parent_directory(database)?)?;
    ensure_space_available(estimated_required_free_space_bytes, available_space_bytes)?;

    let vacuum_path = vacuum_path(database, &request_sha256)?;
    let backup_path = options
        .keep_backup
        .then(|| backup_path(database, &request_sha256))
        .transpose()?;
    let mut journal = DatabaseCompactJournal {
        operation_protocol_version: OPERATION_PROTOCOL_VERSION,
        project_key: project_key.to_owned(),
        request_id: request_id.to_owned(),
        request_sha256,
        parameters_sha256,
        external_writer_protection: EXTERNAL_WRITER_PROTECTION.to_owned(),
        replacement_durability: REPLACEMENT_DURABILITY.to_owned(),
        state: OperationState::Running,
        database_file_name: file_name(database)?,
        vacuum_file_name: file_name(&vacuum_path)?,
        backup_file_name: backup_path.as_deref().map(file_name).transpose()?,
        source,
        source_file_bytes: source_bytes,
        source_verification,
        source_file_sha256,
        target: None,
        target_verification: None,
        target_file_sha256: None,
        wal_checkpoint: checkpoint,
        estimated_required_free_space_bytes,
        available_space_bytes,
        wal_bytes_at_preflight,
        atomic_temporary_baseline: Vec::new(),
        keep_backup: options.keep_backup,
        full_check: options.full_check,
        result: None,
        failure: None,
        updated_at_unix_seconds: now_unix_seconds()?,
    };
    write_journal(&journal_path, &journal)?;

    let result = execute_or_resume(database, &journal_path, &mut journal, false);
    if let Err(error) = &result {
        journal.state = failure_state(error);
        journal.failure = Some(error.to_string());
        journal.updated_at_unix_seconds = now_unix_seconds()?;
        write_journal(&journal_path, &journal)?;
    }
    result
}

fn resume_compaction(
    database: &Path,
    journal_path: &Path,
    mut journal: DatabaseCompactJournal,
) -> Result<DatabaseCompactReport, AppError> {
    let current_hash = digest_file(database)?;
    if journal.target_verification.is_some()
        && journal.target_file_sha256.as_deref() == Some(current_hash.as_str())
    {
        inspect_database_logical_content(database, journal.full_check)?;
        journal.state = OperationState::Swapped;
        return complete_operation(database, journal_path, &mut journal, true);
    }
    let source_matches = if current_hash == journal.source_file_sha256 {
        inspect_database_logical_content(database, journal.full_check).is_ok_and(|current| {
            current.logical_manifest_sha256 == journal.source_verification.logical_manifest_sha256
        })
    } else {
        false
    };
    if !source_matches && !restore_verified_backup(database, &journal)? {
        return fail_journal(
            journal_path,
            &mut journal,
            "恢复时当前数据库既不匹配原始文件，也不匹配已验证目标，且没有可用的已验证备份",
        );
    }
    journal.state = OperationState::Running;
    journal.failure = None;
    journal.updated_at_unix_seconds = now_unix_seconds()?;
    write_journal(journal_path, &journal)?;
    let result = execute_or_resume(database, journal_path, &mut journal, true);
    if let Err(error) = &result {
        journal.state = failure_state(error);
        journal.failure = Some(error.to_string());
        journal.updated_at_unix_seconds = now_unix_seconds()?;
        write_journal(journal_path, &journal)?;
    }
    result
}

fn restore_verified_backup(
    database: &Path,
    journal: &DatabaseCompactJournal,
) -> Result<bool, AppError> {
    let Some(backup_name) = journal.backup_file_name.as_deref() else {
        return Ok(false);
    };
    let backup = parent_directory(database)?.join(backup_name);
    if !backup.is_file() || digest_file(&backup)? != journal.source_file_sha256 {
        return Ok(false);
    }
    let current_hash = digest_file(database)?;
    remove_checkpointed_sidecars(database)?;
    atomic_copy_verified(
        &backup,
        database,
        Some(&current_hash),
        &journal.source_file_sha256,
    )?;
    let restored = inspect_database_logical_content(database, journal.full_check)?;
    if restored.logical_manifest_sha256 != journal.source_verification.logical_manifest_sha256 {
        return Err(AppError::DatabaseMaintenance(
            "备份文件哈希正确，但恢复后的逻辑清单不匹配原始数据库".to_owned(),
        ));
    }
    Ok(true)
}

fn execute_or_resume(
    database: &Path,
    journal_path: &Path,
    journal: &mut DatabaseCompactJournal,
    replayed: bool,
) -> Result<DatabaseCompactReport, AppError> {
    let parent = parent_directory(database)?;
    let vacuum_path = parent.join(&journal.vacuum_file_name);
    let backup_path = journal
        .backup_file_name
        .as_deref()
        .map(|name| parent.join(name));

    let reuse_vacuum = if vacuum_path.is_file() {
        inspect_database_logical_content(&vacuum_path, journal.full_check).is_ok_and(
            |verification| {
                verification.logical_manifest_sha256
                    == journal.source_verification.logical_manifest_sha256
            },
        )
    } else {
        false
    };
    if !reuse_vacuum {
        remove_known_temporary(&vacuum_path, parent)?;
        journal.wal_checkpoint = vacuum_database_into(database, &vacuum_path)?;
    }

    let source_after = inspect_database_logical_content(database, journal.full_check)?;
    let source_hash_after = digest_file(database)?;
    if source_after.logical_manifest_sha256 != journal.source_verification.logical_manifest_sha256
        || source_hash_after != journal.source_file_sha256
    {
        return Err(AppError::DatabaseMaintenance(
            "VACUUM 期间源数据库发生变化，拒绝替换".to_owned(),
        ));
    }
    let target = inspect_database_storage(&vacuum_path)?;
    let target_verification = inspect_database_logical_content(&vacuum_path, journal.full_check)?;
    if target_verification.logical_manifest_sha256
        != journal.source_verification.logical_manifest_sha256
    {
        return Err(AppError::DatabaseMaintenance(format!(
            "VACUUM 候选逻辑清单不一致：source={} target={}",
            journal.source_verification.logical_manifest_sha256,
            target_verification.logical_manifest_sha256
        )));
    }
    let target_file_sha256 = digest_file(&vacuum_path)?;
    journal.target = Some(target);
    journal.target_verification = Some(target_verification);
    journal.target_file_sha256 = Some(target_file_sha256);
    journal.atomic_temporary_baseline = atomic_database_temporary_file_names(database)?;
    journal.state = OperationState::Verified;
    journal.updated_at_unix_seconds = now_unix_seconds()?;
    write_journal(journal_path, journal)?;

    if let Some(backup_path) = backup_path.as_deref() {
        atomic_copy_verified(database, backup_path, None, &journal.source_file_sha256)?;
    }
    atomic_replace_database(&vacuum_path, database, journal)?;
    journal.state = OperationState::Swapped;
    journal.updated_at_unix_seconds = now_unix_seconds()?;
    write_journal(journal_path, journal)?;
    complete_operation(database, journal_path, journal, replayed)
}

const fn failure_state(error: &AppError) -> OperationState {
    if matches!(error, AppError::DatabaseSwapBusy(_)) {
        OperationState::Verified
    } else {
        OperationState::Failed
    }
}

fn complete_operation(
    database: &Path,
    journal_path: &Path,
    journal: &mut DatabaseCompactJournal,
    recovered_after_swap: bool,
) -> Result<DatabaseCompactReport, AppError> {
    let target = inspect_database_storage(database)?;
    let target_verification = inspect_database_logical_content(database, journal.full_check)?;
    let expected_verification = journal
        .target_verification
        .as_ref()
        .ok_or_else(|| AppError::DatabaseMaintenance("压缩日志缺少目标逻辑验证".to_owned()))?;
    if target_verification.logical_manifest_sha256 != expected_verification.logical_manifest_sha256
    {
        return Err(AppError::DatabaseMaintenance(
            "原子替换后的数据库逻辑清单不匹配已验证候选".to_owned(),
        ));
    }
    let target_file_sha256 = digest_file(database)?;
    if journal.target_file_sha256.as_deref() != Some(target_file_sha256.as_str()) {
        return Err(AppError::DatabaseMaintenance(
            "原子替换后的数据库文件哈希不匹配已验证候选".to_owned(),
        ));
    }
    let source_bytes = journal.source_file_bytes;
    let target_bytes = file_size(database)?;
    let parent = parent_directory(database)?;
    let result = DatabaseCompactReport {
        operation_protocol_version: OPERATION_PROTOCOL_VERSION,
        project_key: journal.project_key.clone(),
        mode: "apply".to_owned(),
        applied: true,
        replayed: recovered_after_swap,
        external_writer_protection: journal.external_writer_protection.clone(),
        replacement_durability: journal.replacement_durability.clone(),
        request_id: Some(journal.request_id.clone()),
        parameters_sha256: Some(journal.parameters_sha256.clone()),
        source: journal.source.clone(),
        target: Some(target),
        source_logical_manifest_sha256: Some(
            journal.source_verification.logical_manifest_sha256.clone(),
        ),
        target_logical_manifest_sha256: Some(target_verification.logical_manifest_sha256.clone()),
        current_logical_manifest_sha256: Some(target_verification.logical_manifest_sha256.clone()),
        source_file_sha256: Some(journal.source_file_sha256.clone()),
        target_file_sha256: Some(target_file_sha256),
        current_file_sha256: journal.target_file_sha256.clone(),
        completed_logical_target_still_current: Some(true),
        completed_file_target_still_current: Some(true),
        source_bytes,
        target_bytes: Some(target_bytes),
        bytes_reclaimed: Some(source_bytes.saturating_sub(target_bytes)),
        estimated_required_free_space_bytes: journal.estimated_required_free_space_bytes,
        available_space_bytes: journal.available_space_bytes,
        wal_bytes_at_preflight: journal.wal_bytes_at_preflight,
        keep_backup: journal.keep_backup,
        backup_path: journal.backup_file_name.clone(),
        wal_checkpoint: Some(journal.wal_checkpoint.clone()),
    };
    journal.target_verification = Some(target_verification);
    journal.result = Some(result.clone());
    journal.state = OperationState::Completed;
    journal.failure = None;
    journal.updated_at_unix_seconds = now_unix_seconds()?;
    let vacuum_path = parent.join(&journal.vacuum_file_name);
    remove_known_temporary(&vacuum_path, parent)?;
    write_journal(journal_path, journal)?;
    Ok(result)
}

fn fail_journal<T>(
    journal_path: &Path,
    journal: &mut DatabaseCompactJournal,
    message: &str,
) -> Result<T, AppError> {
    journal.state = OperationState::Failed;
    journal.failure = Some(message.to_owned());
    journal.updated_at_unix_seconds = now_unix_seconds()?;
    write_journal(journal_path, journal)?;
    Err(AppError::DatabaseMaintenance(message.to_owned()))
}

fn validate_options(options: &DatabaseCompactOptions) -> Result<(), AppError> {
    if options.lock_timeout_seconds > MAX_LOCK_TIMEOUT_SECONDS {
        return Err(AppError::DatabaseMaintenance(format!(
            "--lock-timeout-seconds 最大为 {MAX_LOCK_TIMEOUT_SECONDS}"
        )));
    }
    if !options.apply && (options.request_id.is_some() || options.human_confirmed) {
        return Err(AppError::DatabaseMaintenance(
            "dry-run 不接受 --request-id 或 --human-confirmed；请同时提供 --apply".to_owned(),
        ));
    }
    Ok(())
}

fn validate_request_id(request_id: &str) -> Result<(), AppError> {
    if request_id.trim().is_empty() || request_id.len() > 256 {
        return Err(AppError::DatabaseMaintenance(
            "request_id 必须是 1..=256 字节的非空字符串".to_owned(),
        ));
    }
    Ok(())
}

fn validate_existing_journal(
    journal: &DatabaseCompactJournal,
    database: &Path,
    project_key: &str,
    request_id: &str,
    parameters_sha256: &str,
) -> Result<(), AppError> {
    if journal.operation_protocol_version != OPERATION_PROTOCOL_VERSION {
        return Err(AppError::DatabaseMaintenance(format!(
            "不支持压缩日志协议版本 {}",
            journal.operation_protocol_version
        )));
    }
    if journal.external_writer_protection != EXTERNAL_WRITER_PROTECTION {
        return Err(AppError::DatabaseMaintenance(format!(
            "不支持压缩日志 external_writer_protection={}",
            journal.external_writer_protection
        )));
    }
    if journal.replacement_durability != REPLACEMENT_DURABILITY {
        return Err(AppError::DatabaseMaintenance(format!(
            "不支持压缩日志 replacement_durability={}",
            journal.replacement_durability
        )));
    }
    validate_atomic_temporary_baseline(database, &journal.atomic_temporary_baseline)?;
    if journal.project_key != project_key || journal.request_id != request_id {
        return Err(AppError::DatabaseMaintenance(
            "压缩日志的 project_key 或 request_id 不匹配".to_owned(),
        ));
    }
    let expected_request_sha256 = digest_bytes(request_id.as_bytes());
    if journal.request_sha256 != expected_request_sha256 {
        return Err(AppError::DatabaseMaintenance(
            "压缩日志 request_sha256 与 request_id 不一致".to_owned(),
        ));
    }
    if journal.parameters_sha256 != parameters_sha256 {
        return Err(AppError::DatabaseMaintenance(format!(
            "request_id={request_id} 已用于不同压缩参数"
        )));
    }
    let expected_database_name = file_name(database)?;
    let expected_vacuum_name = file_name(&vacuum_path(database, &expected_request_sha256)?)?;
    let expected_backup_name = if journal.keep_backup {
        Some(file_name(&backup_path(
            database,
            &expected_request_sha256,
        )?)?)
    } else {
        None
    };
    if journal.database_file_name != expected_database_name
        || journal.vacuum_file_name != expected_vacuum_name
        || journal.backup_file_name != expected_backup_name
    {
        return Err(AppError::DatabaseMaintenance(
            "压缩日志中的数据库、候选或备份文件名不符合确定性派生规则".to_owned(),
        ));
    }
    Ok(())
}

fn reject_other_pending_operations(database: &Path, current: &Path) -> Result<(), AppError> {
    for path in journal_paths(database)? {
        if path == current {
            continue;
        }
        let journal = read_journal(&path)?;
        if journal.state.requires_recovery() {
            return Err(AppError::DatabaseMaintenance(format!(
                "另一个压缩 request_id={} 尚未完成，必须先恢复该操作",
                journal.request_id
            )));
        }
    }
    Ok(())
}

fn parameters_hash(
    project_key: &str,
    options: &DatabaseCompactOptions,
) -> Result<String, AppError> {
    let payload = serde_json::json!({
        "operation_protocol_version": OPERATION_PROTOCOL_VERSION,
        "project_key": project_key,
        "full_check": options.full_check,
        "keep_backup": options.keep_backup,
    });
    Ok(digest_bytes(&serde_json::to_vec(&payload)?))
}

fn estimated_required_space(
    database_bytes: u64,
    wal_bytes: u64,
    keep_backup: bool,
) -> Result<u64, AppError> {
    let copies = if keep_backup { 3 } else { 2 };
    database_bytes
        .checked_mul(copies)
        .and_then(|bytes| bytes.checked_add(wal_bytes))
        .and_then(|bytes| bytes.checked_mul(DISK_MARGIN_NUMERATOR))
        .map(|bytes| bytes / DISK_MARGIN_DENOMINATOR)
        .ok_or_else(|| AppError::DatabaseMaintenance("磁盘空间预估溢出".to_owned()))
}

fn sqlite_sidecar_bytes(database: &Path, suffix: &str) -> Result<u64, AppError> {
    let path = parent_directory(database)?.join(format!("{}{suffix}", file_name(database)?));
    if path.is_file() {
        file_size(&path)
    } else {
        Ok(0)
    }
}

fn ensure_space_available(required: u64, available: u64) -> Result<(), AppError> {
    if available < required {
        return Err(AppError::DatabaseMaintenance(format!(
            "可用空间不足：需要至少 {required} 字节，当前 {available} 字节"
        )));
    }
    Ok(())
}

fn atomic_copy_verified(
    source: &Path,
    target: &Path,
    expected_target_sha256: Option<&str>,
    expected_source_sha256: &str,
) -> Result<(), AppError> {
    if digest_file(source)? != expected_source_sha256 {
        return Err(AppError::DatabaseMaintenance(format!(
            "原子复制源文件哈希漂移：{}",
            source.display()
        )));
    }
    if let Some(expected) = expected_target_sha256
        && digest_file(target)? != expected
    {
        return Err(AppError::ConcurrentModification(target.to_owned()));
    }
    if expected_target_sha256.is_none() && target.exists() {
        if digest_file(target)? == expected_source_sha256 {
            return Ok(());
        }
        return Err(AppError::ConcurrentModification(target.to_owned()));
    }
    let mut input = BufReader::new(File::open(source)?);
    let mut output = AtomicWriteFile::options().open(target)?;
    io::copy(&mut input, &mut output)?;
    output.flush()?;
    output
        .as_file()
        .set_permissions(fs::metadata(source)?.permissions())?;
    if let Some(expected) = expected_target_sha256
        && digest_file(target)? != expected
    {
        return Err(AppError::ConcurrentModification(target.to_owned()));
    }
    output.commit()?;
    if digest_file(target)? != expected_source_sha256 {
        return Err(AppError::DatabaseMaintenance(format!(
            "原子复制后文件哈希不一致：{}",
            target.display()
        )));
    }
    Ok(())
}

fn atomic_replace_database(
    candidate: &Path,
    database: &Path,
    journal: &mut DatabaseCompactJournal,
) -> Result<(), AppError> {
    for attempt in 0..=SWAP_RETRY_DELAYS_MS.len() {
        let before = atomic_database_temporary_files(database)?;
        match atomic_replace_database_once(candidate, database, journal) {
            Ok(()) => return Ok(()),
            Err(error @ AppError::DatabaseSwapBusy(_)) => {
                cleanup_new_atomic_temporaries(database, &before)?;
                let Some(delay) = SWAP_RETRY_DELAYS_MS.get(attempt) else {
                    return Err(error);
                };
                thread::sleep(Duration::from_millis(*delay));
            }
            Err(error) => {
                cleanup_new_atomic_temporaries(database, &before)?;
                return Err(error);
            }
        }
    }
    unreachable!("有限重试循环必然返回")
}

fn atomic_replace_database_once(
    candidate: &Path,
    database: &Path,
    journal: &mut DatabaseCompactJournal,
) -> Result<(), AppError> {
    let expected_target = journal
        .target_file_sha256
        .as_deref()
        .ok_or_else(|| AppError::DatabaseMaintenance("压缩日志缺少目标文件哈希".to_owned()))?;
    if digest_file(candidate)? != expected_target {
        return Err(AppError::DatabaseMaintenance(
            "原子替换前候选文件哈希漂移".to_owned(),
        ));
    }

    let mut input = BufReader::new(File::open(candidate)?);
    let mut output = AtomicWriteFile::options().open(database)?;
    io::copy(&mut input, &mut output)?;
    output.flush()?;
    output
        .as_file()
        .set_permissions(fs::metadata(candidate)?.permissions())?;

    journal.wal_checkpoint = checkpoint_database_wal(database)?;
    let source = inspect_database_logical_content(database, journal.full_check)?;
    if source.logical_manifest_sha256 != journal.source_verification.logical_manifest_sha256
        || digest_file(database)? != journal.source_file_sha256
    {
        return Err(AppError::DatabaseMaintenance(
            "原子提交前源数据库发生变化，拒绝替换".to_owned(),
        ));
    }
    remove_checkpointed_sidecars(database)?;
    if digest_file(database)? != journal.source_file_sha256 {
        return Err(AppError::ConcurrentModification(database.to_owned()));
    }

    if let Err(error) = output.commit() {
        return Err(
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
            ) {
                AppError::DatabaseSwapBusy(format!("{}：{error}", database.display()))
            } else {
                error.into()
            },
        );
    }
    if digest_file(database)? != expected_target {
        return Err(AppError::DatabaseMaintenance(
            "原子替换后的数据库文件哈希不匹配候选".to_owned(),
        ));
    }
    Ok(())
}

fn atomic_database_temporary_files(database: &Path) -> Result<BTreeSet<PathBuf>, AppError> {
    let parent = parent_directory(database)?;
    let mut paths = BTreeSet::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_atomic_database_temporary_name(database, name)? && entry.file_type()?.is_file() {
            paths.insert(entry.path());
        }
    }
    Ok(paths)
}

fn atomic_database_temporary_file_names(database: &Path) -> Result<Vec<String>, AppError> {
    atomic_database_temporary_files(database)?
        .iter()
        .map(|path| file_name(path))
        .collect()
}

fn is_atomic_database_temporary_name(database: &Path, name: &str) -> Result<bool, AppError> {
    let prefix = format!(".{}.", file_name(database)?);
    let suffix = name.strip_prefix(&prefix);
    Ok(suffix.is_some_and(|value| {
        value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }))
}

fn validate_atomic_temporary_baseline(
    database: &Path,
    baseline: &[String],
) -> Result<(), AppError> {
    if baseline.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AppError::DatabaseMaintenance(
            "压缩日志中的原子临时文件基线非法或未严格排序".to_owned(),
        ));
    }
    for name in baseline {
        if !is_atomic_database_temporary_name(database, name)? {
            return Err(AppError::DatabaseMaintenance(
                "压缩日志中的原子临时文件基线非法或未严格排序".to_owned(),
            ));
        }
    }
    Ok(())
}

fn cleanup_uncommitted_atomic_temporaries(
    database: &Path,
    journal: &DatabaseCompactJournal,
) -> Result<(), AppError> {
    let parent = parent_directory(database)?;
    let baseline = journal
        .atomic_temporary_baseline
        .iter()
        .map(|name| parent.join(name))
        .collect::<BTreeSet<_>>();
    for path in atomic_database_temporary_files(database)?.difference(&baseline) {
        remove_known_temporary(path, parent)?;
    }
    Ok(())
}

fn cleanup_new_atomic_temporaries(
    database: &Path,
    before: &BTreeSet<PathBuf>,
) -> Result<(), AppError> {
    let parent = parent_directory(database)?;
    for path in atomic_database_temporary_files(database)?.difference(before) {
        remove_known_temporary(path, parent)?;
    }
    Ok(())
}

fn write_journal(path: &Path, journal: &DatabaseCompactJournal) -> Result<(), AppError> {
    let mut output = AtomicWriteFile::options().open(path)?;
    output.write_all(&pretty_json_bytes(journal)?)?;
    output.commit()?;
    Ok(())
}

fn read_journal(path: &Path) -> Result<DatabaseCompactJournal, AppError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn journal_paths(database: &Path) -> Result<Vec<PathBuf>, AppError> {
    let parent = parent_directory(database)?;
    let prefix = format!("{}.maintenance.", file_name(database)?);
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix)
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            && entry.file_type()?.is_file()
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn journal_path(database: &Path, request_sha256: &str) -> Result<PathBuf, AppError> {
    Ok(parent_directory(database)?.join(format!(
        "{}.maintenance.{request_sha256}.json",
        file_name(database)?
    )))
}

fn vacuum_path(database: &Path, request_sha256: &str) -> Result<PathBuf, AppError> {
    Ok(parent_directory(database)?.join(format!(
        "{}.compact.{request_sha256}.vacuum.db",
        file_name(database)?
    )))
}

fn backup_path(database: &Path, request_sha256: &str) -> Result<PathBuf, AppError> {
    Ok(parent_directory(database)?.join(format!(
        "{}.backup.{request_sha256}.db",
        file_name(database)?
    )))
}

fn lock_path(database: &Path) -> Result<PathBuf, AppError> {
    Ok(parent_directory(database)?.join(format!("{}.maintenance.lock", file_name(database)?)))
}

fn open_lock_file(path: &Path) -> Result<File, AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn remove_known_temporary(path: &Path, expected_parent: &Path) -> Result<(), AppError> {
    if path.parent() != Some(expected_parent) {
        return Err(AppError::DatabaseMaintenance(format!(
            "拒绝删除不在数据库目录中的临时文件：{}",
            path.display()
        )));
    }
    if path.is_file() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn remove_checkpointed_sidecars(database: &Path) -> Result<(), AppError> {
    let parent = parent_directory(database)?;
    let database_name = file_name(database)?;
    let wal = parent.join(format!("{database_name}-wal"));
    if wal.is_file() {
        let bytes = file_size(&wal)?;
        if bytes != 0 {
            return Err(AppError::DatabaseMaintenance(format!(
                "WAL checkpoint 后 sidecar 仍有 {bytes} 字节：{}",
                wal.display()
            )));
        }
        fs::remove_file(&wal)?;
    }
    let shared_memory = parent.join(format!("{database_name}-shm"));
    if shared_memory.is_file() {
        fs::remove_file(shared_memory)?;
    }
    Ok(())
}

fn parent_directory(path: &Path) -> Result<&Path, AppError> {
    path.parent().ok_or_else(|| {
        AppError::DatabaseMaintenance(format!("数据库路径没有父目录：{}", path.display()))
    })
}

fn file_name(path: &Path) -> Result<String, AppError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::DatabaseMaintenance(format!(
                "数据库维护路径不是有效 UTF-8：{}",
                path.display()
            ))
        })
}

fn file_size(path: &Path) -> Result<u64, AppError> {
    Ok(fs::metadata(path)?.len())
}

fn digest_file(path: &Path) -> Result<String, AppError> {
    let mut input = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_unix_seconds() -> Result<u64, AppError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use brain_store::{BrainStore, inspect_database_logical_content};

    use super::{
        DatabaseAccessLock, DatabaseCompactOptions, OperationState, apply_compaction, digest_bytes,
        ensure_no_pending_operation, ensure_space_available, estimated_required_space,
        journal_path, read_journal, validate_options, write_journal,
    };

    #[test]
    fn disk_estimate_accounts_for_atomic_copy_and_optional_backup() {
        assert_eq!(estimated_required_space(1_000, 0, false).unwrap(), 2_400);
        assert_eq!(estimated_required_space(1_000, 0, true).unwrap(), 3_600);
        assert_eq!(estimated_required_space(1_000, 500, false).unwrap(), 3_000);
        assert!(ensure_space_available(3_600, 3_599).is_err());
        ensure_space_available(3_600, 3_600).unwrap();
    }

    #[test]
    fn dry_run_rejects_mutation_credentials() {
        let options = DatabaseCompactOptions {
            apply: false,
            request_id: Some("request-1".to_owned()),
            human_confirmed: false,
            full_check: false,
            keep_backup: true,
            lock_timeout_seconds: 5,
        };
        assert!(validate_options(&options).is_err());
    }

    #[test]
    fn shared_access_blocks_exclusive_maintenance_until_released() {
        let root = temporary_root("lock");
        let database = root.join("brain.db");
        BrainStore::open(&database).unwrap();
        let shared = DatabaseAccessLock::acquire_shared(&database).unwrap();
        assert!(
            DatabaseAccessLock::acquire_exclusive(&database, std::time::Duration::ZERO).is_err()
        );
        drop(shared);
        DatabaseAccessLock::acquire_exclusive(&database, std::time::Duration::ZERO).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compact_is_logically_lossless_idempotent_and_rejects_request_collisions() {
        let root = temporary_root("compact");
        let database = root.join("brain.db");
        BrainStore::open(&database).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        let payload = "x".repeat(2_048);
        for index in 0..500 {
            connection
                .execute(
                    "INSERT INTO audit_events(
                         event_id, session_id, hook_event, action_json, decision_json,
                         created_at_unix_seconds
                     ) VALUES (?1, 'session', 'test', ?2, '{}', 1)",
                    rusqlite::params![format!("event-{index}"), payload],
                )
                .unwrap();
        }
        connection
            .execute("DELETE FROM audit_events WHERE id % 2 = 0", [])
            .unwrap();
        drop(connection);
        let before = inspect_database_logical_content(&database, true).unwrap();
        let options = DatabaseCompactOptions {
            apply: true,
            request_id: Some("compact-request-1".to_owned()),
            human_confirmed: true,
            full_check: true,
            keep_backup: true,
            lock_timeout_seconds: 0,
        };

        let first = apply_compaction("project-test", &database, &options).unwrap();
        assert!(first.applied);
        assert!(!first.replayed);
        assert_eq!(
            first.source_logical_manifest_sha256.as_deref(),
            Some(before.logical_manifest_sha256.as_str())
        );
        assert_eq!(
            first.source_logical_manifest_sha256,
            first.target_logical_manifest_sha256
        );
        assert_ne!(first.source_file_sha256, first.target_file_sha256);

        let request_sha256 = digest_bytes(b"compact-request-1");
        let journal_path = journal_path(&database, &request_sha256).unwrap();
        let mut journal = read_journal(&journal_path).unwrap();
        let vacuum = root.join(&journal.vacuum_file_name);
        let backup = root.join(journal.backup_file_name.as_ref().unwrap());
        fs::copy(&database, &vacuum).unwrap();
        fs::copy(&backup, &database).unwrap();
        journal.state = OperationState::Failed;
        journal.failure = Some("simulated crash before swap".to_owned());
        journal.result = None;
        write_journal(&journal_path, &journal).unwrap();
        assert!(ensure_no_pending_operation(&database).is_err());

        let resumed_before_swap = apply_compaction("project-test", &database, &options).unwrap();
        assert!(resumed_before_swap.replayed);

        let mut journal = read_journal(&journal_path).unwrap();
        journal.state = OperationState::Verified;
        journal.result = None;
        write_journal(&journal_path, &journal).unwrap();
        let abandoned_atomic_temporary = root.join(".brain.db.abc123");
        fs::write(&abandoned_atomic_temporary, b"abandoned atomic temporary").unwrap();
        assert!(ensure_no_pending_operation(&database).is_err());
        let resumed_after_swap = apply_compaction("project-test", &database, &options).unwrap();
        assert!(resumed_after_swap.replayed);
        assert!(!abandoned_atomic_temporary.exists());

        let mut journal = read_journal(&journal_path).unwrap();
        journal.state = OperationState::Failed;
        journal.failure = Some("simulated target corruption after swap".to_owned());
        journal.result = None;
        write_journal(&journal_path, &journal).unwrap();
        fs::write(&database, b"corrupt-target").unwrap();
        let restored_from_backup = apply_compaction("project-test", &database, &options).unwrap();
        assert!(restored_from_backup.replayed);

        BrainStore::open(&database).unwrap();

        let replay = apply_compaction("project-test", &database, &options).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.completed_logical_target_still_current, Some(true));

        append_audit_event(&database, "event-after-compaction");
        let replay_after_legitimate_write =
            apply_compaction("project-test", &database, &options).unwrap();
        assert!(replay_after_legitimate_write.replayed);
        assert_eq!(
            replay_after_legitimate_write.completed_logical_target_still_current,
            Some(false)
        );
        assert_ne!(
            replay_after_legitimate_write.current_logical_manifest_sha256,
            replay_after_legitimate_write.target_logical_manifest_sha256
        );
        let collision = DatabaseCompactOptions {
            full_check: false,
            ..options
        };
        assert!(apply_compaction("project-test", &database, &collision).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn append_audit_event(database: &std::path::Path, event_id: &str) {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection
            .execute(
                "INSERT INTO audit_events(
                     event_id, session_id, hook_event, action_json, decision_json,
                     created_at_unix_seconds
                 ) VALUES (?1, 'session', 'test', '{}', '{}', 2)",
                [event_id],
            )
            .unwrap();
    }

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-database-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
}
