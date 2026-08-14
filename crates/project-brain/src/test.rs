use std::{
    collections::BTreeSet,
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
use serde::{Deserialize, Serialize};

use crate::{artifact_store, error::AppError, git, provider, runtime};

const TEST_RUN_SCHEMA_VERSION: u32 = 1;
const DOTNET_TEST_CONTRACT_VERSION: u16 = 1;
const RUST_TEST_CONTRACT_VERSION: u16 = 1;
const GODOT_SCENARIO_TEST_CONTRACT_VERSION: u16 = 1;
const MAX_TRX_FILES: usize = 32;
const MAX_TRX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GODOT_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_GODOT_ASSERTIONS: usize = 1_000;
const MAX_GODOT_MESSAGE_BYTES: usize = 4_096;
const MAX_GODOT_LOG_BYTES: u64 = 64 * 1024 * 1024;
const GODOT_RESULT_FILE: &str = ".project-brain-test-result-v1.json";

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

pub(crate) struct GodotScenarioTestRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) install_root: Option<&'a Path>,
    pub(crate) project_key: &'a str,
    pub(crate) profile_id: &'a str,
    pub(crate) build_profile_id: &'a str,
    pub(crate) executable: &'a Path,
    pub(crate) target: &'a Path,
    pub(crate) scenario: &'a Path,
    pub(crate) trust_local_executable: bool,
    pub(crate) trust_repository_test_code: bool,
    pub(crate) quit_after: u32,
    pub(crate) timeout_seconds: u64,
    pub(crate) evidence_heads: &'a [EvidenceHeadRecord],
}

#[derive(Debug, Serialize)]
struct GodotScenarioTestContract {
    contract_version: u16,
    adapter: &'static str,
    profile_id: String,
    build_provider_id: String,
    build_snapshot_fingerprint: String,
    build_bundle_fingerprint: String,
    target: String,
    scenario: String,
    result_file: &'static str,
    import_argv: Vec<String>,
    scenario_argv: Vec<String>,
    environment_policy: &'static str,
    network_policy: &'static str,
    execution_class: &'static str,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeclaredScenarioStatus {
    Passed,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GodotScenarioResult {
    schema_version: u32,
    scenario_id: String,
    status: DeclaredScenarioStatus,
    assertions: Vec<GodotScenarioAssertion>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GodotScenarioAssertion {
    id: String,
    passed: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct GodotScenarioSummary {
    assertions: usize,
    passed: usize,
    failed: usize,
    result_fingerprint: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GodotScenarioTestReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    status: TestStatus,
    coverage: TestCoverage,
    engine_version: String,
    executable_sha256: String,
    contract: GodotScenarioTestContract,
    import: TestProcessSummary,
    scenario: Option<TestProcessSummary>,
    result: GodotScenarioSummary,
    pub(crate) evidence: EvidenceSnapshot,
}

impl GodotScenarioTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
    }
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
}

impl DotnetTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
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
}

impl RustTestReport {
    pub(crate) fn passed(&self) -> bool {
        self.status == TestStatus::Passed
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

struct GodotTestScratch {
    directory: PathBuf,
    project: PathBuf,
    import_log: PathBuf,
    scenario_log: PathBuf,
}

impl GodotTestScratch {
    fn create(project_key: &str) -> Result<Self, AppError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::Provider(format!("系统时间无效：{error}")))?
            .as_nanos();
        let key_fingerprint = brain_evidence::content_fingerprint(project_key.as_bytes());
        let directory = std::env::temp_dir().join(format!(
            "project-brain-godot-test-{}-{}-{nonce}",
            &key_fingerprint[..16],
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self {
            project: directory.join("project"),
            import_log: directory.join("import.log"),
            scenario_log: directory.join("scenario.log"),
            directory,
        })
    }

    fn environment(&self) -> Result<Vec<(&'static str, PathBuf)>, AppError> {
        let variables = [
            ("HOME", self.directory.join("home")),
            ("USERPROFILE", self.directory.join("home")),
            ("APPDATA", self.directory.join("appdata/roaming")),
            ("LOCALAPPDATA", self.directory.join("appdata/local")),
            ("XDG_DATA_HOME", self.directory.join("xdg/data")),
            ("XDG_CONFIG_HOME", self.directory.join("xdg/config")),
            ("XDG_CACHE_HOME", self.directory.join("xdg/cache")),
            ("TEMP", self.directory.join("temp")),
            ("TMP", self.directory.join("temp")),
        ];
        for (_, path) in &variables {
            fs::create_dir_all(path)?;
        }
        Ok(variables.into_iter().collect())
    }
}

impl Drop for GodotTestScratch {
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
        finding.into_iter().collect(),
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
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
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "Godot Scenario Test 线性保留 Source、Engine、Build CAS、staging、结构化结果和 TOCTOU 校验"
)]
pub(crate) fn run_godot_scenario(
    request: &GodotScenarioTestRequest<'_>,
) -> Result<GodotScenarioTestReport, AppError> {
    if !request.trust_local_executable || !request.trust_repository_test_code {
        return Err(AppError::Provider(
            "Godot Scenario Test 会执行仓库场景代码，必须显式提供机器 executable 与 repository test code 两个信任位"
                .to_owned(),
        ));
    }
    validate_profile_id(request.profile_id)?;
    validate_profile_id(request.build_profile_id)?;
    let root = request.project_root.canonicalize()?;
    if !root.join("project.godot").is_file() {
        return Err(AppError::Provider(
            "Godot Scenario Test 项目缺少 project.godot".to_owned(),
        ));
    }
    if root.join("override.cfg").exists() || root.join(GODOT_RESULT_FILE).exists() {
        return Err(AppError::Provider(
            "项目源码与 Godot Scenario Test 受控 override/result 路径冲突".to_owned(),
        ));
    }
    let target = resolve_project_file(&root, request.target)?;
    let target_display = project_relative_path(&root, &target)?;
    let scenario = resolve_godot_scenario(&root, request.scenario)?;
    let scenario_display = project_relative_path(&root, &scenario)?;
    let repository_files = git::repository_files(&root)?;
    if !repository_files
        .iter()
        .any(|path| path == &scenario_display)
    {
        return Err(AppError::Provider(
            "Godot Scenario Test 场景必须属于 Git Source manifest".to_owned(),
        ));
    }

    let source_fingerprint = git::worktree_fingerprint(&root)?;
    let build_provider_id = format!("dotnet-build.{}", request.build_profile_id);
    let build_head = request
        .evidence_heads
        .iter()
        .find(|head| head.plane == EvidencePlane::Build && head.provider_id == build_provider_id)
        .ok_or_else(|| AppError::Provider("缺少指定 Godot .NET Build Evidence head".to_owned()))?;
    if build_head.freshness != EvidenceFreshness::Fresh
        || build_head.snapshot.coverage != EvidenceCoverage::Complete
        || build_head.snapshot.provider.authority != EvidenceAuthority::Deterministic
        || !build_head.snapshot.findings.is_empty()
        || build_head.snapshot.source_fingerprint != source_fingerprint
    {
        return Err(AppError::Provider(
            "Godot Scenario Test 只接受同源码、fresh、complete、deterministic 且无 finding 的 Build head"
                .to_owned(),
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
        || bundle.assembly_binding().is_none()
    {
        return Err(AppError::Provider(
            "Godot Scenario Test bundle 与项目、provider、源码、target 或主程序集绑定不一致"
                .to_owned(),
        ));
    }

    let executable =
        provider::pin_external_executable(&root, request.executable, "Godot executable")?;
    let upstream =
        qualify_godot_test_upstream(request.evidence_heads, build_head, &executable.sha256)?;
    let scratch = GodotTestScratch::create(request.project_key)?;
    let staged_source = runtime::stage_project(&root, &scratch.project)?;
    fs::write(
        scratch.project.join("override.cfg"),
        "[application]\nconfig/use_custom_user_dir=true\nconfig/custom_user_dir=\"project-brain-godot-scenario-test\"\n",
    )?;
    let bundle_directory = scratch.project.join(".godot/mono/temp/bin/Debug");
    artifact_store::materialize_runtime_bundle(
        request.install_root,
        &bundle_fingerprint,
        &bundle_directory,
    )?;
    artifact_store::verify_materialized_bundle(&bundle, &bundle_directory)?;
    runtime::verify_staged_source(&staged_source, &scratch.project, &["override.cfg"])?;
    if git::worktree_fingerprint(&root)? != source_fingerprint {
        return Err(AppError::Provider(
            "权威源码在 Godot Scenario Test staging 期间发生变化".to_owned(),
        ));
    }

    let environment_storage = scratch.environment()?;
    let environment = environment_storage
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect::<Vec<_>>();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let engine_version = runtime::qualify_engine(&root, &executable, timeout, &environment)?;
    let staged_root = provider::provider_cli_path(&scratch.project);
    let import_argv = vec![
        "--headless".to_owned(),
        "--no-header".to_owned(),
        "--path".to_owned(),
        staged_root.clone(),
        "--import".to_owned(),
        "--log-file".to_owned(),
        provider::provider_cli_path(&scratch.import_log),
    ];
    let scenario_resource = format!("res://{scenario_display}");
    let scenario_argv = vec![
        "--headless".to_owned(),
        "--no-header".to_owned(),
        "--path".to_owned(),
        staged_root,
        "--quit-after".to_owned(),
        request.quit_after.to_string(),
        "--log-file".to_owned(),
        provider::provider_cli_path(&scratch.scenario_log),
        scenario_resource.clone(),
    ];
    runtime::validate_fixed_argv(&import_argv)?;
    runtime::validate_fixed_argv(&scenario_argv)?;

    let source_before = git::worktree_fingerprint(&root)?;
    let import = provider::run_process_with_environment_observing_timeout(
        &executable.canonical_path,
        None,
        &import_argv,
        &scratch.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    artifact_store::verify_materialized_bundle(&bundle, &bundle_directory)?;
    runtime::verify_staged_source(&staged_source, &scratch.project, &["override.cfg"])?;
    let import_diagnostics = read_godot_diagnostics(&scratch.import_log);
    let import_ready = !import.timed_out
        && import.status.success()
        && !import.stdout.truncated
        && !import.stderr.truncated
        && import_diagnostics.as_ref().is_ok_and(Vec::is_empty);
    if import_ready && scratch.project.join(GODOT_RESULT_FILE).exists() {
        return Err(AppError::Provider(
            "Godot import 在场景执行前写入了保留结果路径；拒绝接受该测试".to_owned(),
        ));
    }
    let scenario_process = if import_ready {
        Some(provider::run_process_with_environment_observing_timeout(
            &executable.canonical_path,
            None,
            &scenario_argv,
            &scratch.directory,
            Some(&root),
            timeout,
            &environment,
        )?)
    } else {
        None
    };
    artifact_store::verify_materialized_bundle(&bundle, &bundle_directory)?;
    runtime::verify_staged_source(
        &staged_source,
        &scratch.project,
        &["override.cfg", GODOT_RESULT_FILE],
    )?;
    let source_after = git::worktree_fingerprint(&root)?;
    if source_before != source_after || source_after != source_fingerprint {
        return Err(AppError::Provider(
            "权威源码在 Godot Scenario Test 运行期间发生变化；结果已丢弃".to_owned(),
        ));
    }
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Godot executable 在 Scenario Test 期间发生漂移".to_owned(),
        ));
    }

    let scenario_diagnostics = scenario_process.as_ref().map_or_else(
        || Ok(Vec::new()),
        |_| read_godot_diagnostics(&scratch.scenario_log),
    );
    let result = if scenario_process.is_some() {
        read_godot_scenario_result(&scratch.project.join(GODOT_RESULT_FILE), request.profile_id)
    } else {
        Err(
            "scenario was not executed because import did not satisfy the fixed contract"
                .to_owned(),
        )
    };
    let (status, coverage, findings) = classify_godot_scenario(
        &import,
        scenario_process.as_ref(),
        &import_diagnostics,
        &scenario_diagnostics,
        &result,
    );
    let summary = result.as_ref().map_or(
        GodotScenarioSummary {
            assertions: 0,
            passed: 0,
            failed: 0,
            result_fingerprint: None,
        },
        |(parsed, bytes)| {
            let passed = parsed.assertions.iter().filter(|item| item.passed).count();
            GodotScenarioSummary {
                assertions: parsed.assertions.len(),
                passed,
                failed: parsed.assertions.len() - passed,
                result_fingerprint: Some(brain_evidence::content_fingerprint(bytes)),
            }
        },
    );
    let provider_id = format!("godot-scenario-test.{}", request.profile_id);
    let contract = GodotScenarioTestContract {
        contract_version: GODOT_SCENARIO_TEST_CONTRACT_VERSION,
        adapter: "godot-scenario-result-v1",
        profile_id: request.profile_id.to_owned(),
        build_provider_id,
        build_snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
        build_bundle_fingerprint: bundle_fingerprint,
        target: target_display,
        scenario: scenario_display,
        result_file: GODOT_RESULT_FILE,
        import_argv: redact_godot_argv(&import_argv, None),
        scenario_argv: redact_godot_argv(&scenario_argv, Some(&scenario_resource)),
        environment_policy: "env_clear+adapter_allowlist+machine_scratch+physical_source_stage",
        network_policy: "adapter_no_restore_build_export_or_network;repository_test_code_not_os_sandboxed",
        execution_class: "repository_scenario_test_code",
    };
    let contract_bytes = serde_json::to_vec(&contract)?;
    let summary_bytes = serde_json::to_vec(&(status, coverage, &summary))?;
    let mut artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_contract",
            "contract",
            "Godot Scenario Test fixed execution contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "test_result_summary",
            "result-summary",
            "Godot Scenario Test classified result",
            None,
            &summary_bytes,
        ),
    ];
    if let Ok((_, bytes)) = &result {
        artifacts.push(ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "godot_scenario_result",
            "structured-result",
            "Bounded Godot Scenario assertion result",
            Some(GODOT_RESULT_FILE),
            bytes,
        ));
    }
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Test,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{engine_version}+sha256.{}", executable.sha256),
            contract_version: GODOT_SCENARIO_TEST_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        &source_fingerprint,
        if matches!(status, TestStatus::ProviderFailed | TestStatus::TimedOut) {
            EvidenceCoverage::Partial
        } else {
            EvidenceCoverage::Complete
        },
        upstream,
        artifacts,
        Vec::new(),
        findings,
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
    Ok(GodotScenarioTestReport {
        schema_version: TEST_RUN_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        profile_id: request.profile_id.to_owned(),
        provider_id,
        status,
        coverage,
        engine_version,
        executable_sha256: executable.sha256,
        contract,
        import: summarize_process(&import),
        scenario: scenario_process.as_ref().map(summarize_process),
        result: summary,
        evidence,
    })
}

fn qualify_godot_test_upstream(
    evidence_heads: &[EvidenceHeadRecord],
    build_head: &EvidenceHeadRecord,
    executable_sha256: &str,
) -> Result<Vec<EvidenceReference>, AppError> {
    let mut engine_references = 0_usize;
    let mut upstream = Vec::with_capacity(build_head.snapshot.upstream.len() + 1);
    upstream.push(EvidenceReference {
        plane: EvidencePlane::Build,
        provider_id: build_head.provider_id.clone(),
        snapshot_fingerprint: build_head.snapshot_fingerprint.clone(),
    });
    for reference in &build_head.snapshot.upstream {
        let head = evidence_heads
            .iter()
            .find(|head| {
                head.plane == reference.plane
                    && head.provider_id == reference.provider_id
                    && head.snapshot_fingerprint == reference.snapshot_fingerprint
                    && head.freshness == EvidenceFreshness::Fresh
            })
            .ok_or_else(|| {
                AppError::Provider(
                    "Godot Scenario Test 的 Build upstream 不再是当前 fresh Evidence head"
                        .to_owned(),
                )
            })?;
        if reference.plane == EvidencePlane::Engine {
            engine_references += 1;
            if head.snapshot.coverage != EvidenceCoverage::Complete
                || head.snapshot.provider.authority != EvidenceAuthority::Deterministic
                || !head.snapshot.findings.is_empty()
                || !head.snapshot.provider.version.contains(executable_sha256)
            {
                return Err(AppError::Provider(
                    "Godot Scenario Test Engine Evidence 不完整、含 finding 或 executable 哈希不一致"
                        .to_owned(),
                ));
            }
        }
        upstream.push(reference.clone());
    }
    if engine_references != 1 {
        return Err(AppError::Provider(
            "Godot Scenario Test 要求 Build Evidence 精确引用一个 Engine head".to_owned(),
        ));
    }
    upstream.sort();
    Ok(upstream)
}

fn resolve_godot_scenario(root: &Path, input: &Path) -> Result<PathBuf, AppError> {
    if input.is_absolute()
        || input
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::Provider(
            "Godot scenario 必须是项目内规范相对路径".to_owned(),
        ));
    }
    let scenario = root.join(input).canonicalize()?;
    let metadata = fs::symlink_metadata(&scenario)?;
    if !scenario.starts_with(root)
        || metadata.file_type().is_symlink()
        || !metadata.is_file()
        || scenario
            .extension()
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("tscn"))
    {
        return Err(AppError::Provider(
            "Godot scenario 必须是项目内普通 .tscn 文件".to_owned(),
        ));
    }
    Ok(scenario)
}

fn read_godot_scenario_result(
    path: &Path,
    expected_scenario_id: &str,
) -> Result<(GodotScenarioResult, Vec<u8>), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_GODOT_RESULT_BYTES
    {
        return Err("Godot scenario result is not a bounded regular file".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    parse_godot_scenario_result(&bytes, expected_scenario_id)
}

fn parse_godot_scenario_result(
    bytes: &[u8],
    expected_scenario_id: &str,
) -> Result<(GodotScenarioResult, Vec<u8>), String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Godot scenario result is not UTF-8".to_owned())?;
    let result: GodotScenarioResult = serde_json::from_str(text)
        .map_err(|error| format!("Godot scenario result JSON is invalid: {error}"))?;
    if result.schema_version != 1 || result.scenario_id != expected_scenario_id {
        return Err("Godot scenario result schema or scenario_id does not match".to_owned());
    }
    if result.assertions.len() > MAX_GODOT_ASSERTIONS {
        return Err("Godot scenario result exceeds the assertion bound".to_owned());
    }
    let mut ids = BTreeSet::new();
    for assertion in &result.assertions {
        if !valid_assertion_id(&assertion.id)
            || !ids.insert(assertion.id.as_str())
            || assertion.message.len() > MAX_GODOT_MESSAGE_BYTES
        {
            return Err("Godot scenario assertion id/message contract is invalid".to_owned());
        }
    }
    let failed = result.assertions.iter().any(|assertion| !assertion.passed);
    if (result.status == DeclaredScenarioStatus::Passed && failed)
        || (result.status == DeclaredScenarioStatus::Failed && !failed)
    {
        return Err("Godot scenario declared status contradicts its assertions".to_owned());
    }
    let canonical = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    Ok((result, canonical))
}

fn valid_assertion_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}

fn read_godot_diagnostics(path: &Path) -> Result<Vec<String>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_GODOT_LOG_BYTES
    {
        return Err("Godot log is not a bounded regular file".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let mut diagnostics = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("ERROR:") || line.starts_with("SCRIPT ERROR"))
        .map(|line| brain_evidence::content_fingerprint(line.as_bytes()))
        .collect::<Vec<_>>();
    diagnostics.sort();
    diagnostics.dedup();
    if diagnostics.len() > 256 {
        return Err("Godot log contains too many distinct diagnostics".to_owned());
    }
    Ok(diagnostics)
}

fn classify_godot_scenario(
    import: &provider::ProcessResult,
    scenario: Option<&provider::ProcessResult>,
    import_diagnostics: &Result<Vec<String>, String>,
    scenario_diagnostics: &Result<Vec<String>, String>,
    result: &Result<(GodotScenarioResult, Vec<u8>), String>,
) -> (TestStatus, TestCoverage, Vec<EvidenceFinding>) {
    if let Some(classification) = classify_godot_import(import, import_diagnostics) {
        return classification;
    }
    classify_godot_execution(scenario, scenario_diagnostics, result)
}

fn classify_godot_import(
    import: &provider::ProcessResult,
    diagnostics: &Result<Vec<String>, String>,
) -> Option<(TestStatus, TestCoverage, Vec<EvidenceFinding>)> {
    if import.timed_out {
        return Some(godot_classified(
            TestStatus::TimedOut,
            TestCoverage::Unknown,
            "godot_scenario_import_timed_out",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot import exceeded the fixed timeout; no project violation is inferred",
        ));
    }
    if import.stdout.truncated || import.stderr.truncated {
        return Some(godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_import_output_truncated",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot import output exceeded capture bounds; scenario was not executed",
        ));
    }
    if !import.status.success() {
        return Some(godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_import_failed",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot import failed before the scenario contract could execute",
        ));
    }
    match diagnostics {
        Err(_) => {
            return Some(godot_classified(
                TestStatus::ProviderFailed,
                TestCoverage::Unknown,
                "godot_scenario_import_log_unavailable",
                FindingSeverity::Warning,
                FindingAuthority::Advisory,
                "Godot import log did not satisfy the bounded log contract",
            ));
        }
        Ok(items) if !items.is_empty() => {
            return Some(godot_classified(
                TestStatus::ProviderFailed,
                TestCoverage::Unknown,
                "godot_scenario_import_diagnostic",
                FindingSeverity::Error,
                FindingAuthority::Advisory,
                "Godot import emitted one or more engine diagnostics; scenario was not executed",
            ));
        }
        Ok(_) => {}
    }
    None
}

fn classify_godot_execution(
    scenario: Option<&provider::ProcessResult>,
    scenario_diagnostics: &Result<Vec<String>, String>,
    result: &Result<(GodotScenarioResult, Vec<u8>), String>,
) -> (TestStatus, TestCoverage, Vec<EvidenceFinding>) {
    let Some(scenario) = scenario else {
        return godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_not_executed",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario was not executed",
        );
    };
    if scenario.timed_out {
        return godot_classified(
            TestStatus::TimedOut,
            TestCoverage::Unknown,
            "godot_scenario_timed_out",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario exceeded the fixed timeout; no assertion violation is inferred",
        );
    }
    if scenario.stdout.truncated || scenario.stderr.truncated {
        return godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_output_truncated",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario output exceeded capture bounds; result is not authoritative",
        );
    }
    if scenario_diagnostics.is_err() {
        return godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_log_unavailable",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario log did not satisfy the bounded log contract",
        );
    }
    let Ok((parsed, canonical)) = result else {
        return godot_classified(
            TestStatus::ProviderFailed,
            TestCoverage::Unknown,
            "godot_scenario_result_unavailable",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario did not produce a valid bounded structured result",
        );
    };
    classify_godot_structured_result(scenario, scenario_diagnostics, parsed, canonical)
}

fn classify_godot_structured_result(
    scenario: &provider::ProcessResult,
    scenario_diagnostics: &Result<Vec<String>, String>,
    parsed: &GodotScenarioResult,
    canonical: &[u8],
) -> (TestStatus, TestCoverage, Vec<EvidenceFinding>) {
    if parsed.assertions.is_empty() {
        return godot_classified(
            TestStatus::NoTests,
            TestCoverage::Empty,
            "godot_scenario_no_assertions",
            FindingSeverity::Warning,
            FindingAuthority::Advisory,
            "Godot scenario completed but declared no assertions",
        );
    }
    if parsed.status == DeclaredScenarioStatus::Failed {
        let failed = parsed
            .assertions
            .iter()
            .filter(|assertion| !assertion.passed)
            .count();
        let message = format!(
            "{failed} structured scenario assertion(s) failed; result_fingerprint={}",
            brain_evidence::content_fingerprint(canonical)
        );
        return godot_classified(
            TestStatus::Failed,
            TestCoverage::Covered,
            "godot_scenario_assertion_failed",
            FindingSeverity::Error,
            FindingAuthority::DeterministicViolation,
            &message,
        );
    }
    if !scenario.status.success() {
        return godot_classified(
            TestStatus::Crashed,
            TestCoverage::Covered,
            "godot_scenario_process_failed",
            FindingSeverity::Error,
            FindingAuthority::Advisory,
            "Godot process returned non-zero after producing passing assertions; no assertion violation is inferred",
        );
    }
    if scenario_diagnostics
        .as_ref()
        .is_ok_and(|items| !items.is_empty())
    {
        return godot_classified(
            TestStatus::Failed,
            TestCoverage::Covered,
            "godot_scenario_runtime_diagnostic",
            FindingSeverity::Error,
            FindingAuthority::Advisory,
            "Godot scenario produced passing assertions but emitted engine diagnostics",
        );
    }
    (TestStatus::Passed, TestCoverage::Covered, Vec::new())
}

fn godot_classified(
    status: TestStatus,
    coverage: TestCoverage,
    code: &str,
    severity: FindingSeverity,
    authority: FindingAuthority,
    message: &str,
) -> (TestStatus, TestCoverage, Vec<EvidenceFinding>) {
    (
        status,
        coverage,
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

fn redact_godot_argv(argv: &[String], scenario: Option<&str>) -> Vec<String> {
    let mut previous = "";
    argv.iter()
        .map(|argument| {
            let redacted = if previous == "--path" {
                "<STAGED_PROJECT>".to_owned()
            } else if previous == "--log-file" {
                "<TEST_LOG>".to_owned()
            } else if scenario == Some(argument.as_str()) {
                "<SCENARIO>".to_owned()
            } else {
                argument.clone()
            };
            previous = argument;
            redacted
        })
        .collect()
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
    if summary.failed > 0 {
        return rust_classified(
            TestStatus::Failed,
            TestCoverage::Covered,
            Some(*summary),
            "rust_test_failed",
            FindingSeverity::Error,
            "one or more Rust tests failed; libtest text v1 cannot safely distinguish assertion failure from panic or harness error",
        );
    }
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
    if !process.status.success() {
        return rust_classified(
            TestStatus::Crashed,
            TestCoverage::Covered,
            Some(*summary),
            "rust_test_process_failed",
            FindingSeverity::Error,
            "cargo test returned non-zero without a failed libtest summary; no assertion violation is inferred",
        );
    }
    (
        TestStatus::Passed,
        TestCoverage::Covered,
        Some(*summary),
        None,
    )
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
    fn test_argv_and_paths_are_adapter_owned_and_bounded() {
        assert!(validate_profile_id("game-debug").is_ok());
        assert!(validate_profile_id("../escape").is_err());
        assert_eq!(
            validate_bundle_relative_path(Path::new("Game.Tests.dll")).unwrap(),
            "Game.Tests.dll"
        );
        assert!(validate_bundle_relative_path(Path::new("../Game.Tests.dll")).is_err());
    }

    #[test]
    fn godot_result_contract_rejects_ambiguous_or_fabricated_shape() {
        let passed = parse_godot_scenario_result(
            br#"{"schema_version":1,"scenario_id":"first-loop","status":"passed","assertions":[{"id":"inventory/closed-loop","passed":true,"message":"ok"}]}"#,
            "first-loop",
        )
        .unwrap();
        assert_eq!(passed.0.assertions.len(), 1);

        let contradiction = parse_godot_scenario_result(
            br#"{"schema_version":1,"scenario_id":"first-loop","status":"passed","assertions":[{"id":"loop","passed":false,"message":"failed"}]}"#,
            "first-loop",
        );
        assert!(contradiction.is_err());

        let duplicate = parse_godot_scenario_result(
            br#"{"schema_version":1,"scenario_id":"first-loop","status":"failed","assertions":[{"id":"loop","passed":false,"message":"a"},{"id":"loop","passed":true,"message":"b"}]}"#,
            "first-loop",
        );
        assert!(duplicate.is_err());

        let unknown_field = parse_godot_scenario_result(
            br#"{"schema_version":1,"scenario_id":"first-loop","status":"passed","assertions":[],"trusted":true}"#,
            "first-loop",
        );
        assert!(unknown_field.is_err());
    }

    #[test]
    fn only_valid_failed_godot_assertion_receives_violation_authority() {
        let process = successful_process();
        let failed = parse_godot_scenario_result(
            br#"{"schema_version":1,"scenario_id":"first-loop","status":"failed","assertions":[{"id":"production/tool","passed":false,"message":"tool missing"}]}"#,
            "first-loop",
        );
        let classified = classify_godot_scenario(
            &process,
            Some(&process),
            &Ok(Vec::new()),
            &Ok(Vec::new()),
            &failed,
        );
        assert_eq!(classified.0, TestStatus::Failed);
        assert_eq!(
            classified.2[0].authority,
            FindingAuthority::DeterministicViolation
        );

        let unavailable = classify_godot_scenario(
            &process,
            Some(&process),
            &Ok(Vec::new()),
            &Ok(Vec::new()),
            &Err("missing".to_owned()),
        );
        assert_eq!(unavailable.0, TestStatus::ProviderFailed);
        assert_eq!(unavailable.2[0].authority, FindingAuthority::Advisory);
    }

    #[test]
    fn godot_argv_redaction_preserves_contract_but_not_machine_paths() {
        let scenario = "res://tests/FirstLoop.tscn";
        let argv = vec![
            "--headless".to_owned(),
            "--path".to_owned(),
            "C:/private/stage".to_owned(),
            "--log-file".to_owned(),
            "C:/private/scenario.log".to_owned(),
            scenario.to_owned(),
        ];
        assert_eq!(
            redact_godot_argv(&argv, Some(scenario)),
            vec![
                "--headless",
                "--path",
                "<STAGED_PROJECT>",
                "--log-file",
                "<TEST_LOG>",
                "<SCENARIO>",
            ]
        );
    }
}
