use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::{
    ArtifactNode, EvidenceAuthority, EvidenceCoverage, EvidenceFinding, EvidenceFreshness,
    EvidencePlane, EvidenceProvider, EvidenceReference, EvidenceSnapshot, FindingAuthority,
    FindingSeverity,
};
use brain_store::EvidenceHeadRecord;
use serde::Serialize;

use crate::{artifact_store, error::AppError, git, provider};

const TEST_RUN_SCHEMA_VERSION: u32 = 1;
const DOTNET_TEST_CONTRACT_VERSION: u16 = 1;
const MAX_TRX_FILES: usize = 32;
const MAX_TRX_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) struct DotnetTestRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) install_root: Option<&'a Path>,
    pub(crate) project_key: &'a str,
    pub(crate) profile_id: &'a str,
    pub(crate) build_profile_id: &'a str,
    pub(crate) executable: &'a Path,
    pub(crate) target: &'a Path,
    pub(crate) test_assembly: &'a Path,
    pub(crate) trust_local_executable: bool,
    pub(crate) trust_repository_test_code: bool,
    pub(crate) timeout_seconds: u64,
    pub(crate) evidence_heads: &'a [EvidenceHeadRecord],
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TestStatus {
    Passed,
    Failed,
    Crashed,
    TimedOut,
    NoTests,
    ProviderFailed,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TestCoverage {
    Covered,
    Empty,
    Unknown,
}

#[derive(Debug, Serialize)]
struct DotnetTestContract {
    contract_version: u16,
    adapter: &'static str,
    profile_id: String,
    build_provider_id: String,
    build_snapshot_fingerprint: String,
    build_bundle_fingerprint: String,
    target: String,
    test_assembly: String,
    argv: Vec<String>,
    environment_policy: &'static str,
    network_policy: &'static str,
    execution_class: &'static str,
}

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
struct TrxCounters {
    total: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    error: u64,
    timeout: u64,
    aborted: u64,
    not_executed: u64,
}

impl TrxCounters {
    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            total: self.total.checked_add(other.total)?,
            executed: self.executed.checked_add(other.executed)?,
            passed: self.passed.checked_add(other.passed)?,
            failed: self.failed.checked_add(other.failed)?,
            error: self.error.checked_add(other.error)?,
            timeout: self.timeout.checked_add(other.timeout)?,
            aborted: self.aborted.checked_add(other.aborted)?,
            not_executed: self.not_executed.checked_add(other.not_executed)?,
        })
    }
}

#[derive(Debug, Serialize)]
struct TestProcessSummary {
    duration_ms: u128,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_sha256: String,
    stderr_sha256: String,
    output_truncated: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct DotnetTestReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    status: TestStatus,
    coverage: TestCoverage,
    toolchain_version: String,
    executable_sha256: String,
    contract: DotnetTestContract,
    process: TestProcessSummary,
    trx_files: usize,
    counters: Option<TrxCounters>,
    pub(crate) evidence: EvidenceSnapshot,
}

impl DotnetTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
    }
}

struct TestScratch {
    directory: PathBuf,
}

impl TestScratch {
    fn create() -> Result<Self, AppError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::Provider(format!("系统时间无效：{error}")))?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "project-brain-dotnet-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self { directory })
    }
}

impl Drop for TestScratch {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        if self.directory.starts_with(&temp) && self.directory != temp {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "固定 Test 合同线性保留 Build 绑定、CAS 物化、执行、TRX 分类与 Evidence 提交前验证"
)]
pub(crate) fn run_dotnet(request: &DotnetTestRequest<'_>) -> Result<DotnetTestReport, AppError> {
    if !request.trust_local_executable || !request.trust_repository_test_code {
        return Err(AppError::Provider(
            ".NET Test 会执行仓库测试代码，必须显式提供机器 executable 与 repository test code 两个信任位"
                .to_owned(),
        ));
    }
    validate_profile_id(request.profile_id)?;
    validate_profile_id(request.build_profile_id)?;
    let root = request.project_root.canonicalize()?;
    let target = resolve_project_file(&root, request.target)?;
    let target_display = project_relative_path(&root, &target)?;
    let assembly_display = validate_bundle_relative_path(request.test_assembly)?;
    if Path::new(&assembly_display)
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("dll"))
    {
        return Err(AppError::Provider(
            "test_assembly 必须是 Build bundle 内的 .dll".to_owned(),
        ));
    }

    let build_provider_id = format!("dotnet-build.{}", request.build_profile_id);
    let build_head = request
        .evidence_heads
        .iter()
        .find(|head| head.plane == EvidencePlane::Build && head.provider_id == build_provider_id)
        .ok_or_else(|| AppError::Provider("缺少指定 .NET Build Evidence head".to_owned()))?;
    if build_head.freshness != EvidenceFreshness::Fresh
        || build_head.snapshot.coverage != EvidenceCoverage::Complete
        || build_head.snapshot.provider.authority != EvidenceAuthority::Deterministic
        || !build_head.snapshot.findings.is_empty()
    {
        return Err(AppError::Provider(
            ".NET Test 只接受 fresh、complete、deterministic 且无 finding 的 Build head".to_owned(),
        ));
    }
    let source_fingerprint = git::worktree_fingerprint(&root)?;
    if build_head.snapshot.source_fingerprint != source_fingerprint {
        return Err(AppError::Provider(
            "当前源码与 Build Evidence source fingerprint 不一致".to_owned(),
        ));
    }
    let bundle_fingerprint = build_head
        .snapshot
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "runtime_artifact_bundle")
        .map(|artifact| artifact.content_fingerprint.clone())
        .ok_or_else(|| {
            AppError::Provider("Build Evidence 缺少可物化 artifact bundle".to_owned())
        })?;
    let bundle = artifact_store::verify_runtime_bundle(request.install_root, &bundle_fingerprint)?;
    if bundle.project_key() != request.project_key
        || bundle.build_provider_id() != build_provider_id
        || bundle.source_fingerprint() != source_fingerprint
        || bundle.build_target() != Some(target_display.as_str())
        || !bundle
            .entries()
            .iter()
            .any(|entry| entry.relative_path == assembly_display)
    {
        return Err(AppError::Provider(
            "Build bundle 与项目、provider、源码、target 或 test assembly 不一致".to_owned(),
        ));
    }

    let executable =
        provider::pin_external_executable(&root, request.executable, "dotnet executable")?;
    if !build_head
        .snapshot
        .provider
        .version
        .ends_with(&format!("+sha256.{}", executable.sha256))
    {
        return Err(AppError::Provider(
            "Test dotnet executable 与 Build Evidence 工具链哈希不一致".to_owned(),
        ));
    }
    let scratch = TestScratch::create()?;
    let materialized = scratch.directory.join("build-bundle");
    artifact_store::materialize_runtime_bundle(
        request.install_root,
        &bundle_fingerprint,
        &materialized,
    )?;
    let results = scratch.directory.join("results");
    fs::create_dir(&results)?;
    let test_assembly = materialized.join(Path::new(&assembly_display));
    let argv = vec![
        "vstest".to_owned(),
        provider::provider_cli_path(&test_assembly),
        "--Logger:trx".to_owned(),
        format!(
            "--ResultsDirectory:{}",
            provider::provider_cli_path(&results)
        ),
        "--nologo".to_owned(),
    ];
    let environment_storage = test_environment(&scratch.directory)?;
    let environment = environment_storage
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect::<Vec<_>>();
    let version_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--version".to_owned()],
        &scratch.directory,
        Some(&root),
        Duration::from_secs(request.timeout_seconds),
        &environment,
    )?;
    if !version_process.status.success()
        || version_process.stdout.truncated
        || version_process.stderr.truncated
    {
        return Err(AppError::Provider(
            "dotnet version probe 未完整成功".to_owned(),
        ));
    }
    let toolchain_version = provider::version_text(&version_process)?;
    let source_before = git::worktree_fingerprint(&root)?;
    let process = provider::run_process_with_environment_observing_timeout(
        &executable.canonical_path,
        None,
        &argv,
        &scratch.directory,
        Some(&root),
        Duration::from_secs(request.timeout_seconds),
        &environment,
    )?;
    let source_after = git::worktree_fingerprint(&root)?;
    if source_before != source_after || source_after != source_fingerprint {
        return Err(AppError::Provider(
            "源码在 Test Evidence 运行期间发生变化；结果已丢弃".to_owned(),
        ));
    }
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Test toolchain executable 在运行期间发生漂移".to_owned(),
        ));
    }
    artifact_store::verify_materialized_bundle(&bundle, &materialized)?;

    let trx = read_trx_results(&results);
    let output_truncated = process.stdout.truncated || process.stderr.truncated;
    let (status, coverage, counters, finding) = classify_result(&process, output_truncated, &trx);
    let trx_files = trx.as_ref().map_or(0, |(files, _)| *files);
    let counters = counters.or_else(|| trx.as_ref().ok().map(|(_, counters)| *counters));
    let provider_id = format!("dotnet-test.{}", request.profile_id);
    let contract = DotnetTestContract {
        contract_version: DOTNET_TEST_CONTRACT_VERSION,
        adapter: "dotnet-vstest",
        profile_id: request.profile_id.to_owned(),
        build_provider_id: build_provider_id.clone(),
        build_snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        build_bundle_fingerprint: bundle_fingerprint,
        target: target_display,
        test_assembly: assembly_display,
        argv: vec![
            "vstest".to_owned(),
            "<BUILD_BUNDLE>/<TEST_ASSEMBLY>".to_owned(),
            "--Logger:trx".to_owned(),
            "--ResultsDirectory:<TEST_ROOT>/results".to_owned(),
            "--nologo".to_owned(),
        ],
        environment_policy: "env_clear+adapter_allowlist+machine_scratch",
        network_policy: "adapter_no_restore_or_network;repository_test_code_not_os_sandboxed",
        execution_class: "repository_test_code",
    };
    let contract_bytes = serde_json::to_vec(&contract)?;
    let result_bytes = serde_json::to_vec(&(status, coverage, counters))?;
    let artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_contract",
            "contract",
            ".NET Test fixed execution contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_result_summary",
            "result-summary",
            ".NET TRX aggregate result",
            None,
            &result_bytes,
        ),
    ];
    let findings = finding.into_iter().collect();
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Test,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{toolchain_version}+sha256.{}", executable.sha256),
            contract_version: DOTNET_TEST_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        &source_fingerprint,
        if matches!(status, TestStatus::ProviderFailed | TestStatus::TimedOut) {
            EvidenceCoverage::Partial
        } else {
            EvidenceCoverage::Complete
        },
        vec![EvidenceReference {
            plane: EvidencePlane::Build,
            provider_id: build_provider_id,
            snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        }],
        artifacts,
        Vec::new(),
        findings,
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
    Ok(DotnetTestReport {
        schema_version: TEST_RUN_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        profile_id: request.profile_id.to_owned(),
        provider_id,
        status,
        coverage,
        toolchain_version,
        executable_sha256: executable.sha256,
        contract,
        process: TestProcessSummary {
            duration_ms: process.duration.as_millis(),
            exit_code: process.status.code(),
            timed_out: process.timed_out,
            stdout_bytes: process.stdout.total_bytes,
            stderr_bytes: process.stderr.total_bytes,
            stdout_sha256: process.stdout.sha256,
            stderr_sha256: process.stderr.sha256,
            output_truncated,
        },
        trx_files,
        counters,
        evidence,
    })
}

fn classify_result(
    process: &provider::ProcessResult,
    output_truncated: bool,
    trx: &Result<(usize, TrxCounters), String>,
) -> (
    TestStatus,
    TestCoverage,
    Option<TrxCounters>,
    Option<EvidenceFinding>,
) {
    if process.timed_out {
        return classified(
            TestStatus::TimedOut,
            TestCoverage::Unknown,
            None,
            "dotnet_test_timed_out",
            FindingSeverity::Warning,
            "dotnet vstest exceeded the fixed timeout; no project violation is inferred",
        );
    }
    if output_truncated {
        return classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "dotnet_test_output_truncated",
            FindingSeverity::Warning,
            "dotnet vstest output exceeded capture bounds; result is not authoritative",
        );
    }
    let Ok((_, counters)) = trx else {
        return classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "dotnet_test_result_unavailable",
            FindingSeverity::Warning,
            "dotnet vstest did not produce a valid bounded TRX result",
        );
    };
    if counters.executed == 0 {
        return classified(
            TestStatus::NoTests,
            TestCoverage::Empty,
            Some(*counters),
            "dotnet_test_no_tests",
            FindingSeverity::Warning,
            "dotnet vstest completed but executed no tests",
        );
    }
    if counters.failed > 0 || counters.error > 0 || counters.timeout > 0 || counters.aborted > 0 {
        return classified(
            TestStatus::Failed,
            TestCoverage::Covered,
            Some(*counters),
            "dotnet_test_failed",
            FindingSeverity::Error,
            "one or more .NET tests failed; TRX v1 cannot safely distinguish assertion failure from test or environment exceptions",
        );
    }
    if !process.status.success() {
        return classified(
            TestStatus::Crashed,
            TestCoverage::Covered,
            Some(*counters),
            "dotnet_test_process_failed",
            FindingSeverity::Error,
            "dotnet vstest returned non-zero without a failed TRX test; no project violation is inferred",
        );
    }
    (
        TestStatus::Passed,
        TestCoverage::Covered,
        Some(*counters),
        None,
    )
}

fn classified(
    status: TestStatus,
    coverage: TestCoverage,
    counters: Option<TrxCounters>,
    code: &str,
    severity: FindingSeverity,
    message: &str,
) -> (
    TestStatus,
    TestCoverage,
    Option<TrxCounters>,
    Option<EvidenceFinding>,
) {
    (
        status,
        coverage,
        counters,
        Some(EvidenceFinding {
            code: code.to_owned(),
            severity,
            authority: FindingAuthority::Advisory,
            message: message.to_owned(),
            artifact_id: None,
            path: None,
        }),
    )
}

fn read_trx_results(results: &Path) -> Result<(usize, TrxCounters), String> {
    let mut paths = fs::read_dir(results)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("trx"))
    });
    paths.sort();
    if paths.is_empty() || paths.len() > MAX_TRX_FILES {
        return Err("TRX file count is outside the fixed contract".to_owned());
    }
    let mut aggregate = TrxCounters::default();
    for path in &paths {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_TRX_BYTES
        {
            return Err("TRX is not a bounded regular file".to_owned());
        }
        aggregate = aggregate
            .checked_add(parse_trx_counters(
                &fs::read(path).map_err(|error| error.to_string())?,
            )?)
            .ok_or_else(|| "TRX counters overflow".to_owned())?;
    }
    Ok((paths.len(), aggregate))
}

fn parse_trx_counters(bytes: &[u8]) -> Result<TrxCounters, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "TRX is not UTF-8".to_owned())?;
    let starts = text.match_indices("<Counters").collect::<Vec<_>>();
    if starts.len() != 1 {
        return Err("TRX must contain exactly one Counters element".to_owned());
    }
    let tag = &text[starts[0].0..];
    let tag = tag
        .get(
            ..tag
                .find('>')
                .ok_or_else(|| "TRX Counters tag is incomplete".to_owned())?
                + 1,
        )
        .ok_or_else(|| "TRX Counters tag boundary is invalid".to_owned())?;
    let value = |name: &str, required: bool| -> Result<u64, String> {
        let marker = format!(" {name}=\"");
        let Some(rest) = tag.split_once(&marker).map(|(_, rest)| rest) else {
            return if required {
                Err(format!("TRX Counters is missing {name}"))
            } else {
                Ok(0)
            };
        };
        let raw = rest
            .split_once('"')
            .map(|(raw, _)| raw)
            .ok_or_else(|| format!("TRX Counters {name} is unterminated"))?;
        raw.parse::<u64>()
            .map_err(|_| format!("TRX Counters {name} is not u64"))
    };
    let counters = TrxCounters {
        total: value("total", true)?,
        executed: value("executed", true)?,
        passed: value("passed", true)?,
        failed: value("failed", true)?,
        error: value("error", false)?,
        timeout: value("timeout", false)?,
        aborted: value("aborted", false)?,
        not_executed: value("notExecuted", false)?,
    };
    if counters.executed > counters.total
        || counters.passed > counters.executed
        || counters.failed > counters.executed
        || counters.passed.saturating_add(counters.failed) > counters.executed
    {
        return Err("TRX Counters values are inconsistent".to_owned());
    }
    Ok(counters)
}

fn test_environment(root: &Path) -> Result<Vec<(&'static str, PathBuf)>, AppError> {
    let variables = [
        ("HOME", root.join("home")),
        ("USERPROFILE", root.join("home")),
        ("APPDATA", root.join("appdata")),
        ("LOCALAPPDATA", root.join("localappdata")),
        ("DOTNET_CLI_HOME", root.join("dotnet-home")),
        ("NUGET_PACKAGES", root.join("nuget-packages")),
        ("TEMP", root.join("temp")),
        ("TMP", root.join("temp")),
    ];
    for (_, path) in &variables {
        fs::create_dir_all(path)?;
    }
    Ok(variables.into_iter().collect())
}

fn validate_profile_id(profile: &str) -> Result<(), AppError> {
    if profile.is_empty()
        || profile.len() > 64
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AppError::Provider("Test profile ID 格式非法".to_owned()));
    }
    Ok(())
}

fn resolve_project_file(root: &Path, input: &Path) -> Result<PathBuf, AppError> {
    if input.is_absolute()
        || input
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::Provider(
            "Test target 必须是项目内规范相对路径".to_owned(),
        ));
    }
    let target = root.join(input).canonicalize()?;
    if !target.starts_with(root)
        || target
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("csproj"))
        || !fs::symlink_metadata(&target)?.is_file()
    {
        return Err(AppError::Provider(
            "Test target 必须是项目内普通 .csproj 文件".to_owned(),
        ));
    }
    Ok(target)
}

fn project_relative_path(root: &Path, path: &Path) -> Result<String, AppError> {
    path.strip_prefix(root)
        .map_err(|_| AppError::Provider("Test target 越出项目根".to_owned()))?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
        .ok_or_else(|| AppError::Provider("Test target 路径不是 UTF-8".to_owned()))
}

fn validate_bundle_relative_path(path: &Path) -> Result<String, AppError> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::Provider(
            "test_assembly 必须是 bundle 内规范相对路径".to_owned(),
        ));
    }
    path.components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
        .ok_or_else(|| AppError::Provider("test_assembly 路径不是 UTF-8".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trx_parser_distinguishes_empty_pass_and_failure() {
        let passed = parse_trx_counters(
            br#"<TestRun><ResultSummary><Counters total="2" executed="2" passed="2" failed="0" /></ResultSummary></TestRun>"#,
        )
        .unwrap();
        assert_eq!(passed.executed, 2);
        assert_eq!(passed.passed, 2);

        let empty = parse_trx_counters(
            br#"<TestRun><ResultSummary><Counters total="0" executed="0" passed="0" failed="0" notExecuted="0" /></ResultSummary></TestRun>"#,
        )
        .unwrap();
        assert_eq!(empty.executed, 0);

        let failed = parse_trx_counters(
            br#"<TestRun><ResultSummary><Counters total="3" executed="3" passed="2" failed="1" error="0" /></ResultSummary></TestRun>"#,
        )
        .unwrap();
        assert_eq!(failed.failed, 1);
    }

    #[test]
    fn test_argv_and_paths_are_adapter_owned_and_bounded() {
        assert!(validate_profile_id("game-debug").is_ok());
        assert!(validate_profile_id("../escape").is_err());
        assert_eq!(
            validate_bundle_relative_path(Path::new("Game.Tests.dll")).unwrap(),
            "Game.Tests.dll"
        );
        assert!(validate_bundle_relative_path(Path::new("../Game.Tests.dll")).is_err());
    }
}
