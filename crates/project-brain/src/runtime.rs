use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::{
    ArtifactNode, EvidenceAuthority, EvidenceCoverage, EvidenceFinding, EvidenceFreshness,
    EvidencePlane, EvidenceProvider, EvidenceReference, EvidenceSnapshot, FindingSeverity,
};
use brain_store::EvidenceHeadRecord;
use serde::Serialize;

use crate::{artifact_store, error::AppError, git, provider, setup};

const RUNTIME_SCHEMA_VERSION: u32 = 1;
const RUNTIME_CONTRACT_VERSION: u16 = 1;
const MAX_RUNTIME_LOG_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) struct RuntimeRequest<'a> {
    pub(crate) project_root: &'a Path,
    pub(crate) install_root: Option<&'a Path>,
    pub(crate) project_key: &'a str,
    pub(crate) bundle_fingerprint: &'a str,
    pub(crate) executable: &'a Path,
    pub(crate) trust_local_executable: bool,
    pub(crate) quit_after: u32,
    pub(crate) timeout_seconds: u64,
    pub(crate) evidence_heads: &'a [EvidenceHeadRecord],
}

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeRunReport {
    schema_version: u32,
    project_key: String,
    provider_id: String,
    engine_version: String,
    executable_sha256: String,
    bundle_fingerprint: String,
    staged_source_manifest_fingerprint: String,
    assembly_binding: artifact_store::AssemblyBindingAttestation,
    contract: RuntimeContract,
    import: RuntimeProcessSummary,
    runtime: Option<RuntimeProcessSummary>,
    status: &'static str,
    evidence: EvidenceSnapshot,
}

impl RuntimeRunReport {
    pub(crate) fn evidence_snapshot(&self) -> &EvidenceSnapshot {
        &self.evidence
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.status == "succeeded"
    }
}

#[derive(Debug, Serialize)]
struct RuntimeContract {
    contract_version: u16,
    adapter: &'static str,
    import_argv: Vec<String>,
    runtime_argv: Vec<String>,
    source_policy: &'static str,
    artifact_policy: &'static str,
    user_data_policy: &'static str,
    forbidden_operations: [&'static str; 6],
}

#[derive(Debug, Serialize)]
struct RuntimeProcessSummary {
    duration_ms: u128,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_sha256: String,
    stderr_sha256: String,
    output_truncated: bool,
}

#[derive(Debug, Serialize)]
struct StagedSourceManifest {
    entries: Vec<StagedSourceEntry>,
}

#[derive(Debug, Serialize)]
struct StagedSourceEntry {
    relative_path: String,
    size: u64,
    sha256: String,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn run_godot(request: &RuntimeRequest<'_>) -> Result<RuntimeRunReport, AppError> {
    if !request.trust_local_executable {
        return Err(AppError::Provider(
            "Runtime Evidence 需要显式传入 --trust-local-executable".to_owned(),
        ));
    }
    let root = request.project_root.canonicalize()?;
    if !root.join("project.godot").is_file() {
        return Err(AppError::Provider(
            "Godot Runtime 项目缺少 project.godot".to_owned(),
        ));
    }
    if root.join("override.cfg").exists() {
        return Err(AppError::Provider(
            "权威源码已包含 override.cfg；Runtime v1 拒绝合并用户目录覆盖".to_owned(),
        ));
    }
    let executable =
        provider::pin_external_executable(&root, request.executable, "Godot executable")?;
    let bundle =
        artifact_store::verify_runtime_bundle(request.install_root, request.bundle_fingerprint)?;
    if bundle.project_key() != request.project_key {
        return Err(AppError::Provider(
            "Runtime bundle 属于另一个 project_key".to_owned(),
        ));
    }
    let assembly_binding = bundle
        .assembly_binding()
        .cloned()
        .ok_or_else(|| AppError::Provider("Runtime bundle 缺少 Godot 主程序集绑定".to_owned()))?;
    let (build_reference, mut upstream) =
        qualify_evidence_heads(request, &bundle, &executable.sha256)?;
    upstream.push(build_reference);
    upstream.sort();

    let source_before = git::worktree_fingerprint(&root)?;
    if source_before != bundle.source_fingerprint() {
        return Err(AppError::Provider(
            "当前 Source fingerprint 与 Runtime bundle 的 Build Source 不一致".to_owned(),
        ));
    }
    let run_directory = RuntimeDirectory::create(request.install_root, request.project_key)?;
    let staged_source = stage_project(&root, &run_directory.project)?;
    run_directory.write_journal("source_staged")?;
    fs::write(
        run_directory.project.join("override.cfg"),
        format!(
            "[application]\nconfig/use_custom_user_dir=true\nconfig/custom_user_dir=\"project-brain-runtime-{}\"\n",
            run_directory.run_id
        ),
    )?;
    let source_after = git::worktree_fingerprint(&root)?;
    if source_before != source_after {
        return Err(AppError::Provider(
            "权威源码在 Runtime staging 期间发生变化；拒绝运行".to_owned(),
        ));
    }
    let staged_source_bytes = serde_json::to_vec(&staged_source)?;
    let staged_source_manifest_fingerprint =
        brain_evidence::content_fingerprint(&staged_source_bytes);
    let bundle_directory = run_directory.project.join(".godot/mono/temp/bin/Debug");
    let materialized = artifact_store::materialize_runtime_bundle(
        request.install_root,
        request.bundle_fingerprint,
        &bundle_directory,
    )?;
    artifact_store::verify_materialized_bundle(&materialized, &bundle_directory)?;
    run_directory.write_journal("bundle_materialized")?;

    let environment = run_directory.environment();
    let timeout = Duration::from_secs(request.timeout_seconds);
    let engine_version = qualify_engine(&root, &executable, timeout, &environment)?;
    let staged_root = provider::provider_cli_path(&run_directory.project);
    let import_log = provider::provider_cli_path(&run_directory.import_log);
    let runtime_log = provider::provider_cli_path(&run_directory.runtime_log);
    let import_argv = vec![
        "--headless".to_owned(),
        "--no-header".to_owned(),
        "--path".to_owned(),
        staged_root.clone(),
        "--import".to_owned(),
        "--log-file".to_owned(),
        import_log,
    ];
    let runtime_argv = vec![
        "--headless".to_owned(),
        "--no-header".to_owned(),
        "--path".to_owned(),
        staged_root,
        "--quit-after".to_owned(),
        request.quit_after.to_string(),
        "--log-file".to_owned(),
        runtime_log,
    ];
    validate_fixed_argv(&import_argv)?;
    validate_fixed_argv(&runtime_argv)?;
    let import = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &import_argv,
        &run_directory.directory,
        Some(&root),
        timeout,
        &environment,
    )?;
    artifact_store::verify_materialized_bundle(&materialized, &bundle_directory)?;
    let runtime = if process_complete_success(&import) {
        run_directory.write_journal("import_complete")?;
        artifact_store::verify_materialized_bundle(&materialized, &bundle_directory)?;
        provider::run_process_with_environment(
            &executable.canonical_path,
            None,
            &runtime_argv,
            &run_directory.directory,
            Some(&root),
            timeout,
            &environment,
        )?
    } else {
        run_directory.write_journal("import_failed")?;
        return build_import_failure_report(
            request,
            &executable,
            engine_version,
            &materialized,
            assembly_binding,
            upstream,
            &source_after,
            staged_source_manifest_fingerprint,
            &staged_source_bytes,
            &import_argv,
            &runtime_argv,
            &import,
        );
    };
    artifact_store::verify_materialized_bundle(&materialized, &bundle_directory)?;
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Godot executable 在 Runtime Evidence 期间发生漂移".to_owned(),
        ));
    }
    let mut findings = process_findings("runtime", &runtime);
    findings.extend(log_findings(
        [&run_directory.import_log, &run_directory.runtime_log],
        &run_directory.directory,
    )?);
    findings.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then(left.message.cmp(&right.message))
    });
    findings.dedup_by(|left, right| left.code == right.code && left.message == right.message);
    let coverage = if import.stdout.truncated
        || import.stderr.truncated
        || runtime.stdout.truncated
        || runtime.stderr.truncated
    {
        EvidenceCoverage::Partial
    } else {
        EvidenceCoverage::Complete
    };
    let status = if process_complete_success(&runtime) && findings.is_empty() {
        "succeeded"
    } else {
        "failed"
    };
    run_directory.write_journal(if status == "succeeded" {
        "runtime_complete"
    } else {
        "runtime_failed"
    })?;
    build_report(
        request,
        &executable,
        engine_version,
        &materialized,
        assembly_binding,
        upstream,
        &source_after,
        staged_source_manifest_fingerprint,
        &staged_source_bytes,
        &import_argv,
        &runtime_argv,
        &import,
        Some(&runtime),
        coverage,
        findings,
        status,
    )
}

fn qualify_evidence_heads(
    request: &RuntimeRequest<'_>,
    bundle: &artifact_store::RuntimeArtifactBundle,
    executable_sha256: &str,
) -> Result<(EvidenceReference, Vec<EvidenceReference>), AppError> {
    let build = request
        .evidence_heads
        .iter()
        .find(|head| {
            head.plane == EvidencePlane::Build
                && head.provider_id == bundle.build_provider_id()
                && head.freshness == EvidenceFreshness::Fresh
        })
        .ok_or_else(|| AppError::Provider("找不到 bundle 对应的 fresh Build head".to_owned()))?;
    if build.snapshot.coverage != EvidenceCoverage::Complete
        || build.snapshot.provider.authority != EvidenceAuthority::Deterministic
        || !build.snapshot.findings.is_empty()
        || build.snapshot.source_fingerprint != bundle.source_fingerprint()
        || !build.snapshot.artifacts.iter().any(|artifact| {
            artifact.kind == "runtime_artifact_bundle"
                && artifact.content_fingerprint == request.bundle_fingerprint
        })
    {
        return Err(AppError::Provider(
            "Build head 未完整、含 finding、来源不符或未绑定指定 Runtime bundle".to_owned(),
        ));
    }
    let mut upstream = Vec::new();
    for reference in &build.snapshot.upstream {
        let head = request
            .evidence_heads
            .iter()
            .find(|head| {
                head.plane == reference.plane
                    && head.provider_id == reference.provider_id
                    && head.snapshot_fingerprint == reference.snapshot_fingerprint
                    && head.freshness == EvidenceFreshness::Fresh
            })
            .ok_or_else(|| {
                AppError::Provider("Build upstream 不再是当前 fresh Evidence head".to_owned())
            })?;
        if reference.plane == EvidencePlane::Engine
            && !head.snapshot.provider.version.contains(executable_sha256)
        {
            return Err(AppError::Provider(
                "Runtime Godot executable 与 Build 引用的 Engine Evidence 不一致".to_owned(),
            ));
        }
        upstream.push(reference.clone());
    }
    Ok((
        EvidenceReference {
            plane: EvidencePlane::Build,
            provider_id: build.provider_id.clone(),
            snapshot_fingerprint: build.snapshot_fingerprint.clone(),
        },
        upstream,
    ))
}

fn stage_project(root: &Path, destination: &Path) -> Result<StagedSourceManifest, AppError> {
    fs::create_dir_all(destination)?;
    let mut entries = Vec::new();
    for relative_path in git::repository_files(root)? {
        if excluded_source_path(&relative_path) {
            continue;
        }
        let relative = Path::new(&relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AppError::Provider(format!(
                "staging 源路径无效：{relative_path:?}"
            )));
        }
        let source = root.join(relative);
        validate_no_link_components(root, relative)?;
        let metadata = fs::symlink_metadata(&source)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(AppError::Provider(format!(
                "staging 源不是普通文件：{relative_path:?}"
            )));
        }
        let canonical = source.canonicalize()?;
        if !canonical.starts_with(root) {
            return Err(AppError::Provider(format!(
                "staging 源解析后越出项目：{relative_path:?}"
            )));
        }
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&canonical, &target)?;
        let target_metadata = fs::symlink_metadata(&target)?;
        if !target_metadata.is_file() || target_metadata.file_type().is_symlink() {
            return Err(AppError::Provider("staging 目标不是普通文件".to_owned()));
        }
        entries.push(StagedSourceEntry {
            relative_path,
            size: target_metadata.len(),
            sha256: provider::hash_file(&target)?,
        });
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(StagedSourceManifest { entries })
}

fn excluded_source_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        matches!(
            segment,
            ".git" | ".godot" | ".project-brain" | "bin" | "obj" | "artifacts"
        )
    })
}

fn validate_no_link_components(root: &Path, relative: &Path) -> Result<(), AppError> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(AppError::Provider("staging 路径组件无效".to_owned()));
        };
        current.push(segment);
        let metadata = fs::symlink_metadata(&current)?;
        if is_link_or_reparse(&metadata) {
            return Err(AppError::Provider(format!(
                "staging 路径包含 link/reparse component：{}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn qualify_engine(
    root: &Path,
    executable: &provider::PinnedExternalExecutable,
    timeout: Duration,
    environment: &[(&str, &Path)],
) -> Result<String, AppError> {
    let version = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--version".to_owned()],
        root,
        Some(root),
        timeout,
        environment,
    )?;
    if !process_complete_success(&version) {
        return Err(AppError::Provider(
            "Godot Runtime version probe 失败".to_owned(),
        ));
    }
    let version_text = provider::version_text(&version)?;
    if version_text.split('.').next() != Some("4") {
        return Err(AppError::Provider("Runtime v1 只支持 Godot 4".to_owned()));
    }
    let help = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        &["--help".to_owned()],
        root,
        Some(root),
        timeout,
        environment,
    )?;
    if !process_complete_success(&help) {
        return Err(AppError::Provider(
            "Godot Runtime capability probe 失败".to_owned(),
        ));
    }
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout.bytes),
        String::from_utf8_lossy(&help.stderr.bytes)
    );
    for capability in [
        "Godot Engine",
        "--headless",
        "--import",
        "--quit-after",
        "--log-file",
    ] {
        if !output.contains(capability) {
            return Err(AppError::Provider(format!(
                "Godot Runtime capability probe 缺少 {capability:?}"
            )));
        }
    }
    Ok(version_text)
}

fn validate_fixed_argv(argv: &[String]) -> Result<(), AppError> {
    const FORBIDDEN: [&str; 10] = [
        "--build-solutions",
        "--script",
        "--export",
        "--export-debug",
        "--export-release",
        "--export-pack",
        "--install-android-build-template",
        "--doctool",
        "--editor",
        "--project-manager",
    ];
    if argv.iter().any(|argument| {
        FORBIDDEN
            .iter()
            .any(|forbidden| argument.eq_ignore_ascii_case(forbidden))
    }) {
        return Err(AppError::Provider(
            "Runtime 固定 argv 意外包含禁止操作".to_owned(),
        ));
    }
    Ok(())
}

fn process_complete_success(process: &provider::ProcessResult) -> bool {
    process.status.success() && !process.stdout.truncated && !process.stderr.truncated
}

fn process_findings(stage: &str, process: &provider::ProcessResult) -> Vec<EvidenceFinding> {
    let mut findings = Vec::new();
    if process.stdout.truncated || process.stderr.truncated {
        findings.push(EvidenceFinding {
            code: format!("{stage}_output_truncated"),
            severity: FindingSeverity::Warning,
            message: format!(
                "{stage} output exceeded capture bounds; stdout_sha256={} stderr_sha256={}",
                process.stdout.sha256, process.stderr.sha256
            ),
            artifact_id: None,
            path: None,
        });
    } else if !process.status.success() {
        findings.push(EvidenceFinding {
            code: format!("{stage}_exit_failure"),
            severity: FindingSeverity::Error,
            message: format!(
                "{stage} returned exit_code={:?}; stdout_sha256={} stderr_sha256={}",
                process.status.code(),
                process.stdout.sha256,
                process.stderr.sha256
            ),
            artifact_id: None,
            path: None,
        });
    }
    findings
}

fn log_findings<const N: usize>(
    paths: [&Path; N],
    run_root: &Path,
) -> Result<Vec<EvidenceFinding>, AppError> {
    let mut findings = Vec::new();
    for path in paths {
        if !path.is_file() {
            return Err(AppError::Provider(
                "Godot Runtime 未生成受控日志文件".to_owned(),
            ));
        }
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_RUNTIME_LOG_BYTES
        {
            return Err(AppError::Provider(
                "Godot Runtime 日志不是受控普通文件或超过大小上限".to_owned(),
            ));
        }
        let bytes = fs::read(path)?;
        for line in String::from_utf8_lossy(&bytes).lines() {
            let line = line.trim();
            if line.contains("ERROR:") || line.starts_with("SCRIPT ERROR") {
                let sanitized =
                    line.replace(&provider::provider_cli_path(run_root), "<RUNTIME_ROOT>");
                findings.push(EvidenceFinding {
                    code: "godot_runtime_diagnostic".to_owned(),
                    severity: FindingSeverity::Error,
                    message: format!(
                        "Godot runtime diagnostic observed; diagnostic_fingerprint={}",
                        brain_evidence::content_fingerprint(sanitized.as_bytes())
                    ),
                    artifact_id: None,
                    path: None,
                });
            }
        }
    }
    Ok(findings)
}

#[allow(clippy::too_many_arguments)]
fn build_import_failure_report(
    request: &RuntimeRequest<'_>,
    executable: &provider::PinnedExternalExecutable,
    engine_version: String,
    bundle: &artifact_store::RuntimeArtifactBundle,
    assembly_binding: artifact_store::AssemblyBindingAttestation,
    upstream: Vec<EvidenceReference>,
    source_fingerprint: &str,
    staged_source_manifest_fingerprint: String,
    staged_source_manifest_bytes: &[u8],
    import_argv: &[String],
    runtime_argv: &[String],
    import: &provider::ProcessResult,
) -> Result<RuntimeRunReport, AppError> {
    let findings = process_findings("import", import);
    let coverage = if import.stdout.truncated || import.stderr.truncated {
        EvidenceCoverage::Partial
    } else {
        EvidenceCoverage::Complete
    };
    build_report(
        request,
        executable,
        engine_version,
        bundle,
        assembly_binding,
        upstream,
        source_fingerprint,
        staged_source_manifest_fingerprint,
        staged_source_manifest_bytes,
        import_argv,
        runtime_argv,
        import,
        None,
        coverage,
        findings,
        "failed",
    )
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    request: &RuntimeRequest<'_>,
    executable: &provider::PinnedExternalExecutable,
    engine_version: String,
    bundle: &artifact_store::RuntimeArtifactBundle,
    assembly_binding: artifact_store::AssemblyBindingAttestation,
    upstream: Vec<EvidenceReference>,
    source_fingerprint: &str,
    staged_source_manifest_fingerprint: String,
    staged_source_manifest_bytes: &[u8],
    import_argv: &[String],
    runtime_argv: &[String],
    import: &provider::ProcessResult,
    runtime: Option<&provider::ProcessResult>,
    coverage: EvidenceCoverage,
    findings: Vec<EvidenceFinding>,
    status: &'static str,
) -> Result<RuntimeRunReport, AppError> {
    let contract = RuntimeContract {
        contract_version: RUNTIME_CONTRACT_VERSION,
        adapter: "godot-headless-runtime",
        import_argv: redact_argv(import_argv),
        runtime_argv: redact_argv(runtime_argv),
        source_policy: "git_manifest_physical_copy+source_toctou",
        artifact_policy: "content_addressed_bundle+four_phase_rehash",
        user_data_policy: "machine_private_home+stage_override",
        forbidden_operations: ["restore", "build", "test", "script", "export", "release"],
    };
    let provider_id = format!("godot-runtime.{}", bundle.build_provider_id());
    let contract_bytes = serde_json::to_vec(&contract)?;
    let binding_bytes = serde_json::to_vec(&assembly_binding)?;
    let bundle_bytes = bundle.canonical_manifest_bytes()?;
    let artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "runtime_contract",
            "contract",
            "Godot isolated Runtime contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "staged_source_manifest",
            "staged-source",
            "Physical staged Source manifest",
            None,
            staged_source_manifest_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "runtime_artifact_bundle",
            "bundle",
            "Exact Build Runtime artifact bundle",
            None,
            &bundle_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "assembly_binding_attestation",
            "main-assembly",
            "Godot main assembly byte binding",
            Some(&assembly_binding.relative_path),
            &binding_bytes,
        ),
    ];
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Runtime,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{engine_version}+sha256.{}", executable.sha256),
            contract_version: RUNTIME_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        source_fingerprint,
        coverage,
        upstream,
        artifacts,
        Vec::new(),
        findings,
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
    Ok(RuntimeRunReport {
        schema_version: RUNTIME_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        provider_id,
        engine_version,
        executable_sha256: executable.sha256.clone(),
        bundle_fingerprint: request.bundle_fingerprint.to_owned(),
        staged_source_manifest_fingerprint,
        assembly_binding,
        contract,
        import: process_summary(import),
        runtime: runtime.map(process_summary),
        status,
        evidence,
    })
}

fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut previous = "";
    for argument in argv {
        let value = if previous == "--path" {
            "<STAGED_PROJECT>".to_owned()
        } else if previous == "--log-file" {
            "<RUNTIME_LOG>".to_owned()
        } else {
            argument.clone()
        };
        redacted.push(value);
        previous = argument;
    }
    redacted
}

fn process_summary(process: &provider::ProcessResult) -> RuntimeProcessSummary {
    RuntimeProcessSummary {
        duration_ms: process.duration.as_millis(),
        exit_code: process.status.code(),
        stdout_bytes: process.stdout.total_bytes,
        stderr_bytes: process.stderr.total_bytes,
        stdout_sha256: process.stdout.sha256.clone(),
        stderr_sha256: process.stderr.sha256.clone(),
        output_truncated: process.stdout.truncated || process.stderr.truncated,
    }
}

struct RuntimeDirectory {
    run_id: String,
    directory: PathBuf,
    project: PathBuf,
    home: PathBuf,
    app_data: PathBuf,
    local_app_data: PathBuf,
    xdg_data: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
    import_log: PathBuf,
    runtime_log: PathBuf,
}

impl RuntimeDirectory {
    fn create(explicit_install_root: Option<&Path>, project_key: &str) -> Result<Self, AppError> {
        let install_root = setup::resolve_install_root(explicit_install_root)?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let run_id = format!("{}-{nonce}", std::process::id());
        let directory = install_root
            .join("state/runtime-runs")
            .join(project_key)
            .join(&run_id);
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("project-brain-runtime.marker"),
            format!("{project_key}\n{run_id}\n"),
        )?;
        let project = directory.join("project");
        let home = directory.join("home");
        let app_data = directory.join("appdata/roaming");
        let local_app_data = directory.join("appdata/local");
        let xdg_data = directory.join("xdg/data");
        let xdg_config = directory.join("xdg/config");
        let xdg_cache = directory.join("xdg/cache");
        for path in [
            &home,
            &app_data,
            &local_app_data,
            &xdg_data,
            &xdg_config,
            &xdg_cache,
        ] {
            fs::create_dir_all(path)?;
        }
        let run = Self {
            run_id,
            project,
            home,
            app_data,
            local_app_data,
            xdg_data,
            xdg_config,
            xdg_cache,
            import_log: directory.join("import.log"),
            runtime_log: directory.join("runtime.log"),
            directory,
        };
        run.write_journal("created")?;
        Ok(run)
    }

    fn environment(&self) -> [(&str, &Path); 6] {
        [
            ("HOME", &self.home),
            ("APPDATA", &self.app_data),
            ("LOCALAPPDATA", &self.local_app_data),
            ("XDG_DATA_HOME", &self.xdg_data),
            ("XDG_CONFIG_HOME", &self.xdg_config),
            ("XDG_CACHE_HOME", &self.xdg_cache),
        ]
    }

    fn write_journal(&self, state: &str) -> Result<(), AppError> {
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "run_id": self.run_id,
            "state": state,
        }))?;
        setup::atomic_replace(&self.directory.join("journal.json"), &bytes, None)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use brain_evidence::{
        ArtifactNode, EvidenceAuthority, EvidenceCoverage, EvidenceFreshness, EvidencePlane,
        EvidenceProvider, EvidenceReference, EvidenceSnapshot, content_fingerprint,
    };
    use brain_store::EvidenceHeadRecord;

    use super::{
        RuntimeRequest, excluded_source_path, qualify_evidence_heads, redact_argv,
        validate_fixed_argv,
    };
    use crate::artifact_store::RuntimeArtifactBundle;

    #[test]
    fn stage_excludes_machine_and_old_build_state() {
        for path in [
            ".git/config",
            ".godot/editor/state",
            ".project-brain/brain.db",
            "src/bin/generated.dll",
            "obj/cache",
            "artifacts/result",
        ] {
            assert!(excluded_source_path(path));
        }
        assert!(!excluded_source_path("scripts/binocular.cs"));
    }

    #[test]
    fn runtime_argv_rejects_all_code_and_export_surfaces() {
        for argument in [
            "--script",
            "--build-solutions",
            "--export-release",
            "--editor",
        ] {
            assert!(validate_fixed_argv(&[argument.to_owned()]).is_err());
        }
        assert!(
            validate_fixed_argv(&[
                "--headless".to_owned(),
                "--quit-after".to_owned(),
                "120".to_owned()
            ])
            .is_ok()
        );
    }

    #[test]
    fn runtime_contract_removes_machine_stage_and_log_paths() {
        let redacted = redact_argv(&[
            "--path".to_owned(),
            "C:/machine/private/project".to_owned(),
            "--log-file".to_owned(),
            "C:/machine/private/runtime.log".to_owned(),
        ]);
        assert_eq!(
            redacted,
            ["--path", "<STAGED_PROJECT>", "--log-file", "<RUNTIME_LOG>"]
        );
    }

    #[test]
    fn runtime_requires_exact_fresh_build_bundle_and_engine_executable() {
        let project_key = "project-a";
        let bundle: RuntimeArtifactBundle = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "project_key": project_key,
            "build_provider_id": "dotnet-build.main",
            "source_fingerprint": "source_build",
            "artifact_manifest_fingerprint": "sha256_manifest",
            "total_bytes": 0,
            "entries": [],
            "assembly_binding": null
        }))
        .unwrap();
        let bundle_bytes = bundle.canonical_manifest_bytes().unwrap();
        let bundle_fingerprint = content_fingerprint(&bundle_bytes);
        let engine = EvidenceSnapshot::new(
            project_key,
            EvidencePlane::Engine,
            EvidenceProvider {
                id: "godot-engine".to_owned(),
                version: "4.6+sha256.enginehash".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            "source_engine",
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let engine_reference = EvidenceReference {
            plane: EvidencePlane::Engine,
            provider_id: engine.provider.id.clone(),
            snapshot_fingerprint: engine.snapshot_fingerprint.clone(),
        };
        let build = EvidenceSnapshot::new(
            project_key,
            EvidencePlane::Build,
            EvidenceProvider {
                id: "dotnet-build.main".to_owned(),
                version: "9.0+sha256.dotnet".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            "source_build",
            EvidenceCoverage::Complete,
            vec![engine_reference],
            vec![ArtifactNode::from_provider_key(
                project_key,
                "dotnet-build.main",
                "runtime_artifact_bundle",
                "runtime-artifact-bundle",
                "bundle",
                None,
                &bundle_bytes,
            )],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let head = |snapshot: EvidenceSnapshot| EvidenceHeadRecord {
            project_key: project_key.to_owned(),
            plane: snapshot.plane,
            provider_id: snapshot.provider.id.clone(),
            snapshot_fingerprint: snapshot.snapshot_fingerprint.clone(),
            freshness: EvidenceFreshness::Fresh,
            stale_event_id: None,
            stale_reason: None,
            updated_at_unix_seconds: 1,
            last_attestation_sequence: 1,
            snapshot,
        };
        let heads = vec![head(engine), head(build)];
        let request = RuntimeRequest {
            project_root: Path::new("."),
            install_root: None,
            project_key,
            bundle_fingerprint: &bundle_fingerprint,
            executable: Path::new("godot"),
            trust_local_executable: true,
            quit_after: 1,
            timeout_seconds: 1,
            evidence_heads: &heads,
        };

        assert!(qualify_evidence_heads(&request, &bundle, "enginehash").is_ok());
        assert!(qualify_evidence_heads(&request, &bundle, "differenthash").is_err());

        let mut stale_heads = heads.clone();
        stale_heads
            .iter_mut()
            .find(|head| head.plane == EvidencePlane::Build)
            .unwrap()
            .freshness = EvidenceFreshness::Stale;
        let stale_request = RuntimeRequest {
            evidence_heads: &stale_heads,
            ..request
        };
        assert!(qualify_evidence_heads(&stale_request, &bundle, "enginehash").is_err());
    }
}
