use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::{
    ArtifactNode, EvidenceAuthority, EvidenceCoverage, EvidenceFinding, EvidenceFreshness,
    EvidenceInputManifestV1, EvidencePlane, EvidenceProvider, EvidenceReference, EvidenceSnapshot,
    FindingAuthority, FindingSeverity,
};
use brain_store::EvidenceHeadRecord;
use serde::{Deserialize, Serialize};

use crate::{artifact_store, error::AppError, evidence_inputs, git, provider, source_stage};

const TEST_RUN_SCHEMA_VERSION: u32 = 1;
const DOTNET_TEST_CONTRACT_VERSION: u16 = 1;
const RUST_TEST_CONTRACT_VERSION: u16 = 1;
const PYTHON_TEST_CONTRACT_VERSION: u16 = 1;
const MAX_TRX_FILES: usize = 32;
const MAX_TRX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PYTHON_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PYTHON_RESULT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PYTHON_TESTS: usize = 10_000;
const PYTHON_RESULT_FILE: &str = "python-test-result-v1.json";
const PYTHON_TEST_BOOTSTRAP: &str = r#"
import importlib
import inspect
import json
import sys
from pathlib import Path


def main():
    source_root, manifest_path, result_path = sys.argv[1:4]
    manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8"))
    sys.path.insert(0, source_root)
    results = []
    for case in manifest["tests"]:
        status = "error"
        try:
            module = importlib.import_module(case["module"])
            function = getattr(module, case["function"])
            if not inspect.isfunction(function) or function.__module__ != module.__name__:
                raise TypeError("declared test is not a module-owned function")
            if inspect.iscoroutinefunction(function) or inspect.isgeneratorfunction(function):
                raise TypeError("declared test must be synchronous")
            returned = function()
            if returned is not None:
                raise TypeError("declared test must return None")
            status = "passed"
        except AssertionError:
            status = "assertion_failed"
        except BaseException:
            status = "error"
        results.append({
            "module": case["module"],
            "function": case["function"],
            "status": status,
        })
    payload = {"schema_version": 1, "tests": results}
    with Path(result_path).open("w", encoding="utf-8", newline="\n") as stream:
        stream.write(
            json.dumps(payload, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        )


main()
"#;

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

pub(crate) struct RustTestRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) project_key: &'a str,
    pub(crate) profile_id: &'a str,
    pub(crate) build_profile_id: &'a str,
    pub(crate) executable: &'a Path,
    pub(crate) manifest: &'a Path,
    pub(crate) trust_local_executable: bool,
    pub(crate) trust_repository_test_code: bool,
    pub(crate) timeout_seconds: u64,
    pub(crate) evidence_heads: &'a [EvidenceHeadRecord],
}

pub(crate) struct PythonTestRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) project_key: &'a str,
    pub(crate) profile_id: &'a str,
    pub(crate) build_profile_id: &'a str,
    pub(crate) executable: &'a Path,
    pub(crate) source_root: &'a Path,
    pub(crate) manifest: &'a Path,
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
    Partial,
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

#[derive(Debug, Serialize)]
struct RustTestContract {
    contract_version: u16,
    adapter: &'static str,
    profile_id: String,
    build_provider_id: String,
    build_snapshot_fingerprint: String,
    manifest: String,
    argv: Vec<String>,
    environment_policy: &'static str,
    network_policy: &'static str,
    execution_class: &'static str,
}

#[derive(Debug, Serialize)]
struct PythonTestContract {
    contract_version: u16,
    adapter: &'static str,
    profile_id: String,
    build_provider_id: String,
    build_snapshot_fingerprint: String,
    source_root: String,
    manifest: String,
    input_manifest_sha256: String,
    bootstrap_sha256: String,
    argv: Vec<String>,
    environment_policy: &'static str,
    network_policy: &'static str,
    execution_class: &'static str,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PythonTestManifest {
    schema_version: u32,
    tests: Vec<PythonTestCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct PythonTestCase {
    module: String,
    function: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PythonDeclaredStatus {
    Passed,
    AssertionFailed,
    Error,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PythonTestResult {
    schema_version: u32,
    tests: Vec<PythonTestResultCase>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PythonTestResultCase {
    module: String,
    function: String,
    status: PythonDeclaredStatus,
}

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
struct PythonTestSummary {
    declared: u64,
    passed: u64,
    assertion_failed: u64,
    error: u64,
}

#[derive(Debug, Default, Clone, Copy, Serialize, PartialEq, Eq)]
struct RustTestSummary {
    result_sections: u64,
    passed: u64,
    failed: u64,
    ignored: u64,
    measured: u64,
    filtered_out: u64,
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
    input_manifest: EvidenceInputManifestV1,
}

impl DotnetTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
    }

    pub(crate) fn input_manifest(&self) -> &EvidenceInputManifestV1 {
        &self.input_manifest
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RustTestReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    status: TestStatus,
    coverage: TestCoverage,
    toolchain_version: String,
    executable_sha256: String,
    contract: RustTestContract,
    process: TestProcessSummary,
    summary: Option<RustTestSummary>,
    pub(crate) evidence: EvidenceSnapshot,
    input_manifest: EvidenceInputManifestV1,
}

impl RustTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
    }

    pub(crate) fn input_manifest(&self) -> &EvidenceInputManifestV1 {
        &self.input_manifest
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PythonTestReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    status: TestStatus,
    coverage: TestCoverage,
    toolchain_version: String,
    executable_sha256: String,
    contract: PythonTestContract,
    process: TestProcessSummary,
    summary: Option<PythonTestSummary>,
    pub(crate) evidence: EvidenceSnapshot,
    input_manifest: EvidenceInputManifestV1,
}

impl PythonTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
    }

    pub(crate) fn input_manifest(&self) -> &EvidenceInputManifestV1 {
        &self.input_manifest
    }
}

struct TestScratch {
    directory: PathBuf,
}

impl TestScratch {
    fn create(adapter: &str) -> Result<Self, AppError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::Provider(format!("系统时间无效：{error}")))?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "project-brain-{adapter}-test-{}-{nonce}",
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
    let scratch = TestScratch::create("dotnet")?;
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
    let environment_storage = dotnet_test_environment(&scratch.directory)?;
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
        evidence_coverage_for_test(status, coverage),
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
    let input_manifest = evidence_inputs::resolve_conservative_for_source(
        &root,
        request.project_key,
        request.profile_id,
        &provider_id,
        u32::from(DOTNET_TEST_CONTRACT_VERSION),
        &source_fingerprint,
    )?;
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
        input_manifest,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "固定 Cargo Test 合同线性保留 Build 绑定、隔离 target、结果分类与 TOCTOU 校验"
)]
pub(crate) fn run_rust(request: &RustTestRequest<'_>) -> Result<RustTestReport, AppError> {
    if !request.trust_local_executable || !request.trust_repository_test_code {
        return Err(AppError::Provider(
            "Rust Test 会执行 build.rs、proc macro 与仓库测试代码，必须显式提供机器 executable 与 repository test code 两个信任位"
                .to_owned(),
        ));
    }
    validate_profile_id(request.profile_id)?;
    validate_profile_id(request.build_profile_id)?;
    let root = request.project_root.canonicalize()?;
    let manifest = resolve_rust_manifest(&root, request.manifest)?;
    let manifest_display = project_relative_path(&root, &manifest)?;
    let build_provider_id = format!("cargo-build.{}", request.build_profile_id);
    let build_head = request
        .evidence_heads
        .iter()
        .find(|head| head.plane == EvidencePlane::Build && head.provider_id == build_provider_id)
        .ok_or_else(|| AppError::Provider("缺少指定 Rust Build Evidence head".to_owned()))?;
    if build_head.freshness != EvidenceFreshness::Fresh
        || build_head.snapshot.coverage != EvidenceCoverage::Complete
        || build_head.snapshot.provider.authority != EvidenceAuthority::Deterministic
        || !build_head.snapshot.findings.is_empty()
    {
        return Err(AppError::Provider(
            "Rust Test 只接受 fresh、complete、deterministic 且无 finding 的 Build head".to_owned(),
        ));
    }
    let source_fingerprint = git::worktree_fingerprint(&root)?;
    if build_head.snapshot.source_fingerprint != source_fingerprint {
        return Err(AppError::Provider(
            "当前源码与 Rust Build Evidence source fingerprint 不一致".to_owned(),
        ));
    }
    let expected_target = brain_evidence::content_fingerprint(manifest_display.as_bytes());
    if !build_head.snapshot.artifacts.iter().any(|artifact| {
        artifact.kind == "build_target" && artifact.content_fingerprint == expected_target
    }) {
        return Err(AppError::Provider(
            "Rust Build Evidence 未绑定当前 Cargo.toml target；请重新生成 Build Evidence"
                .to_owned(),
        ));
    }

    let executable =
        provider::pin_external_executable(&root, request.executable, "cargo executable")?;
    if !build_head
        .snapshot
        .provider
        .version
        .ends_with(&format!("+sha256.{}", executable.sha256))
    {
        return Err(AppError::Provider(
            "Test cargo executable 与 Build Evidence 工具链哈希不一致".to_owned(),
        ));
    }
    let scratch = TestScratch::create("cargo")?;
    let target = scratch.directory.join("target");
    fs::create_dir(&target)?;
    let argv = vec![
        "test".to_owned(),
        "--manifest-path".to_owned(),
        provider::provider_cli_path(&manifest),
        "--workspace".to_owned(),
        "--all-targets".to_owned(),
        "--frozen".to_owned(),
        "--target-dir".to_owned(),
        provider::provider_cli_path(&target),
    ];
    let environment_storage = rust_test_environment(&scratch.directory)?;
    let environment = environment_storage
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect::<Vec<_>>();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let version_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--version".to_owned()],
        &scratch.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    if !version_process.status.success()
        || version_process.stdout.truncated
        || version_process.stderr.truncated
    {
        return Err(AppError::Provider(
            "cargo version probe 未完整成功".to_owned(),
        ));
    }
    let toolchain_version = provider::version_text(&version_process)?;
    if !toolchain_version.to_ascii_lowercase().contains("cargo") {
        return Err(AppError::Provider(
            "Rust Test executable version probe 不是 cargo".to_owned(),
        ));
    }
    let source_before = git::worktree_fingerprint(&root)?;
    let process = provider::run_process_with_environment_observing_timeout(
        &executable.canonical_path,
        None,
        &argv,
        &scratch.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    let source_after = git::worktree_fingerprint(&root)?;
    if source_before != source_after || source_after != source_fingerprint {
        return Err(AppError::Provider(
            "源码在 Rust Test Evidence 运行期间发生变化；结果已丢弃".to_owned(),
        ));
    }
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Rust Test toolchain executable 在运行期间发生漂移".to_owned(),
        ));
    }

    let output_truncated = process.stdout.truncated || process.stderr.truncated;
    let parsed = parse_rust_test_summary(&process.stdout.bytes, &process.stderr.bytes);
    let (status, coverage, summary, finding) =
        classify_rust_result(&process, output_truncated, &parsed);
    let provider_id = format!("cargo-test.{}", request.profile_id);
    let contract = RustTestContract {
        contract_version: RUST_TEST_CONTRACT_VERSION,
        adapter: "cargo-test",
        profile_id: request.profile_id.to_owned(),
        build_provider_id: build_provider_id.clone(),
        build_snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        manifest: manifest_display,
        argv: vec![
            "test".to_owned(),
            "--manifest-path".to_owned(),
            "<PROJECT_ROOT>/Cargo.toml".to_owned(),
            "--workspace".to_owned(),
            "--all-targets".to_owned(),
            "--frozen".to_owned(),
            "--target-dir".to_owned(),
            "<TEST_ROOT>/target".to_owned(),
        ],
        environment_policy: "env_clear+adapter_allowlist+machine_scratch",
        network_policy: "offline_frozen;repository_test_code_not_os_sandboxed",
        execution_class: "repository_test_code",
    };
    let contract_bytes = serde_json::to_vec(&contract)?;
    let result_bytes = serde_json::to_vec(&(status, coverage, summary))?;
    let artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_contract",
            "contract",
            "Rust Test fixed execution contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_result_summary",
            "result-summary",
            "Rust libtest aggregate result",
            None,
            &result_bytes,
        ),
    ];
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Test,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{toolchain_version}+sha256.{}", executable.sha256),
            contract_version: RUST_TEST_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        &source_fingerprint,
        evidence_coverage_for_test(status, coverage),
        vec![EvidenceReference {
            plane: EvidencePlane::Build,
            provider_id: build_provider_id,
            snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        }],
        artifacts,
        Vec::new(),
        finding.into_iter().collect(),
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
    let input_manifest = evidence_inputs::resolve_conservative_for_source(
        &root,
        request.project_key,
        request.profile_id,
        &provider_id,
        u32::from(RUST_TEST_CONTRACT_VERSION),
        &source_fingerprint,
    )?;
    Ok(RustTestReport {
        schema_version: TEST_RUN_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        profile_id: request.profile_id.to_owned(),
        provider_id,
        status,
        coverage,
        toolchain_version,
        executable_sha256: executable.sha256,
        contract,
        process: summarize_process(&process),
        summary,
        evidence,
        input_manifest,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "固定 Python Test 合同线性保留 Build 绑定、清单验证、源码 staging、结构化结果与 TOCTOU 校验"
)]
pub(crate) fn run_python(request: &PythonTestRequest<'_>) -> Result<PythonTestReport, AppError> {
    if !request.trust_local_executable || !request.trust_repository_test_code {
        return Err(AppError::Provider(
            "Python Test 会 import 并执行仓库测试函数，必须显式提供机器 executable 与 repository test code 两个信任位"
                .to_owned(),
        ));
    }
    validate_profile_id(request.profile_id)?;
    validate_profile_id(request.build_profile_id)?;
    let root = request.project_root.canonicalize()?;
    let source_root = resolve_python_source_root(&root, request.source_root)?;
    let source_root_display = project_relative_path_or_dot(&root, &source_root)?;
    let manifest_path = resolve_python_manifest(&root, &source_root, request.manifest)?;
    let manifest_display = project_relative_path(&root, &manifest_path)?;
    let repository_files = git::repository_files(&root)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if !repository_files.contains(&manifest_display) {
        return Err(AppError::Provider(
            "Python Test manifest 必须属于 Git Source 文件集合".to_owned(),
        ));
    }
    let (manifest, manifest_source_bytes) =
        read_python_test_manifest(&manifest_path, &source_root, &repository_files, &root)?;
    let canonical_manifest_bytes = serde_json::to_vec(&manifest)?;

    let build_provider_id = format!("python-compile.{}", request.build_profile_id);
    let build_head = request
        .evidence_heads
        .iter()
        .find(|head| head.plane == EvidencePlane::Build && head.provider_id == build_provider_id)
        .ok_or_else(|| AppError::Provider("缺少指定 Python Build Evidence head".to_owned()))?;
    if build_head.freshness != EvidenceFreshness::Fresh
        || build_head.snapshot.coverage != EvidenceCoverage::Complete
        || build_head.snapshot.provider.authority != EvidenceAuthority::Deterministic
        || !build_head.snapshot.findings.is_empty()
    {
        return Err(AppError::Provider(
            "Python Test 只接受 fresh、complete、deterministic 且无 finding 的 Build head"
                .to_owned(),
        ));
    }
    let source_fingerprint = git::worktree_fingerprint(&root)?;
    if build_head.snapshot.source_fingerprint != source_fingerprint {
        return Err(AppError::Provider(
            "当前源码与 Python Build Evidence source fingerprint 不一致".to_owned(),
        ));
    }
    let expected_target = brain_evidence::content_fingerprint(source_root_display.as_bytes());
    if !build_head.snapshot.artifacts.iter().any(|artifact| {
        artifact.kind == "build_target" && artifact.content_fingerprint == expected_target
    }) {
        return Err(AppError::Provider(
            "Python Build Evidence 未绑定当前 source_root；请重新生成 Build Evidence".to_owned(),
        ));
    }

    let executable =
        provider::pin_external_executable(&root, request.executable, "Python executable")?;
    if !build_head
        .snapshot
        .provider
        .version
        .ends_with(&format!("+sha256.{}", executable.sha256))
    {
        return Err(AppError::Provider(
            "Test Python executable 与 Build Evidence 工具链哈希不一致".to_owned(),
        ));
    }

    let scratch = TestScratch::create("python")?;
    let staged_project = scratch.directory.join("project");
    let staged_manifest = source_stage::stage_project(&root, &staged_project)?;
    source_stage::verify_staged_source(&staged_manifest, &staged_project, &[])?;
    let staged_source_root = if source_root_display == "." {
        staged_project.clone()
    } else {
        staged_project.join(Path::new(&source_root_display))
    };
    if !staged_source_root.is_dir() {
        return Err(AppError::Provider(
            "Python Test staged source_root 不存在".to_owned(),
        ));
    }
    let adapter_manifest = scratch.directory.join("input-manifest-v1.json");
    fs::write(&adapter_manifest, &canonical_manifest_bytes)?;
    let result_path = scratch.directory.join(PYTHON_RESULT_FILE);
    let argv = vec![
        "-I".to_owned(),
        "-S".to_owned(),
        "-B".to_owned(),
        "-X".to_owned(),
        "utf8".to_owned(),
        "-c".to_owned(),
        PYTHON_TEST_BOOTSTRAP.to_owned(),
        provider::provider_cli_path(&staged_source_root),
        provider::provider_cli_path(&adapter_manifest),
        provider::provider_cli_path(&result_path),
    ];
    let environment_storage = python_test_environment(&scratch.directory)?;
    let environment = environment_storage
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect::<Vec<_>>();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let version_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--version".to_owned()],
        &scratch.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    if !version_process.status.success()
        || version_process.stdout.truncated
        || version_process.stderr.truncated
    {
        return Err(AppError::Provider(
            "Python version probe 未完整成功".to_owned(),
        ));
    }
    let toolchain_version = provider::version_text(&version_process)?;
    if !toolchain_version.to_ascii_lowercase().contains("python") {
        return Err(AppError::Provider(
            "Python Test executable version probe 不是 Python".to_owned(),
        ));
    }

    let source_before = git::worktree_fingerprint(&root)?;
    let process = provider::run_process_with_environment_observing_timeout(
        &executable.canonical_path,
        None,
        &argv,
        &staged_source_root,
        Some(&root),
        timeout,
        &environment,
    )?;
    let source_after = git::worktree_fingerprint(&root)?;
    if source_before != source_after || source_after != source_fingerprint {
        return Err(AppError::Provider(
            "源码在 Python Test Evidence 运行期间发生变化；结果已丢弃".to_owned(),
        ));
    }
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Python Test toolchain executable 在运行期间发生漂移".to_owned(),
        ));
    }
    source_stage::verify_staged_source(&staged_manifest, &staged_project, &[])?;

    let parsed = read_python_test_result(&result_path, &manifest);
    let (status, coverage, summary, findings) = classify_python_result(&process, &parsed);
    let provider_id = format!("python-test.{}", request.profile_id);
    let contract = PythonTestContract {
        contract_version: PYTHON_TEST_CONTRACT_VERSION,
        adapter: "python-manifest-test",
        profile_id: request.profile_id.to_owned(),
        build_provider_id: build_provider_id.clone(),
        build_snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        source_root: source_root_display,
        manifest: manifest_display.clone(),
        input_manifest_sha256: brain_evidence::content_fingerprint(&manifest_source_bytes),
        bootstrap_sha256: brain_evidence::content_fingerprint(PYTHON_TEST_BOOTSTRAP.as_bytes()),
        argv: vec![
            "-I".to_owned(),
            "-S".to_owned(),
            "-B".to_owned(),
            "-X".to_owned(),
            "utf8".to_owned(),
            "-c".to_owned(),
            "<ADAPTER_BOOTSTRAP>".to_owned(),
            "<STAGED_SOURCE_ROOT>".to_owned(),
            "<ADAPTER_MANIFEST>".to_owned(),
            "<TEST_RESULT>".to_owned(),
        ],
        environment_policy: "env_clear+adapter_allowlist+machine_scratch+isolated_mode",
        network_policy: "adapter_no_install_or_network;repository_test_code_not_os_sandboxed",
        execution_class: "repository_test_code",
    };
    let contract_bytes = serde_json::to_vec(&contract)?;
    let result_bytes = serde_json::to_vec(&(status, coverage, summary))?;
    let staged_manifest_bytes = serde_json::to_vec(&staged_manifest)?;
    let artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_contract",
            "contract",
            "Python Test fixed execution contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_input_manifest",
            "input-manifest",
            "Validated repository Python test manifest",
            Some(&manifest_display),
            &canonical_manifest_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "staged_source_manifest",
            "staged-source",
            "Python Test physical Git Source staging manifest",
            None,
            &staged_manifest_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_result_summary",
            "result-summary",
            "Python adapter-owned aggregate result",
            None,
            &result_bytes,
        ),
    ];
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Test,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{toolchain_version}+sha256.{}", executable.sha256),
            contract_version: PYTHON_TEST_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        &source_fingerprint,
        evidence_coverage_for_test(status, coverage),
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
    let input_manifest = evidence_inputs::resolve_conservative_for_source(
        &root,
        request.project_key,
        request.profile_id,
        &provider_id,
        u32::from(PYTHON_TEST_CONTRACT_VERSION),
        &source_fingerprint,
    )?;
    Ok(PythonTestReport {
        schema_version: TEST_RUN_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        profile_id: request.profile_id.to_owned(),
        provider_id,
        status,
        coverage,
        toolchain_version,
        executable_sha256: executable.sha256,
        contract,
        process: summarize_process(&process),
        summary,
        evidence,
        input_manifest,
    })
}

fn summarize_process(process: &provider::ProcessResult) -> TestProcessSummary {
    TestProcessSummary {
        duration_ms: process.duration.as_millis(),
        exit_code: process.status.code(),
        timed_out: process.timed_out,
        stdout_bytes: process.stdout.total_bytes,
        stderr_bytes: process.stderr.total_bytes,
        stdout_sha256: process.stdout.sha256.clone(),
        stderr_sha256: process.stderr.sha256.clone(),
        output_truncated: process.stdout.truncated || process.stderr.truncated,
    }
}

fn evidence_coverage_for_test(status: TestStatus, coverage: TestCoverage) -> EvidenceCoverage {
    if matches!(status, TestStatus::ProviderFailed | TestStatus::TimedOut)
        || coverage == TestCoverage::Partial
    {
        EvidenceCoverage::Partial
    } else {
        EvidenceCoverage::Complete
    }
}

fn parse_rust_test_summary(stdout: &[u8], stderr: &[u8]) -> Result<RustTestSummary, String> {
    let stdout = std::str::from_utf8(stdout).map_err(|_| "cargo test stdout is not UTF-8")?;
    let stderr = std::str::from_utf8(stderr).map_err(|_| "cargo test stderr is not UTF-8")?;
    let mut summary = RustTestSummary::default();
    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("test result:") else {
            continue;
        };
        let (_, counters) = rest
            .split_once('.')
            .ok_or_else(|| "cargo test result line is malformed".to_owned())?;
        let mut values = [None; 5];
        for field in counters.split(';').map(str::trim) {
            for (index, label) in ["passed", "failed", "ignored", "measured", "filtered out"]
                .iter()
                .enumerate()
            {
                if let Some(raw) = field.strip_suffix(label).map(str::trim) {
                    let value = raw
                        .parse::<u64>()
                        .map_err(|_| "cargo test result counter is not u64".to_owned())?;
                    values[index] = Some(value);
                }
            }
        }
        let [passed, failed, ignored, measured, filtered_out] = values;
        summary.result_sections = summary
            .result_sections
            .checked_add(1)
            .ok_or_else(|| "cargo test result section count overflow".to_owned())?;
        summary.passed = summary
            .passed
            .checked_add(passed.ok_or("cargo test result is missing passed")?)
            .ok_or_else(|| "cargo test passed counter overflow".to_owned())?;
        summary.failed = summary
            .failed
            .checked_add(failed.ok_or("cargo test result is missing failed")?)
            .ok_or_else(|| "cargo test failed counter overflow".to_owned())?;
        summary.ignored = summary
            .ignored
            .checked_add(ignored.ok_or("cargo test result is missing ignored")?)
            .ok_or_else(|| "cargo test ignored counter overflow".to_owned())?;
        summary.measured = summary
            .measured
            .checked_add(measured.ok_or("cargo test result is missing measured")?)
            .ok_or_else(|| "cargo test measured counter overflow".to_owned())?;
        summary.filtered_out = summary
            .filtered_out
            .checked_add(filtered_out.ok_or("cargo test result is missing filtered out")?)
            .ok_or_else(|| "cargo test filtered counter overflow".to_owned())?;
    }
    if summary.result_sections == 0 {
        Err("cargo test produced no bounded libtest result sections".to_owned())
    } else {
        Ok(summary)
    }
}

fn classify_rust_result(
    process: &provider::ProcessResult,
    output_truncated: bool,
    parsed: &Result<RustTestSummary, String>,
) -> (
    TestStatus,
    TestCoverage,
    Option<RustTestSummary>,
    Option<EvidenceFinding>,
) {
    if process.timed_out {
        return rust_classified(
            TestStatus::TimedOut,
            TestCoverage::Unknown,
            None,
            "rust_test_timed_out",
            FindingSeverity::Warning,
            "cargo test exceeded the fixed timeout; no project violation is inferred",
        );
    }
    if output_truncated {
        return rust_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "rust_test_output_truncated",
            FindingSeverity::Warning,
            "cargo test output exceeded capture bounds; result is not authoritative",
        );
    }
    let Ok(summary) = parsed else {
        return rust_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "rust_test_result_unavailable",
            FindingSeverity::Warning,
            "cargo test did not produce a complete bounded libtest summary",
        );
    };
    let executed = summary
        .passed
        .saturating_add(summary.failed)
        .saturating_add(summary.measured);
    if executed == 0 {
        return rust_classified(
            TestStatus::NoTests,
            TestCoverage::Empty,
            Some(*summary),
            "rust_test_no_tests",
            FindingSeverity::Warning,
            "cargo test completed but executed no tests",
        );
    }
    let observed_coverage = if summary.ignored > 0 || summary.filtered_out > 0 {
        TestCoverage::Partial
    } else {
        TestCoverage::Covered
    };
    if summary.failed > 0 {
        return rust_classified(
            TestStatus::Failed,
            observed_coverage,
            Some(*summary),
            "rust_test_failed",
            FindingSeverity::Error,
            "one or more Rust tests failed; libtest text v1 cannot safely distinguish assertion failure from panic or harness error",
        );
    }
    if !process.status.success() {
        return rust_classified(
            TestStatus::Crashed,
            observed_coverage,
            Some(*summary),
            "rust_test_process_failed",
            FindingSeverity::Error,
            "cargo test returned non-zero without a failed libtest summary; no assertion violation is inferred",
        );
    }
    (TestStatus::Passed, observed_coverage, Some(*summary), None)
}

fn rust_classified(
    status: TestStatus,
    coverage: TestCoverage,
    summary: Option<RustTestSummary>,
    code: &str,
    severity: FindingSeverity,
    message: &str,
) -> (
    TestStatus,
    TestCoverage,
    Option<RustTestSummary>,
    Option<EvidenceFinding>,
) {
    (
        status,
        coverage,
        summary,
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

fn read_python_test_result(
    path: &Path,
    manifest: &PythonTestManifest,
) -> Result<PythonTestSummary, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_PYTHON_RESULT_BYTES
    {
        return Err("Python Test result is not a bounded regular file".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let result: PythonTestResult =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if result.schema_version != 1
        || result.tests.len() != manifest.tests.len()
        || result.tests.len() > MAX_PYTHON_TESTS
    {
        return Err("Python Test result schema or cardinality mismatch".to_owned());
    }
    let mut summary = PythonTestSummary {
        declared: u64::try_from(result.tests.len())
            .map_err(|_| "Python Test result count overflow".to_owned())?,
        ..PythonTestSummary::default()
    };
    for (declared, observed) in manifest.tests.iter().zip(&result.tests) {
        if declared.module != observed.module || declared.function != observed.function {
            return Err("Python Test result order or identity mismatch".to_owned());
        }
        let counter = match observed.status {
            PythonDeclaredStatus::Passed => &mut summary.passed,
            PythonDeclaredStatus::AssertionFailed => &mut summary.assertion_failed,
            PythonDeclaredStatus::Error => &mut summary.error,
        };
        *counter = counter
            .checked_add(1)
            .ok_or_else(|| "Python Test result counter overflow".to_owned())?;
    }
    let observed = summary
        .passed
        .checked_add(summary.assertion_failed)
        .and_then(|value| value.checked_add(summary.error))
        .ok_or_else(|| "Python Test aggregate count overflow".to_owned())?;
    if observed != summary.declared {
        return Err("Python Test aggregate count is inconsistent".to_owned());
    }
    Ok(summary)
}

fn classify_python_result(
    process: &provider::ProcessResult,
    parsed: &Result<PythonTestSummary, String>,
) -> (
    TestStatus,
    TestCoverage,
    Option<PythonTestSummary>,
    Vec<EvidenceFinding>,
) {
    if process.timed_out {
        return python_classified(
            TestStatus::TimedOut,
            TestCoverage::Unknown,
            None,
            "python_test_timed_out",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Python test runner exceeded the fixed timeout; no project violation is inferred",
        );
    }
    if process.stdout.truncated || process.stderr.truncated {
        return python_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "python_test_output_truncated",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Python test output exceeded capture bounds; result is not authoritative",
        );
    }
    if !process.status.success() {
        return python_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "python_test_runner_failed",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "adapter-owned Python test runner exited unsuccessfully; no project violation is inferred",
        );
    }
    let Ok(summary) = parsed else {
        return python_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            None,
            "python_test_result_unavailable",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "adapter-owned Python test runner did not produce a valid bounded result",
        );
    };
    if summary.declared == 0 {
        return python_classified(
            TestStatus::NoTests,
            TestCoverage::Empty,
            Some(*summary),
            "python_test_no_tests",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Python test manifest declared no tests",
        );
    }
    let mut findings = Vec::new();
    if summary.assertion_failed > 0 {
        findings.push(EvidenceFinding {
            code: "python_test_assertion_failed".to_owned(),
            severity: FindingSeverity::Error,
            authority: FindingAuthority::DeterministicViolation,
            message: "one or more explicitly declared Python test functions raised AssertionError"
                .to_owned(),
            artifact_id: None,
            path: None,
        });
    }
    if summary.error > 0 {
        findings.push(EvidenceFinding {
            code: "python_test_unexpected_exception".to_owned(),
            severity: FindingSeverity::Error,
            authority: FindingAuthority::Advisory,
            message: "one or more explicitly declared Python test functions failed outside AssertionError; no project violation is inferred"
                .to_owned(),
            artifact_id: None,
            path: None,
        });
        return (
            TestStatus::Crashed,
            TestCoverage::Covered,
            Some(*summary),
            findings,
        );
    }
    if summary.assertion_failed > 0 {
        return (
            TestStatus::Failed,
            TestCoverage::Covered,
            Some(*summary),
            findings,
        );
    }
    (
        TestStatus::Passed,
        TestCoverage::Covered,
        Some(*summary),
        findings,
    )
}

fn python_classified(
    status: TestStatus,
    coverage: TestCoverage,
    summary: Option<PythonTestSummary>,
    code: &str,
    severity: FindingSeverity,
    authority: FindingAuthority,
    message: &str,
) -> (
    TestStatus,
    TestCoverage,
    Option<PythonTestSummary>,
    Vec<EvidenceFinding>,
) {
    (
        status,
        coverage,
        summary,
        vec![EvidenceFinding {
            code: code.to_owned(),
            severity,
            authority,
            message: message.to_owned(),
            artifact_id: None,
            path: None,
        }],
    )
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
    let observed_coverage = if counters.not_executed > 0 || counters.executed < counters.total {
        TestCoverage::Partial
    } else {
        TestCoverage::Covered
    };
    if counters.failed > 0 || counters.error > 0 || counters.timeout > 0 || counters.aborted > 0 {
        return classified(
            TestStatus::Failed,
            observed_coverage,
            Some(*counters),
            "dotnet_test_failed",
            FindingSeverity::Error,
            "one or more .NET tests failed; TRX v1 cannot safely distinguish assertion failure from test or environment exceptions",
        );
    }
    if !process.status.success() {
        return classified(
            TestStatus::Crashed,
            observed_coverage,
            Some(*counters),
            "dotnet_test_process_failed",
            FindingSeverity::Error,
            "dotnet vstest returned non-zero without a failed TRX test; no project violation is inferred",
        );
    }
    (TestStatus::Passed, observed_coverage, Some(*counters), None)
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

fn dotnet_test_environment(root: &Path) -> Result<Vec<(&'static str, PathBuf)>, AppError> {
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

fn rust_test_environment(root: &Path) -> Result<Vec<(&'static str, PathBuf)>, AppError> {
    let mut variables = vec![
        ("HOME", root.join("home")),
        ("USERPROFILE", root.join("home")),
        ("TEMP", root.join("temp")),
        ("TMP", root.join("temp")),
        ("TMPDIR", root.join("temp")),
        ("CARGO_INCREMENTAL", PathBuf::from("0")),
        ("CARGO_NET_OFFLINE", PathBuf::from("true")),
    ];
    let machine_home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| machine_home.as_ref().map(|home| home.join(".cargo")))
    {
        variables.push(("CARGO_HOME", cargo_home));
    }
    if let Some(rustup_home) = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| machine_home.map(|home| home.join(".rustup")))
    {
        variables.push(("RUSTUP_HOME", rustup_home));
    }
    for (name, path) in &variables {
        if !matches!(
            *name,
            "CARGO_INCREMENTAL" | "CARGO_NET_OFFLINE" | "CARGO_HOME" | "RUSTUP_HOME"
        ) {
            fs::create_dir_all(path)?;
        }
    }
    Ok(variables.into_iter().collect())
}

fn python_test_environment(root: &Path) -> Result<Vec<(&'static str, PathBuf)>, AppError> {
    let variables = vec![
        ("HOME", root.join("home")),
        ("USERPROFILE", root.join("home")),
        ("APPDATA", root.join("appdata")),
        ("LOCALAPPDATA", root.join("localappdata")),
        ("TEMP", root.join("temp")),
        ("TMP", root.join("temp")),
        ("TMPDIR", root.join("temp")),
    ];
    for (_, path) in &variables {
        fs::create_dir_all(path)?;
    }
    Ok(variables)
}

fn resolve_python_source_root(root: &Path, input: &Path) -> Result<PathBuf, AppError> {
    if input.is_absolute()
        || input.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::Provider(
            "Python Test source_root 必须是项目内规范相对路径".to_owned(),
        ));
    }
    let source_root = root.join(input).canonicalize()?;
    if !source_root.starts_with(root) || !source_root.is_dir() {
        return Err(AppError::Provider(
            "Python Test source_root 必须是项目内目录".to_owned(),
        ));
    }
    Ok(source_root)
}

fn resolve_python_manifest(
    root: &Path,
    source_root: &Path,
    input: &Path,
) -> Result<PathBuf, AppError> {
    if input.is_absolute()
        || input
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::Provider(
            "Python Test manifest 必须是项目内规范相对路径".to_owned(),
        ));
    }
    let manifest = root.join(input).canonicalize()?;
    let metadata = fs::symlink_metadata(&manifest)?;
    if !manifest.starts_with(source_root)
        || metadata.file_type().is_symlink()
        || !metadata.is_file()
        || manifest
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("json"))
    {
        return Err(AppError::Provider(
            "Python Test manifest 必须是 source_root 内普通 JSON 文件".to_owned(),
        ));
    }
    Ok(manifest)
}

fn read_python_test_manifest(
    manifest_path: &Path,
    source_root: &Path,
    repository_files: &BTreeSet<String>,
    project_root: &Path,
) -> Result<(PythonTestManifest, Vec<u8>), AppError> {
    let metadata = fs::symlink_metadata(manifest_path)?;
    if metadata.len() > MAX_PYTHON_MANIFEST_BYTES {
        return Err(AppError::Provider(
            "Python Test manifest 超过字节上限".to_owned(),
        ));
    }
    let bytes = fs::read(manifest_path)?;
    let manifest: PythonTestManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 || manifest.tests.len() > MAX_PYTHON_TESTS {
        return Err(AppError::Provider(
            "Python Test manifest schema_version 或测试数量非法".to_owned(),
        ));
    }
    let mut unique = BTreeSet::new();
    for case in &manifest.tests {
        validate_python_test_case(case)?;
        if !unique.insert(case.clone()) {
            return Err(AppError::Provider(
                "Python Test manifest 包含重复 module/function".to_owned(),
            ));
        }
        validate_python_module_source(case, source_root, repository_files, project_root)?;
    }
    Ok((manifest, bytes))
}

fn validate_python_test_case(case: &PythonTestCase) -> Result<(), AppError> {
    if case.module.is_empty()
        || case.module.len() > 256
        || case
            .module
            .split('.')
            .any(|segment| !is_python_identifier(segment))
        || !is_python_identifier(&case.function)
        || case.function.len() > 128
    {
        return Err(AppError::Provider(
            "Python Test module/function 必须是有界 ASCII 标识符".to_owned(),
        ));
    }
    Ok(())
}

fn is_python_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_python_module_source(
    case: &PythonTestCase,
    source_root: &Path,
    repository_files: &BTreeSet<String>,
    project_root: &Path,
) -> Result<(), AppError> {
    let module_path = case.module.replace('.', "/");
    let candidates = [
        source_root.join(format!("{module_path}.py")),
        source_root.join(&module_path).join("__init__.py"),
    ];
    let mut matched = 0_u8;
    for candidate in candidates {
        if !candidate.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&candidate)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AppError::Provider(
                "Python Test module source 不是普通文件".to_owned(),
            ));
        }
        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(source_root) {
            return Err(AppError::Provider(
                "Python Test module source 越出 source_root".to_owned(),
            ));
        }
        let display = project_relative_path(project_root, &canonical)?;
        if !repository_files.contains(&display) {
            return Err(AppError::Provider(
                "Python Test module source 不属于 Git Source 文件集合".to_owned(),
            ));
        }
        matched = matched.saturating_add(1);
    }
    if matched != 1 {
        return Err(AppError::Provider(
            "Python Test module 必须唯一对应 module.py 或 module/__init__.py".to_owned(),
        ));
    }
    Ok(())
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

fn resolve_rust_manifest(root: &Path, input: &Path) -> Result<PathBuf, AppError> {
    if input.is_absolute()
        || input
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::Provider(
            "Rust Test manifest 必须是项目内规范相对路径".to_owned(),
        ));
    }
    let manifest = root.join(input).canonicalize()?;
    let metadata = fs::symlink_metadata(&manifest)?;
    if !manifest.starts_with(root)
        || metadata.file_type().is_symlink()
        || !metadata.is_file()
        || manifest.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml")
    {
        return Err(AppError::Provider(
            "Rust Test manifest 必须是项目内普通 Cargo.toml".to_owned(),
        ));
    }
    Ok(manifest)
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

fn project_relative_path_or_dot(root: &Path, path: &Path) -> Result<String, AppError> {
    let relative = project_relative_path(root, path)?;
    Ok(if relative.is_empty() {
        ".".to_owned()
    } else {
        relative
    })
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

    fn successful_process() -> provider::ProcessResult {
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        };
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0)
        };
        let empty = || provider::CapturedOutput {
            bytes: Vec::new(),
            total_bytes: 0,
            sha256: brain_evidence::content_fingerprint(&[]),
            truncated: false,
        };
        provider::ProcessResult {
            status,
            timed_out: false,
            duration: Duration::ZERO,
            stdout: empty(),
            stderr: empty(),
        }
    }

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
    fn rust_summary_parser_aggregates_multiple_harnesses() {
        let summary = parse_rust_test_summary(
            b"running 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
            b"running 1 test\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
        )
        .unwrap();
        assert_eq!(summary.result_sections, 2);
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 1);
    }

    #[test]
    fn rust_result_classification_keeps_text_failures_advisory() {
        let process = successful_process();
        let failed = classify_rust_result(
            &process,
            false,
            &Ok(RustTestSummary {
                result_sections: 1,
                failed: 1,
                ..RustTestSummary::default()
            }),
        );
        assert_eq!(failed.0, TestStatus::Failed);
        assert_eq!(failed.1, TestCoverage::Covered);
        assert_eq!(failed.3.unwrap().authority, FindingAuthority::Advisory);

        let empty = classify_rust_result(
            &process,
            false,
            &Ok(RustTestSummary {
                result_sections: 1,
                ..RustTestSummary::default()
            }),
        );
        assert_eq!(empty.0, TestStatus::NoTests);
        assert_eq!(empty.1, TestCoverage::Empty);
    }

    #[test]
    fn skipped_tests_are_partial_coverage() {
        let process = successful_process();
        let rust = classify_rust_result(
            &process,
            false,
            &Ok(RustTestSummary {
                result_sections: 1,
                passed: 1,
                ignored: 1,
                ..RustTestSummary::default()
            }),
        );
        assert_eq!(rust.0, TestStatus::Passed);
        assert_eq!(rust.1, TestCoverage::Partial);
        assert_eq!(
            evidence_coverage_for_test(rust.0, rust.1),
            EvidenceCoverage::Partial
        );

        let dotnet = classify_result(
            &process,
            false,
            &Ok((
                1,
                TrxCounters {
                    total: 2,
                    executed: 1,
                    passed: 1,
                    not_executed: 1,
                    ..TrxCounters::default()
                },
            )),
        );
        assert_eq!(dotnet.0, TestStatus::Passed);
        assert_eq!(dotnet.1, TestCoverage::Partial);
    }

    #[test]
    fn python_manifest_and_result_contract_are_exact() {
        assert!(
            validate_python_test_case(&PythonTestCase {
                module: "tests.inventory".to_owned(),
                function: "test_save".to_owned(),
            })
            .is_ok()
        );
        assert!(
            validate_python_test_case(&PythonTestCase {
                module: "../tests".to_owned(),
                function: "test_save".to_owned(),
            })
            .is_err()
        );
        let unknown: Result<PythonTestManifest, _> =
            serde_json::from_slice(br#"{"schema_version":1,"tests":[],"command":"pytest"}"#);
        assert!(unknown.is_err());

        let manifest = PythonTestManifest {
            schema_version: 1,
            tests: vec![PythonTestCase {
                module: "sample_tests".to_owned(),
                function: "test_addition".to_owned(),
            }],
        };
        let directory = std::env::temp_dir().join(format!(
            "project-brain-python-result-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let result = directory.join("result.json");
        fs::write(
            &result,
            br#"{"schema_version":1,"tests":[{"module":"sample_tests","function":"test_addition","status":"passed"}]}"#,
        )
        .unwrap();
        let summary = read_python_test_result(&result, &manifest).unwrap();
        assert_eq!(summary.declared, 1);
        assert_eq!(summary.passed, 1);
        fs::write(
            &result,
            br#"{"schema_version":1,"tests":[{"module":"other","function":"test_addition","status":"passed"}]}"#,
        )
        .unwrap();
        assert!(read_python_test_result(&result, &manifest).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn python_assertion_is_structured_but_exception_is_advisory() {
        let process = successful_process();
        let assertion = classify_python_result(
            &process,
            &Ok(PythonTestSummary {
                declared: 1,
                assertion_failed: 1,
                ..PythonTestSummary::default()
            }),
        );
        assert_eq!(assertion.0, TestStatus::Failed);
        assert_eq!(assertion.1, TestCoverage::Covered);
        assert_eq!(
            assertion.3[0].authority,
            FindingAuthority::DeterministicViolation
        );

        let error = classify_python_result(
            &process,
            &Ok(PythonTestSummary {
                declared: 1,
                error: 1,
                ..PythonTestSummary::default()
            }),
        );
        assert_eq!(error.0, TestStatus::Crashed);
        assert_eq!(error.3[0].authority, FindingAuthority::Advisory);

        let empty = classify_python_result(&process, &Ok(PythonTestSummary::default()));
        assert_eq!(empty.0, TestStatus::NoTests);
        assert_eq!(empty.1, TestCoverage::Empty);
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
