use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use brain_core::{
    ActionKind, AdapterIdentity, AdapterKind, BrainConfig, EventIdentityQuality, FeedbackItem,
    GateDecision, HOOK_PROTOCOL_VERSION, HookEventPayload, HookOutcomePayload, IdempotencyMetadata,
    InternalHookEvent, InternalHookOutcome, SessionOpenReason, SessionOpened, StopReconcileConfig,
    ToolAboutToRun, ToolAction, ToolFinished, ToolStatus,
};
use brain_store::{AdapterRecordResult, BrainStore, DATABASE_SCHEMA_VERSION, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{app::HookEvent, codex, dsh, error::AppError, git, opencode, pi, provider, setup};

const QUALIFICATION_SCHEMA_VERSION: u32 = 1;
const QUALIFICATION_SUITE_ID: &str = "control-plane";
const QUALIFICATION_SUITE_VERSION: u32 = 1;
const QUALIFICATION_LEDGER_FILE: &str = "qualification.sqlite";
const LONG_SESSION_EVENT_COUNT: usize = 10_000;
const CONCURRENT_SESSION_COUNT: usize = 32;
const OPERATIONS_PER_SESSION: usize = 100;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QualificationState {
    Running,
    Qualified,
    Failed,
    Inconclusive,
}

impl QualificationState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Qualified => "qualified",
            Self::Failed => "failed",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum QualificationCaseState {
    Passed,
    Failed,
    Inconclusive,
}

impl QualificationCaseState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QualificationTarget {
    pub project_brain_version: String,
    pub binary_sha256: String,
    pub contract_manifest_hash: String,
    pub database_schema_version: i64,
    pub os: String,
    pub architecture: String,
    pub target_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QualificationContext {
    project_key: String,
    project_root: PathBuf,
    source_fingerprint_before: Option<String>,
    source_fingerprint_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationCaseReport {
    pub case_id: String,
    pub case_version: u32,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    pub observation_hash: String,
    pub metrics: Value,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationRunReport {
    pub schema_version: u32,
    pub suite_id: String,
    pub suite_version: u32,
    pub run_id: String,
    pub request_id: String,
    pub status: QualificationState,
    pub target: QualificationTarget,
    context: QualificationContext,
    pub cases: Vec<QualificationCaseReport>,
    pub started_at_unix_seconds: i64,
    pub finished_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationRunSummary {
    pub run_id: String,
    pub request_id: String,
    pub status: QualificationState,
    pub target_hash: String,
    pub started_at_unix_seconds: i64,
    pub finished_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualificationStatusReport {
    pub schema_version: u32,
    pub suite_id: String,
    pub suite_version: u32,
    pub current_target: QualificationTarget,
    pub qualified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matching_qualified_run: Option<QualificationRunSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_run: Option<QualificationRunSummary>,
}

struct QualificationLedger {
    connection: Connection,
}

struct BeginRun<'a> {
    run_id: &'a str,
    request_id: &'a str,
    request_hash: &'a str,
    target: &'a QualificationTarget,
    project_key: &'a str,
    source_fingerprint: Option<&'a str>,
    started_at: i64,
}

impl QualificationLedger {
    #[allow(
        clippy::too_many_lines,
        reason = "资格账本 schema、append-only trigger 与版本拒绝必须在一次显式初始化中审计"
    )]
    fn open(path: &Path) -> Result<Self, AppError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS qualification_metadata (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qualification_runs (
                 run_id TEXT PRIMARY KEY,
                 request_id TEXT NOT NULL UNIQUE,
                 request_hash TEXT NOT NULL,
                 suite_id TEXT NOT NULL,
                 suite_version INTEGER NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('running', 'qualified', 'failed', 'inconclusive')),
                 project_brain_version TEXT NOT NULL,
                 binary_sha256 TEXT NOT NULL,
                 contract_manifest_hash TEXT NOT NULL,
                 target_hash TEXT NOT NULL,
                 database_schema_version INTEGER NOT NULL,
                 os TEXT NOT NULL,
                 architecture TEXT NOT NULL,
                 project_key TEXT NOT NULL,
                 source_fingerprint TEXT,
                 started_at_unix_seconds INTEGER NOT NULL,
                 finished_at_unix_seconds INTEGER,
                 report_json TEXT,
                 report_hash TEXT,
                 CHECK(
                     (status = 'running' AND finished_at_unix_seconds IS NULL
                         AND report_json IS NULL AND report_hash IS NULL)
                     OR
                     (status != 'running' AND finished_at_unix_seconds IS NOT NULL
                         AND report_json IS NOT NULL AND report_hash IS NOT NULL)
                 )
             );
             CREATE INDEX IF NOT EXISTS idx_qualification_runs_target
                 ON qualification_runs(target_hash, status, finished_at_unix_seconds DESC);
             CREATE TABLE IF NOT EXISTS qualification_case_results (
                 run_id TEXT NOT NULL,
                 case_id TEXT NOT NULL,
                 case_version INTEGER NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('passed', 'failed', 'inconclusive')),
                 failure_code TEXT,
                 observation_hash TEXT NOT NULL,
                 metrics_json TEXT NOT NULL,
                 elapsed_ms INTEGER NOT NULL,
                 PRIMARY KEY(run_id, case_id),
                 FOREIGN KEY(run_id) REFERENCES qualification_runs(run_id)
             );
             CREATE TRIGGER IF NOT EXISTS trg_qualification_case_no_update
                 BEFORE UPDATE ON qualification_case_results
                 BEGIN SELECT RAISE(ABORT, 'qualification case results are immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_qualification_case_no_delete
                 BEFORE DELETE ON qualification_case_results
                 BEGIN SELECT RAISE(ABORT, 'qualification case results are immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_qualification_run_no_delete
                 BEFORE DELETE ON qualification_runs
                 BEGIN SELECT RAISE(ABORT, 'qualification runs are append-only'); END;
             CREATE TRIGGER IF NOT EXISTS trg_qualification_run_terminal_once
                 BEFORE UPDATE ON qualification_runs
                 WHEN OLD.status != 'running'
                      OR NEW.run_id != OLD.run_id
                      OR NEW.request_id != OLD.request_id
                      OR NEW.request_hash != OLD.request_hash
                      OR NEW.suite_id != OLD.suite_id
                      OR NEW.suite_version != OLD.suite_version
                      OR NEW.project_brain_version != OLD.project_brain_version
                      OR NEW.binary_sha256 != OLD.binary_sha256
                      OR NEW.contract_manifest_hash != OLD.contract_manifest_hash
                      OR NEW.target_hash != OLD.target_hash
                      OR NEW.database_schema_version != OLD.database_schema_version
                      OR NEW.os != OLD.os
                      OR NEW.architecture != OLD.architecture
                      OR NEW.project_key != OLD.project_key
                      OR NEW.source_fingerprint IS NOT OLD.source_fingerprint
                      OR NEW.started_at_unix_seconds != OLD.started_at_unix_seconds
                      OR NEW.status = 'running'
                      OR NEW.finished_at_unix_seconds IS NULL
                      OR NEW.report_json IS NULL
                      OR NEW.report_hash IS NULL
                 BEGIN SELECT RAISE(ABORT, 'invalid qualification run transition'); END;",
        )?;
        let stored_version = connection
            .query_row(
                "SELECT value FROM qualification_metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match stored_version {
            None => {
                connection.execute(
                    "INSERT INTO qualification_metadata(key, value) VALUES('schema_version', ?1)",
                    [QUALIFICATION_SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(version) if version == QUALIFICATION_SCHEMA_VERSION.to_string() => {}
            Some(version) => {
                return Err(AppError::Qualification(format!(
                    "不支持 qualification ledger schema_version={version}"
                )));
            }
        }
        Ok(Self { connection })
    }

    fn replay_for_request(
        &self,
        request_id: &str,
        request_hash: &str,
    ) -> Result<Option<QualificationRunReport>, AppError> {
        let existing = self
            .connection
            .query_row(
                "SELECT request_hash, status, report_json, report_hash FROM qualification_runs
                 WHERE request_id = ?1",
                [request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored_hash, status, report_json, report_hash)) = existing else {
            return Ok(None);
        };
        if stored_hash != request_hash {
            return Err(AppError::Qualification(format!(
                "request_id={request_id:?} 已绑定到不同资格目标"
            )));
        }
        let report_json = report_json.ok_or_else(|| {
            AppError::Qualification(format!(
                "request_id={request_id:?} 对应运行仍为 {status}；中断运行不会被冒充为 Qualified，请使用新 request_id"
            ))
        })?;
        let report_hash = report_hash.ok_or_else(|| {
            AppError::Qualification(format!(
                "request_id={request_id:?} 的终态证明缺少 report_hash"
            ))
        })?;
        verify_report_hash(&report_json, &report_hash)?;
        Ok(Some(serde_json::from_str(&report_json)?))
    }

    fn begin_run(&self, run: &BeginRun<'_>) -> Result<(), AppError> {
        self.connection.execute(
            "INSERT INTO qualification_runs(
                 run_id, request_id, request_hash, suite_id, suite_version, status,
                 project_brain_version, binary_sha256, contract_manifest_hash, target_hash,
                 database_schema_version, os, architecture, project_key, source_fingerprint,
                 started_at_unix_seconds
             ) VALUES(?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                run.run_id,
                run.request_id,
                run.request_hash,
                QUALIFICATION_SUITE_ID,
                QUALIFICATION_SUITE_VERSION,
                run.target.project_brain_version,
                run.target.binary_sha256,
                run.target.contract_manifest_hash,
                run.target.target_hash,
                run.target.database_schema_version,
                run.target.os,
                run.target.architecture,
                run.project_key,
                run.source_fingerprint,
                run.started_at,
            ],
        )?;
        Ok(())
    }

    fn record_case(&self, run_id: &str, case: &QualificationCaseReport) -> Result<(), AppError> {
        self.connection.execute(
            "INSERT INTO qualification_case_results(
                 run_id, case_id, case_version, status, failure_code,
                 observation_hash, metrics_json, elapsed_ms
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                case.case_id,
                case.case_version,
                case.status,
                case.failure_code,
                case.observation_hash,
                serde_json::to_string(&case.metrics)?,
                i64::try_from(case.elapsed_ms).unwrap_or(i64::MAX),
            ],
        )?;
        Ok(())
    }

    fn finish_run(&self, report: &QualificationRunReport) -> Result<(), AppError> {
        let report_json = serde_json::to_string(report)?;
        let report_hash = hash_bytes(report_json.as_bytes());
        let changed = self.connection.execute(
            "UPDATE qualification_runs
             SET status = ?1, finished_at_unix_seconds = ?2, report_json = ?3,
                 report_hash = ?4
             WHERE run_id = ?5 AND status = 'running'",
            params![
                report.status.as_str(),
                report.finished_at_unix_seconds,
                report_json,
                report_hash,
                report.run_id,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::Qualification(format!(
                "资格运行 {} 无法从 running 原子收口",
                report.run_id
            )));
        }
        Ok(())
    }

    fn status(&self, target: QualificationTarget) -> Result<QualificationStatusReport, AppError> {
        let matching_qualified_run = self.summary_query(
            "SELECT run_id, request_id, status, target_hash, started_at_unix_seconds,
                    finished_at_unix_seconds, report_json, report_hash
             FROM qualification_runs
             WHERE target_hash = ?1 AND status = 'qualified'
             ORDER BY finished_at_unix_seconds DESC, rowid DESC LIMIT 1",
            Some(&target.target_hash),
        )?;
        let latest_run = self.summary_query(
            "SELECT run_id, request_id, status, target_hash, started_at_unix_seconds,
                    finished_at_unix_seconds, report_json, report_hash
             FROM qualification_runs
             ORDER BY started_at_unix_seconds DESC, rowid DESC LIMIT 1",
            None,
        )?;
        Ok(QualificationStatusReport {
            schema_version: QUALIFICATION_SCHEMA_VERSION,
            suite_id: QUALIFICATION_SUITE_ID.to_owned(),
            suite_version: QUALIFICATION_SUITE_VERSION,
            current_target: target,
            qualified: matching_qualified_run.is_some(),
            matching_qualified_run,
            latest_run,
        })
    }

    fn summary_query(
        &self,
        sql: &str,
        target_hash: Option<&str>,
    ) -> Result<Option<QualificationRunSummary>, AppError> {
        let mapper = |row: &rusqlite::Row<'_>| {
            let status: String = row.get(2)?;
            let status = parse_state(&status).map_err(|message| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        message,
                    )),
                )
            })?;
            let run_id: String = row.get(0)?;
            let target_hash: String = row.get(3)?;
            let report_json: Option<String> = row.get(6)?;
            let report_hash: Option<String> = row.get(7)?;
            if status != QualificationState::Running {
                let valid = report_json.zip(report_hash).is_some_and(|(report, hash)| {
                    hash_bytes(report.as_bytes()) == hash
                        && serde_json::from_str::<QualificationRunReport>(&report).is_ok_and(
                            |parsed| {
                                parsed.run_id == run_id
                                    && parsed.status == status
                                    && parsed.target.target_hash == target_hash
                            },
                        )
                });
                if !valid {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "qualification report hash or identity mismatch",
                        )),
                    ));
                }
            }
            Ok(QualificationRunSummary {
                run_id,
                request_id: row.get(1)?,
                status,
                target_hash,
                started_at_unix_seconds: row.get(4)?,
                finished_at_unix_seconds: row.get(5)?,
            })
        };
        let result = if let Some(target_hash) = target_hash {
            self.connection
                .query_row(sql, [target_hash], mapper)
                .optional()?
        } else {
            self.connection.query_row(sql, [], mapper).optional()?
        };
        Ok(result)
    }

    fn show(&self, run_id: &str) -> Result<QualificationRunReport, AppError> {
        let row = self
            .connection
            .query_row(
                "SELECT status, report_json, report_hash FROM qualification_runs WHERE run_id = ?1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| AppError::Qualification(format!("资格运行不存在：{run_id}")))?;
        let report_json = row.1.ok_or_else(|| {
            AppError::Qualification(format!(
                "资格运行 {run_id} 仍为 {}；中断运行没有完整证明",
                row.0
            ))
        })?;
        let report_hash = row.2.ok_or_else(|| {
            AppError::Qualification(format!("资格运行 {run_id} 缺少 report_hash"))
        })?;
        verify_report_hash(&report_json, &report_hash)?;
        Ok(serde_json::from_str(&report_json)?)
    }
}

pub fn run(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    config: &BrainConfig,
    request_id: &str,
) -> Result<QualificationRunReport, AppError> {
    validate_external_id("request_id", request_id)?;
    let install_root = setup::resolve_install_root(explicit_install_root)?;
    let state_root = install_root.join("state");
    fs::create_dir_all(&state_root)?;
    let ledger = QualificationLedger::open(&state_root.join(QUALIFICATION_LEDGER_FILE))?;
    let target_before = current_target()?;
    let project_root = project_root.canonicalize()?;
    let source_before = git::worktree_fingerprint(&project_root).ok();
    let request_hash = hash_json(&json!({
        "suite_id": QUALIFICATION_SUITE_ID,
        "suite_version": QUALIFICATION_SUITE_VERSION,
        "target_hash": target_before.target_hash,
        "project_key": config.project_key,
        "project_root": project_root,
        "source_fingerprint": source_before,
    }))?;
    if let Some(report) = ledger.replay_for_request(request_id, &request_hash)? {
        return Ok(report);
    }

    let started_at = unix_seconds()?;
    let run_id = format!(
        "qualification_{}",
        &hash_bytes(format!("{request_id}\0{request_hash}\0{started_at}").as_bytes())[..32]
    );
    ledger.begin_run(&BeginRun {
        run_id: &run_id,
        request_id,
        request_hash: &request_hash,
        target: &target_before,
        project_key: &config.project_key,
        source_fingerprint: source_before.as_deref(),
        started_at,
    })?;

    let fixture_root = state_root.join("qualification-work").join(&run_id);
    if fixture_root.exists() {
        return Err(AppError::Qualification(format!(
            "资格 fixture 路径已存在：{}",
            fixture_root.display()
        )));
    }
    fs::create_dir_all(&fixture_root)?;

    let mut cases = Vec::new();
    for (case_id, case) in [
        ("Q1_adapter_contract", case_adapter_contract as CaseFunction),
        ("Q2_project_isolation", case_project_isolation),
        ("Q3_replay_idempotency", case_replay_idempotency),
        ("Q4_concurrent_interleaving", case_concurrent_interleaving),
        ("Q5_provider_drift", case_provider_drift),
        ("Q6_stop_loop_boundedness", case_stop_loop_boundedness),
        ("Q7_long_session_stability", case_long_session_stability),
    ] {
        let report = execute_case(case_id, &fixture_root, case);
        ledger.record_case(&run_id, &report)?;
        cases.push(report);
    }

    let source_after = git::worktree_fingerprint(&project_root).ok();
    let target_after = current_target()?;
    let target_changed = target_before != target_after;
    let source_changed = source_before != source_after;
    let status = if cases
        .iter()
        .any(|case| case.status == QualificationCaseState::Failed.as_str())
    {
        QualificationState::Failed
    } else if target_changed
        || source_changed
        || cases
            .iter()
            .any(|case| case.status == QualificationCaseState::Inconclusive.as_str())
    {
        QualificationState::Inconclusive
    } else {
        QualificationState::Qualified
    };
    let report = QualificationRunReport {
        schema_version: QUALIFICATION_SCHEMA_VERSION,
        suite_id: QUALIFICATION_SUITE_ID.to_owned(),
        suite_version: QUALIFICATION_SUITE_VERSION,
        run_id,
        request_id: request_id.to_owned(),
        status,
        target: target_before,
        context: QualificationContext {
            project_key: config.project_key.clone(),
            project_root,
            source_fingerprint_before: source_before,
            source_fingerprint_after: source_after,
        },
        cases,
        started_at_unix_seconds: started_at,
        finished_at_unix_seconds: unix_seconds()?,
    };
    ledger.finish_run(&report)?;
    cleanup_fixture(&state_root, &fixture_root)?;
    Ok(report)
}

pub fn status(explicit_install_root: Option<&Path>) -> Result<QualificationStatusReport, AppError> {
    let install_root = setup::resolve_install_root(explicit_install_root)?;
    let state_root = install_root.join("state");
    fs::create_dir_all(&state_root)?;
    QualificationLedger::open(&state_root.join(QUALIFICATION_LEDGER_FILE))?
        .status(current_target()?)
}

pub fn show(
    explicit_install_root: Option<&Path>,
    run_id: &str,
) -> Result<QualificationRunReport, AppError> {
    validate_external_id("run_id", run_id)?;
    let install_root = setup::resolve_install_root(explicit_install_root)?;
    let ledger_path = install_root.join("state").join(QUALIFICATION_LEDGER_FILE);
    if !ledger_path.is_file() {
        return Err(AppError::Qualification("资格账本尚不存在".to_owned()));
    }
    QualificationLedger::open(&ledger_path)?.show(run_id)
}

type CaseFunction = fn(&Path) -> Result<Value, String>;

fn execute_case(case_id: &str, root: &Path, case: CaseFunction) -> QualificationCaseReport {
    let started = Instant::now();
    match case(root) {
        Ok(metrics) => case_report(
            case_id,
            QualificationCaseState::Passed,
            None,
            metrics,
            started,
        ),
        Err(error) => case_report(
            case_id,
            QualificationCaseState::Failed,
            Some(error),
            json!({}),
            started,
        ),
    }
}

fn case_report(
    case_id: &str,
    status: QualificationCaseState,
    failure: Option<String>,
    metrics: Value,
    started: Instant,
) -> QualificationCaseReport {
    let failure_code = failure.as_ref().map(|message| {
        format!(
            "qualification_{}",
            &hash_bytes(format!("{case_id}\0{message}").as_bytes())[..16]
        )
    });
    let observation_hash = hash_json(&json!({
        "case_id": case_id,
        "case_version": 1,
        "status": status.as_str(),
        "failure": failure,
        "metrics": metrics,
    }))
    .unwrap_or_else(|_| "sha256_unavailable".to_owned());
    QualificationCaseReport {
        case_id: case_id.to_owned(),
        case_version: 1,
        status: status.as_str().to_owned(),
        failure_code,
        failure_message: failure,
        observation_hash,
        metrics,
        elapsed_ms: elapsed_millis(started),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Q1 在一个固定用例中交叉核对三种 adapter 输出、错误类型与身份降级"
)]
fn case_adapter_contract(root: &Path) -> Result<Value, String> {
    let case_root = root.join("q1");
    fs::create_dir_all(&case_root).map_err(display_error)?;
    let store = BrainStore::open(&case_root.join("brain.db")).map_err(display_error)?;
    let config = fixture_config("qualification_q1");
    let provider_trust = BTreeMap::new();
    let input_value = json!({
        "session_id": "session-q1",
        "cwd": case_root,
        "turn_id": "turn-q1",
        "tool_name": "Read",
        "tool_use_id": "operation-q1",
        "tool_input": { "file_path": "README.md" },
        "unknown_future_field": { "retained_by_vendor": true }
    });
    let input: codex::CodexHookInput =
        serde_json::from_value(input_value).map_err(display_error)?;
    let codex_output = codex::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::PreToolUse,
        &input,
    )
    .map_err(display_error)?;
    let pi_output = pi::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::PreToolUse,
        &input,
    );
    let opencode_output = opencode::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::PreToolUse,
        &input,
    );
    let dsh_output = dsh::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::PreToolUse,
        &input,
    );
    if codex_output
        .0
        .pointer("/hookSpecificOutput/permissionDecision")
        == Some(&json!("allow"))
        || pi_output.0.get("block") != Some(&json!(false))
        || opencode_output.0.get("block") != Some(&json!(false))
        || dsh_output.0.get("block") != Some(&json!(false))
    {
        return Err("adapter no-veto 被错误表达成授权或缺少 block=false".to_owned());
    }
    if serde_json::from_value::<codex::CodexHookInput>(json!({ "session_id": 7 })).is_ok() {
        return Err("adapter 接受了已知字段的错误类型".to_owned());
    }

    let per_delivery: codex::CodexHookInput = serde_json::from_value(json!({
        "session_id": "session-q1",
        "cwd": case_root,
        "tool_name": "Read",
        "tool_input": { "file_path": "README.md" }
    }))
    .map_err(display_error)?;
    for _ in 0..2 {
        codex::handle_with_provider_trust(
            &case_root,
            &config,
            &store,
            &provider_trust,
            HookEvent::PreToolUse,
            &per_delivery,
        )
        .map_err(display_error)?;
    }
    let records = store
        .recent_adapter_audit(&config.project_key, 20)
        .map_err(display_error)?;
    let per_delivery_events = records
        .iter()
        .filter_map(|record| serde_json::from_str::<InternalHookEvent>(&record.event_json).ok())
        .filter(|event| event.idempotency.identity_quality == EventIdentityQuality::PerDelivery)
        .collect::<Vec<_>>();
    if per_delivery_events.len() != 2
        || per_delivery_events[0].event_id == per_delivery_events[1].event_id
    {
        return Err("缺少 vendor stable ID 时没有诚实生成两个 per_delivery 事件".to_owned());
    }
    let capabilities = json!({
        "codex": codex::capabilities(),
        "pi": pi::capabilities(),
        "opencode": opencode::capabilities(),
        "dsh": dsh::capabilities(),
    });
    if capabilities["opencode"]["continue_after_stop"] != "unsupported"
        || capabilities["pi"]["continue_after_stop"] != "emulated"
        || capabilities["dsh"]["continue_after_stop"] != "supported"
        || capabilities["codex"]["deny_intent"] != "unsupported"
    {
        return Err("adapter capability contract 与显式降级边界不一致".to_owned());
    }
    Ok(json!({
        "adapter_count": 4,
        "audited_event_count": records.len(),
        "per_delivery_event_count": per_delivery_events.len(),
        "capabilities_hash": hash_json(&capabilities).map_err(display_error)?,
    }))
}

fn case_project_isolation(root: &Path) -> Result<Value, String> {
    let database = root.join("q2-isolation.db");
    let store = BrainStore::open(&database).map_err(display_error)?;
    let event_a = fixture_session_event("qualification_project_a", "shared-event", "session-a");
    let event_b = fixture_session_event("qualification_project_b", "shared-event", "session-b");
    store
        .record_adapter_event(&event_a, &fixture_session_outcome("shared-event"), 1)
        .map_err(display_error)?;
    store
        .record_adapter_event(&event_b, &fixture_session_outcome("shared-event"), 1)
        .map_err(display_error)?;
    let records_a = store
        .recent_adapter_audit("qualification_project_a", 10)
        .map_err(display_error)?;
    let records_b = store
        .recent_adapter_audit("qualification_project_b", 10)
        .map_err(display_error)?;
    if records_a.len() != 1
        || records_b.len() != 1
        || records_a[0].project_key == records_b[0].project_key
        || records_a[0].session_key != "session-a"
        || records_b[0].session_key != "session-b"
    {
        return Err("相同 adapter/event_id 的两个 project 没有保持严格隔离".to_owned());
    }
    Ok(json!({
        "project_count": 2,
        "shared_event_id": true,
        "rows_per_project": [records_a.len(), records_b.len()],
    }))
}

fn case_replay_idempotency(root: &Path) -> Result<Value, String> {
    let database = root.join("q3-replay.db");
    drop(BrainStore::open(&database).map_err(display_error)?);
    let event = fixture_session_event("qualification_q3", "stable-event", "stable-session");
    let outcome = fixture_session_outcome("stable-event");
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let database = database.clone();
            let event = event.clone();
            let outcome = outcome.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<Vec<AdapterRecordResult>, StoreError> {
                let store = BrainStore::open(&database)?;
                barrier.wait();
                (0..10)
                    .map(|_| store.record_adapter_event(&event, &outcome, 1))
                    .collect()
            })
        })
        .collect::<Vec<_>>();
    let mut inserted = 0;
    let mut duplicate = 0;
    for handle in handles {
        for result in handle
            .join()
            .map_err(|_| "Q3 并发 worker panic".to_owned())?
            .map_err(display_error)?
        {
            match result {
                AdapterRecordResult::Inserted(_) => inserted += 1,
                AdapterRecordResult::Duplicate(_) => duplicate += 1,
            }
        }
    }
    let store = BrainStore::open(&database).map_err(display_error)?;
    let restart_result = store
        .record_adapter_event(&event, &outcome, 1)
        .map_err(display_error)?;
    let mut collision = event.clone();
    "different-session".clone_into(&mut collision.session_key);
    let collision_rejected = matches!(
        store.record_adapter_event(&collision, &outcome, 1),
        Err(StoreError::AdapterIdempotencyConflict(_))
    );
    let rows = store
        .recent_adapter_audit("qualification_q3", 10)
        .map_err(display_error)?;
    if inserted != 1
        || duplicate != 79
        || !matches!(restart_result, AdapterRecordResult::Duplicate(_))
        || !collision_rejected
        || rows.len() != 1
    {
        return Err("并发、重启或碰撞重放没有收敛到唯一原始事件".to_owned());
    }
    Ok(json!({
        "deliveries": 81,
        "inserted": inserted,
        "duplicates_before_restart": duplicate,
        "restart_replayed": true,
        "different_payload_rejected": true,
        "persisted_rows": rows.len(),
    }))
}

#[allow(
    clippy::too_many_lines,
    reason = "Q4 的固定事件生成、并发调度和完整因果复核保持在同一资格用例"
)]
fn case_concurrent_interleaving(root: &Path) -> Result<Value, String> {
    let database = root.join("q4-interleaving.db");
    drop(BrainStore::open(&database).map_err(display_error)?);
    let mut deliveries = Vec::with_capacity(CONCURRENT_SESSION_COUNT * OPERATIONS_PER_SESSION * 2);
    for session in 0..CONCURRENT_SESSION_COUNT {
        let project = if session % 2 == 0 {
            "qualification_q4_a"
        } else {
            "qualification_q4_b"
        };
        for operation in 0..OPERATIONS_PER_SESSION {
            let operation_id = format!("operation-{session:02}-{operation:03}");
            let action = ToolAction {
                kind: ActionKind::Modify,
                target_files: vec![format!("src/session-{session:02}.rs")],
                command: None,
                deterministic_impacts: Vec::new(),
            };
            let pre_event_id = format!("pre-{operation_id}");
            deliveries.push((
                fixture_event(
                    project,
                    &pre_event_id,
                    &format!("session-{session:02}"),
                    HookEventPayload::ToolAboutToRun(ToolAboutToRun {
                        operation_id: operation_id.clone(),
                        tool_name: "Edit".to_owned(),
                        action: action.clone(),
                    }),
                ),
                InternalHookOutcome {
                    protocol_version: HOOK_PROTOCOL_VERSION,
                    event_id: pre_event_id,
                    payload: HookOutcomePayload::ToolAboutToRun {
                        gate: GateDecision::NoVeto,
                        inject: Vec::new(),
                    },
                },
            ));
            let post_event_id = format!("post-{operation_id}");
            deliveries.push((
                fixture_event(
                    project,
                    &post_event_id,
                    &format!("session-{session:02}"),
                    HookEventPayload::ToolFinished(ToolFinished {
                        operation_id,
                        tool_name: "Edit".to_owned(),
                        action,
                        status: ToolStatus::Succeeded,
                        duration_ms: Some(1),
                    }),
                ),
                InternalHookOutcome {
                    protocol_version: HOOK_PROTOCOL_VERSION,
                    event_id: post_event_id,
                    payload: HookOutcomePayload::ToolFinished {
                        feedback: Vec::<FeedbackItem>::new(),
                    },
                },
            ));
        }
    }
    let deliveries = Arc::new(deliveries);
    let mut order = (0..deliveries.len()).collect::<Vec<_>>();
    deterministic_shuffle(&mut order);
    let order = Arc::new(order);
    let handles = (0..8)
        .map(|worker| {
            let database = database.clone();
            let deliveries = Arc::clone(&deliveries);
            let order = Arc::clone(&order);
            thread::spawn(move || -> Result<(), StoreError> {
                let store = BrainStore::open(&database)?;
                for position in (worker..order.len()).step_by(8) {
                    let (event, outcome) = &deliveries[order[position]];
                    store.record_adapter_event(event, outcome, 1)?;
                }
                Ok(())
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle
            .join()
            .map_err(|_| "Q4 并发 worker panic".to_owned())?
            .map_err(display_error)?;
    }
    let store = BrainStore::open(&database).map_err(display_error)?;
    let expected_per_project = CONCURRENT_SESSION_COUNT * OPERATIONS_PER_SESSION;
    let mut correlations = BTreeMap::<String, BTreeSet<String>>::new();
    for project in ["qualification_q4_a", "qualification_q4_b"] {
        let records = store
            .recent_adapter_audit(project, 10_000)
            .map_err(display_error)?;
        if records.len() != expected_per_project {
            return Err(format!(
                "{project} 期望 {expected_per_project} 条事件，实际 {}",
                records.len()
            ));
        }
        for record in records {
            let event: InternalHookEvent =
                serde_json::from_str(&record.event_json).map_err(display_error)?;
            let (operation_id, phase) = match event.payload {
                HookEventPayload::ToolAboutToRun(tool) => (tool.operation_id, "pre"),
                HookEventPayload::ToolFinished(tool) => (tool.operation_id, "post"),
                _ => return Err("Q4 出现非工具事件".to_owned()),
            };
            correlations
                .entry(format!("{project}/{operation_id}"))
                .or_default()
                .insert(phase.to_owned());
        }
    }
    if correlations.len() != CONCURRENT_SESSION_COUNT * OPERATIONS_PER_SESSION
        || correlations
            .values()
            .any(|phases| phases.len() != 2 || !phases.contains("pre") || !phases.contains("post"))
    {
        return Err("并发交错后 operation pre/post 因果关联不完整".to_owned());
    }
    Ok(json!({
        "sessions": CONCURRENT_SESSION_COUNT,
        "operations_per_session": OPERATIONS_PER_SESSION,
        "event_count": deliveries.len(),
        "correlated_operations": correlations.len(),
        "worker_count": 8,
        "fixed_schedule_seed": "0x5eed_c0de_d15c_a11e",
    }))
}

fn case_provider_drift(root: &Path) -> Result<Value, String> {
    let directory = root.join("q5-provider");
    fs::create_dir_all(&directory).map_err(display_error)?;
    let artifact = directory.join("provider-artifact.bin");
    fs::write(&artifact, b"provider-v1").map_err(display_error)?;
    let pinned = provider::qualification_pin_artifact(&artifact).map_err(display_error)?;
    provider::qualification_validate_pinned_artifact(&artifact, &pinned).map_err(display_error)?;
    fs::write(&artifact, b"provider-v2-drifted").map_err(display_error)?;
    let drift_error = provider::qualification_validate_pinned_artifact(&artifact, &pinned)
        .expect_err("漂移后的 Provider artifact 必须被拒绝")
        .to_string();
    let current = provider::qualification_pin_artifact(&artifact).map_err(display_error)?;
    if current == pinned || !drift_error.contains("内容发生漂移") {
        return Err("Provider 同路径内容漂移未被正式 binding 校验路径识别".to_owned());
    }
    Ok(json!({
        "original_sha256": pinned,
        "drifted_sha256": current,
        "drift_detected": true,
        "trusted_snapshot_committed": false,
    }))
}

fn case_stop_loop_boundedness(root: &Path) -> Result<Value, String> {
    let case_root = root.join("q6-stop");
    fs::create_dir_all(&case_root).map_err(display_error)?;
    let store = BrainStore::open(&case_root.join("brain.db")).map_err(display_error)?;
    let mut config = fixture_config("qualification_q6");
    config.stop_reconcile = StopReconcileConfig {
        enabled: true,
        base: "HEAD".to_owned(),
        envelope: "missing-envelope.json".to_owned(),
    };
    let provider_trust = BTreeMap::new();
    let first: codex::CodexHookInput = serde_json::from_value(json!({
        "session_id": "session-q6",
        "cwd": case_root,
        "last_assistant_message": "done",
        "stop_hook_active": false
    }))
    .map_err(display_error)?;
    let first_output = codex::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::Stop,
        &first,
    )
    .map_err(display_error)?;
    if first_output.0.get("decision") != Some(&json!("block")) {
        return Err("首个未通过对账的 Stop 没有请求继续工作".to_owned());
    }
    let active: codex::CodexHookInput = serde_json::from_value(json!({
        "session_id": "session-q6",
        "cwd": case_root,
        "last_assistant_message": "retry",
        "stop_hook_active": true
    }))
    .map_err(display_error)?;
    for _ in 0..20 {
        let output = codex::handle_with_provider_trust(
            &case_root,
            &config,
            &store,
            &provider_trust,
            HookEvent::Stop,
            &active,
        )
        .map_err(display_error)?;
        if output.0.get("continue") != Some(&json!(true)) {
            return Err("vendor loop active 时 Stop 没有确定性放行，存在自递归风险".to_owned());
        }
    }
    let pi_output = pi::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::Stop,
        &first,
    );
    if pi_output.0.pointer("/continuation/supported") != Some(&json!(true)) {
        return Err("PI 未声明已验证的 Stop continuation".to_owned());
    }
    let opencode_output = opencode::handle_with_provider_trust(
        &case_root,
        &config,
        &store,
        &provider_trust,
        HookEvent::Stop,
        &first,
    );
    if opencode_output.0.pointer("/continuation/supported") != Some(&json!(false)) {
        return Err("opencode 被错误声明为支持 Stop continuation".to_owned());
    }
    Ok(json!({
        "initial_continue_request_count": 1,
        "vendor_loop_active_allow_count": 20,
        "maximum_project_brain_owned_reentry": 1,
            "pi_continuation_mode": "emulated",
        "opencode_continuation_supported": false,
    }))
}

fn case_long_session_stability(root: &Path) -> Result<Value, String> {
    let database = root.join("q7-long-session.db");
    let store = BrainStore::open(&database).map_err(display_error)?;
    let mut durations_micros = Vec::with_capacity(LONG_SESSION_EVENT_COUNT);
    for index in 0..LONG_SESSION_EVENT_COUNT {
        let event_id = format!("long-event-{index:05}");
        let event = fixture_session_event("qualification_q7", &event_id, "long-session");
        let outcome = fixture_session_outcome(&event_id);
        let started = Instant::now();
        store
            .record_adapter_event(&event, &outcome, 1)
            .map_err(display_error)?;
        durations_micros.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    drop(store);
    let reopened = BrainStore::open(&database).map_err(display_error)?;
    let rows = reopened
        .recent_adapter_audit("qualification_q7", 20_000)
        .map_err(display_error)?;
    let quartile = LONG_SESSION_EVENT_COUNT / 4;
    let first_p95 = percentile_95(&durations_micros[..quartile]);
    let last_p95 = percentile_95(&durations_micros[LONG_SESSION_EVENT_COUNT - quartile..]);
    let allowed_last_p95 = first_p95.saturating_mul(2).saturating_add(50_000);
    if rows.len() != LONG_SESSION_EVENT_COUNT || last_p95 > allowed_last_p95 {
        return Err(format!(
            "长会话不满足稳定性阈值：rows={} first_p95_us={first_p95} last_p95_us={last_p95} allowed_us={allowed_last_p95}",
            rows.len()
        ));
    }
    Ok(json!({
        "event_count": LONG_SESSION_EVENT_COUNT,
        "persisted_after_restart": rows.len(),
        "first_quartile_p95_us": first_p95,
        "last_quartile_p95_us": last_p95,
        "allowed_last_quartile_p95_us": allowed_last_p95,
        "lost_events": 0,
    }))
}

fn fixture_config(project_key: &str) -> BrainConfig {
    BrainConfig {
        schema_version: brain_core::CURRENT_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        project_name: "Production Qualification fixture".to_owned(),
        language_profiles: Vec::new(),
        semantic_providers: Vec::new(),
        finding_effect_mappings: Vec::new(),
        rules: Vec::new(),
        stop_reconcile: StopReconcileConfig::default(),
    }
}

fn fixture_session_event(project: &str, event_id: &str, session: &str) -> InternalHookEvent {
    fixture_event(
        project,
        event_id,
        session,
        HookEventPayload::SessionOpened(SessionOpened {
            reason: SessionOpenReason::Resume,
            previous_session_key: None,
        }),
    )
}

fn fixture_event(
    project: &str,
    event_id: &str,
    session: &str,
    payload: HookEventPayload,
) -> InternalHookEvent {
    InternalHookEvent {
        protocol_version: HOOK_PROTOCOL_VERSION,
        project_key: project.to_owned(),
        event_id: event_id.to_owned(),
        idempotency: IdempotencyMetadata {
            identity_quality: EventIdentityQuality::VendorStable,
        },
        adapter: AdapterIdentity {
            kind: AdapterKind::Codex,
            adapter_version: 1,
        },
        session_key: session.to_owned(),
        cwd: ".".to_owned(),
        turn_key: None,
        payload,
    }
}

fn fixture_session_outcome(event_id: &str) -> InternalHookOutcome {
    InternalHookOutcome {
        protocol_version: HOOK_PROTOCOL_VERSION,
        event_id: event_id.to_owned(),
        payload: HookOutcomePayload::SessionOpened { inject: Vec::new() },
    }
}

fn current_target() -> Result<QualificationTarget, AppError> {
    let executable = env::current_exe()?;
    let binary_sha256 = hash_file(&executable)?;
    let contract_manifest_hash = hash_json(&json!({
        "suite_id": QUALIFICATION_SUITE_ID,
        "suite_version": QUALIFICATION_SUITE_VERSION,
        "hook_protocol_version": HOOK_PROTOCOL_VERSION,
        "config_schema_version": brain_core::CURRENT_SCHEMA_VERSION,
        "database_schema_version": DATABASE_SCHEMA_VERSION,
        "adapters": {
            "codex": codex::capabilities(),
            "pi": pi::capabilities(),
            "opencode": opencode::capabilities(),
            "dsh": dsh::capabilities(),
        }
    }))?;
    let mut target = QualificationTarget {
        project_brain_version: env!("CARGO_PKG_VERSION").to_owned(),
        binary_sha256,
        contract_manifest_hash,
        database_schema_version: DATABASE_SCHEMA_VERSION,
        os: env::consts::OS.to_owned(),
        architecture: env::consts::ARCH.to_owned(),
        target_hash: String::new(),
    };
    target.target_hash = hash_json(&json!({
        "project_brain_version": target.project_brain_version,
        "binary_sha256": target.binary_sha256,
        "contract_manifest_hash": target.contract_manifest_hash,
        "database_schema_version": target.database_schema_version,
        "os": target.os,
        "architecture": target.architecture,
    }))?;
    Ok(target)
}

fn validate_external_id(label: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AppError::Qualification(format!(
            "{label} 不能为空、超过 160 字节或包含非 [A-Za-z0-9._-] 字符"
        )));
    }
    Ok(())
}

fn cleanup_fixture(state_root: &Path, fixture_root: &Path) -> Result<(), AppError> {
    let work_root = state_root.join("qualification-work");
    let canonical_parent = work_root.canonicalize()?;
    let canonical_fixture = fixture_root.canonicalize()?;
    if canonical_fixture.parent() != Some(canonical_parent.as_path()) {
        return Err(AppError::Qualification(format!(
            "拒绝删除越界 qualification fixture：{}",
            canonical_fixture.display()
        )));
    }
    fs::remove_dir_all(&canonical_fixture)?;
    let _ = fs::remove_dir(&canonical_parent);
    Ok(())
}

fn deterministic_shuffle(values: &mut [usize]) {
    let mut state = 0x5eed_c0de_d15c_a11e_u64;
    for index in (1..values.len()).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let swap = usize::try_from(state % u64::try_from(index + 1).unwrap_or(u64::MAX))
            .unwrap_or_default();
        values.swap(index, swap);
    }
}

fn percentile_95(values: &[u64]) -> u64 {
    let mut values = values.to_vec();
    values.sort_unstable();
    let index = (values.len().saturating_sub(1) * 95) / 100;
    values[index]
}

fn hash_file(path: &Path) -> Result<String, AppError> {
    Ok(format!("sha256_{}", hash_bytes(&fs::read(path)?)))
}

fn hash_json(value: &Value) -> Result<String, AppError> {
    Ok(format!(
        "sha256_{}",
        hash_bytes(&serde_json::to_vec(value)?)
    ))
}

fn verify_report_hash(report_json: &str, expected_hash: &str) -> Result<(), AppError> {
    let actual_hash = hash_bytes(report_json.as_bytes());
    if actual_hash != expected_hash {
        return Err(AppError::Qualification(format!(
            "资格报告哈希不匹配：expected={expected_hash} actual={actual_hash}"
        )));
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn unix_seconds() -> Result<i64, AppError> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn parse_state(value: &str) -> Result<QualificationState, String> {
    match value {
        "running" => Ok(QualificationState::Running),
        "qualified" => Ok(QualificationState::Qualified),
        "failed" => Ok(QualificationState::Failed),
        "inconclusive" => Ok(QualificationState::Inconclusive),
        _ => Err(format!("未知资格状态：{value}")),
    }
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        QualificationLedger, QualificationState, case_project_isolation, case_replay_idempotency,
        case_stop_loop_boundedness, current_target, fixture_config, run,
    };

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "project-brain-qualification-{label}-{}-{}",
            std::process::id(),
            super::unix_seconds().unwrap()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn focused_cases_prove_isolation_replay_and_stop_bound() {
        let root = temporary_root("focused");
        assert!(case_project_isolation(&root).is_ok());
        assert!(case_replay_idempotency(&root).is_ok());
        assert!(case_stop_loop_boundedness(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ledger_refuses_request_id_collision_and_replays_exact_report() {
        let root = temporary_root("ledger");
        let project = root.join("project");
        let install = root.join("install");
        fs::create_dir_all(project.join(".git")).unwrap();
        let config = fixture_config("qualification_test");
        let first = run(Some(&install), &project, &config, "request-1").unwrap();
        assert_eq!(first.status, QualificationState::Qualified);
        let replay = run(Some(&install), &project, &config, "request-1").unwrap();
        assert_eq!(replay.run_id, first.run_id);

        let ledger =
            QualificationLedger::open(&install.join("state/qualification.sqlite")).unwrap();
        let status = ledger.status(current_target().unwrap()).unwrap();
        assert!(status.qualified);
        assert!(
            ledger
                .connection
                .execute(
                    "UPDATE qualification_runs SET report_hash = 'tampered' WHERE run_id = ?1",
                    [&first.run_id],
                )
                .is_err()
        );
        assert!(
            ledger
                .connection
                .execute(
                    "DELETE FROM qualification_case_results WHERE run_id = ?1",
                    [&first.run_id],
                )
                .is_err()
        );
        assert!(
            ledger
                .replay_for_request("request-1", "sha256_different")
                .is_err()
        );
        drop(ledger);
        fs::remove_dir_all(root).unwrap();
    }
}
