use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::{
    ArtifactNode, EvidenceAuthority, EvidenceCoverage, EvidenceFinding, EvidencePlane,
    EvidenceProvider, EvidenceReference, EvidenceSnapshot, FindingAuthority, FindingSeverity,
    content_fingerprint,
};
use serde::{Deserialize, Serialize};

use crate::{artifact_store, error::AppError, git, provider};

const BUILD_RUN_SCHEMA_VERSION: u32 = 1;
const BUILD_CONTRACT_VERSION: u16 = 1;
const MAX_ARTIFACT_FILES: usize = 20_000;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const PYTHON_COMPILE_BOOTSTRAP: &str = r#"import json, pathlib, sys
manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
for item in manifest["files"]:
    path = pathlib.Path(item["absolute_path"])
    compile(path.read_bytes(), item["display_path"], "exec", dont_inherit=True, optimize=0)
"#;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    CompilerOnly,
    RepositoryBuildCode,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildOutputKind {
    ArtifactSet,
    ValidationOnly,
}

#[derive(Debug, Serialize)]
pub struct BuildRunReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    toolchain_version: String,
    executable_sha256: String,
    execution_class: ExecutionClass,
    output_kind: BuildOutputKind,
    contract: BuildContract,
    process: BuildProcessSummary,
    artifact_manifest: Option<ArtifactManifest>,
    runtime_artifact_bundle: Option<artifact_store::RuntimeArtifactBundleReceipt>,
    status: &'static str,
    evidence: EvidenceSnapshot,
}

impl BuildRunReport {
    pub fn evidence_snapshot(&self) -> &EvidenceSnapshot {
        &self.evidence
    }

    pub fn succeeded(&self) -> bool {
        self.status == "succeeded"
    }
}

#[derive(Debug, Serialize)]
struct BuildContract {
    contract_version: u16,
    adapter: &'static str,
    profile_id: String,
    target: String,
    argv: Vec<String>,
    environment_policy: &'static str,
    network_policy: &'static str,
    execution_class: ExecutionClass,
}

#[derive(Debug, Serialize)]
struct BuildProcessSummary {
    duration_ms: u128,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_sha256: String,
    stdout_excerpt: String,
    stderr_sha256: String,
    stderr_excerpt: String,
    output_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ArtifactManifest {
    pub(crate) entries: Vec<ArtifactEntry>,
    pub(crate) total_bytes: u64,
    pub(crate) manifest_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ArtifactEntry {
    pub(crate) relative_path: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

pub struct BuildRequest<'a> {
    pub project_root: &'a Path,
    pub install_root: Option<&'a Path>,
    pub project_key: &'a str,
    pub profile_id: &'a str,
    pub executable: &'a Path,
    pub target: &'a Path,
    pub trust_local_executable: bool,
    pub trust_repository_build_code: bool,
    pub timeout_seconds: u64,
    pub upstream: Vec<EvidenceReference>,
}

pub fn run_dotnet(request: &BuildRequest<'_>) -> Result<BuildRunReport, AppError> {
    require_trust(request, ExecutionClass::RepositoryBuildCode)?;
    let root = request.project_root.canonicalize()?;
    let target = resolve_project_file(&root, request.target, &["csproj"])?;
    let executable =
        provider::pin_external_executable(&root, request.executable, "dotnet executable")?;
    let scratch = BuildScratch::create("dotnet")?;
    prepare_dotnet_environment(&scratch.directory)?;
    let artifacts = scratch.directory.join("artifacts");
    fs::create_dir(&artifacts)?;
    let bin = artifacts.join("bin");
    let bin_debug = bin.join("Debug");
    let obj = artifacts.join("obj");
    let obj_debug = obj.join("Debug");
    prepare_dotnet_restore_state(&root, &target, &obj, !request.upstream.is_empty())?;
    let target_display = project_relative_path(&root, &target)?;
    let argv = vec![
        "build".to_owned(),
        provider::provider_cli_path(&target),
        "--configuration".to_owned(),
        "Debug".to_owned(),
        "--no-restore".to_owned(),
        "--no-incremental".to_owned(),
        "--disable-build-servers".to_owned(),
        "--artifacts-path".to_owned(),
        provider::provider_cli_path(&artifacts),
        format!(
            "--property:GodotProjectDir={}",
            provider::provider_cli_path(&root)
        ),
        format!("--property:BaseOutputPath={}", dotnet_directory_path(&bin)),
        format!(
            "--property:OutputPath={}",
            dotnet_directory_path(&bin_debug)
        ),
        format!(
            "--property:BaseIntermediateOutputPath={}",
            dotnet_directory_path(&obj)
        ),
        format!(
            "--property:IntermediateOutputPath={}",
            dotnet_directory_path(&obj_debug)
        ),
        "--nologo".to_owned(),
    ];
    run_build(
        request,
        &root,
        executable,
        &scratch,
        "dotnet-build",
        &["--version".to_owned()],
        "dotnet",
        &target_display,
        &argv,
        ExecutionClass::RepositoryBuildCode,
        BuildOutputKind::ArtifactSet,
        Some(&bin_debug),
    )
}

fn dotnet_directory_path(path: &Path) -> String {
    let mut rendered = provider::provider_cli_path(path);
    if !rendered.ends_with(std::path::MAIN_SEPARATOR) {
        rendered.push(std::path::MAIN_SEPARATOR);
    }
    rendered
}

fn prepare_dotnet_restore_state(
    root: &Path,
    target: &Path,
    destination: &Path,
    godot_layout: bool,
) -> Result<(), AppError> {
    let source = if godot_layout {
        root.join(".godot").join("mono").join("temp").join("obj")
    } else {
        target
            .parent()
            .ok_or_else(|| AppError::Provider(".NET project 没有父目录".to_owned()))?
            .join("obj")
    };
    if !source.is_dir() {
        return Ok(());
    }
    let canonical_root = root.canonicalize()?;
    let canonical_source = source.canonicalize()?;
    if !canonical_source.starts_with(&canonical_root) {
        return Err(AppError::Provider(
            ".NET restore state 解析后越出项目根目录".to_owned(),
        ));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(&canonical_source)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let allowed = matches!(
            name_text.as_ref(),
            "project.assets.json" | "project.nuget.cache"
        ) || name_text.ends_with(".nuget.g.props")
            || name_text.ends_with(".nuget.g.targets")
            || name_text.ends_with(".nuget.dgspec.json");
        if allowed {
            fs::copy(entry.path(), destination.join(name))?;
        }
    }
    Ok(())
}

pub fn run_rust(request: &BuildRequest<'_>) -> Result<BuildRunReport, AppError> {
    require_trust(request, ExecutionClass::RepositoryBuildCode)?;
    let root = request.project_root.canonicalize()?;
    let manifest = resolve_project_file(&root, request.target, &["toml"])?;
    if manifest.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml") {
        return Err(AppError::Provider(
            "Rust Build Evidence target 必须是 Cargo.toml".to_owned(),
        ));
    }
    let executable =
        provider::pin_external_executable(&root, request.executable, "cargo executable")?;
    let scratch = BuildScratch::create("cargo")?;
    let artifacts = scratch.directory.join("target");
    fs::create_dir(&artifacts)?;
    let target_display = project_relative_path(&root, &manifest)?;
    let argv = vec![
        "build".to_owned(),
        "--manifest-path".to_owned(),
        provider::provider_cli_path(&manifest),
        "--workspace".to_owned(),
        "--all-targets".to_owned(),
        "--frozen".to_owned(),
        "--target-dir".to_owned(),
        provider::provider_cli_path(&artifacts),
    ];
    run_build(
        request,
        &root,
        executable,
        &scratch,
        "cargo-build",
        &["--version".to_owned()],
        "cargo",
        &target_display,
        &argv,
        ExecutionClass::RepositoryBuildCode,
        BuildOutputKind::ArtifactSet,
        Some(&artifacts),
    )
}

pub fn run_python(request: &BuildRequest<'_>) -> Result<BuildRunReport, AppError> {
    require_trust(request, ExecutionClass::CompilerOnly)?;
    let root = request.project_root.canonicalize()?;
    let source_root = resolve_project_directory(&root, request.target)?;
    let executable =
        provider::pin_external_executable(&root, request.executable, "Python executable")?;
    let scratch = BuildScratch::create("python")?;
    let target_display = project_relative_path(&root, &source_root)?;
    let manifest_path = scratch.directory.join("python-sources.json");
    let files = git::repository_files(&root)?
        .into_iter()
        .filter(|path| {
            Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
        })
        .filter_map(|path| {
            let absolute = root.join(Path::new(&path));
            absolute
                .starts_with(&source_root)
                .then_some((path, absolute))
        })
        .map(|(display_path, absolute_path)| {
            serde_json::json!({
                "display_path": display_path,
                "absolute_path": provider::provider_cli_path(&absolute_path),
            })
        })
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err(AppError::Provider(format!(
            "Python source_root={target_display:?} 没有已跟踪或未忽略的 .py 文件"
        )));
    }
    fs::write(
        &manifest_path,
        serde_json::to_vec(&serde_json::json!({ "files": files }))?,
    )?;
    let argv = vec![
        "-I".to_owned(),
        "-S".to_owned(),
        "-B".to_owned(),
        "-c".to_owned(),
        PYTHON_COMPILE_BOOTSTRAP.to_owned(),
        provider::provider_cli_path(&manifest_path),
    ];
    run_build(
        request,
        &root,
        executable,
        &scratch,
        "python-compile",
        &["--version".to_owned()],
        "Python",
        &target_display,
        &argv,
        ExecutionClass::CompilerOnly,
        BuildOutputKind::ValidationOnly,
        None,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_build(
    request: &BuildRequest<'_>,
    root: &Path,
    executable: provider::PinnedExternalExecutable,
    scratch: &BuildScratch,
    adapter: &'static str,
    version_argv: &[String],
    version_marker: &str,
    target_display: &str,
    argv: &[String],
    execution_class: ExecutionClass,
    output_kind: BuildOutputKind,
    artifact_root: Option<&Path>,
) -> Result<BuildRunReport, AppError> {
    validate_profile_id(request.profile_id)?;
    let timeout = Duration::from_secs(request.timeout_seconds);
    let environment_storage = build_environment(&scratch.directory);
    let environment = environment_storage
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect::<Vec<_>>();
    let version_process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        version_argv,
        &scratch.directory,
        Some(root),
        timeout,
        &environment,
    )?;
    require_probe_success("build toolchain version probe", &version_process)?;
    let version = provider::version_text(&version_process)?;
    if !version
        .to_ascii_lowercase()
        .contains(&version_marker.to_ascii_lowercase())
        && adapter != "dotnet-build"
    {
        return Err(AppError::Provider(format!(
            "toolchain version probe 不符合 adapter={adapter}：{version:?}"
        )));
    }
    let source_before = git::worktree_fingerprint(root)?;
    let process = provider::run_process_with_environment(
        &executable.canonical_path,
        None,
        argv,
        &scratch.directory,
        Some(root),
        timeout,
        &environment,
    )?;
    let source_after = git::worktree_fingerprint(root)?;
    if source_before != source_after {
        return Err(AppError::Provider(
            "源码在 Build Evidence 运行期间发生变化；结果已丢弃".to_owned(),
        ));
    }
    if provider::hash_file(&executable.canonical_path)? != executable.sha256 {
        return Err(AppError::Provider(
            "Build toolchain executable 在运行期间发生漂移".to_owned(),
        ));
    }
    let output_truncated = process.stdout.truncated || process.stderr.truncated;
    let infrastructure_failure = !process.status.success()
        && infrastructure_failure(adapter, &process.stdout.bytes, &process.stderr.bytes);
    let coverage = if output_truncated || infrastructure_failure {
        EvidenceCoverage::Partial
    } else {
        EvidenceCoverage::Complete
    };
    let artifact_manifest = artifact_root.map(build_artifact_manifest).transpose()?;
    let mut findings = Vec::new();
    if infrastructure_failure {
        findings.push(EvidenceFinding {
            code: "build_unavailable".to_owned(),
            severity: FindingSeverity::Warning,
            authority: FindingAuthority::Advisory,
            message: format!(
                "{adapter} could not complete its fixed contract because required machine toolchain or prepared dependency state was unavailable; stdout_sha256={} stderr_sha256={}",
                process.stdout.sha256, process.stderr.sha256
            ),
            artifact_id: None,
            path: Some(target_display.to_owned()),
        });
    } else if !process.status.success() {
        findings.push(EvidenceFinding {
            code: "build_exit_failure".to_owned(),
            severity: FindingSeverity::Error,
            authority: FindingAuthority::DeterministicViolation,
            message: format!(
                "{adapter} returned exit_code={:?}; stdout_sha256={} stderr_sha256={}",
                process.status.code(),
                process.stdout.sha256,
                process.stderr.sha256
            ),
            artifact_id: None,
            path: Some(target_display.to_owned()),
        });
    } else if matches!(output_kind, BuildOutputKind::ArtifactSet)
        && artifact_manifest
            .as_ref()
            .is_none_or(|manifest| manifest.entries.is_empty())
    {
        findings.push(EvidenceFinding {
            code: "required_artifact_missing".to_owned(),
            severity: FindingSeverity::Error,
            authority: FindingAuthority::DeterministicViolation,
            message: format!("{adapter} succeeded but produced no regular artifact files"),
            artifact_id: None,
            path: Some(target_display.to_owned()),
        });
    }
    let mut runtime_bundle_failure = None;
    let runtime_artifact_bundle = if !infrastructure_failure
        && findings.is_empty()
        && process.status.success()
        && adapter == "dotnet-build"
    {
        let root = artifact_root.ok_or_else(|| {
            AppError::Provider("dotnet Build 缺少最终产物根，拒绝创建 Runtime bundle".to_owned())
        })?;
        let manifest = artifact_manifest.as_ref().ok_or_else(|| {
            AppError::Provider("dotnet Build 缺少最终产物清单，拒绝创建 Runtime bundle".to_owned())
        })?;
        match artifact_store::promote_runtime_bundle(
            request.install_root,
            request.project_key,
            &format!("{adapter}.{}", request.profile_id),
            target_display,
            &source_after,
            request.project_root,
            root,
            manifest,
        ) {
            Ok(bundle) => Some(bundle),
            Err(error) => {
                runtime_bundle_failure = Some(error.to_string());
                None
            }
        }
    } else {
        None
    };
    if let Some(failure) = &runtime_bundle_failure {
        findings.push(EvidenceFinding {
            code: "runtime_bundle_unavailable".to_owned(),
            severity: FindingSeverity::Warning,
            authority: FindingAuthority::Advisory,
            message: format!(
                "Build completed but exact Runtime bundle could not be committed to machine CAS; failure_fingerprint={}",
                content_fingerprint(failure.as_bytes())
            ),
            artifact_id: None,
            path: None,
        });
    }
    let status = if infrastructure_failure || runtime_bundle_failure.is_some() {
        "incomplete"
    } else if findings.is_empty() && process.status.success() {
        "succeeded"
    } else {
        "failed"
    };
    let contract = BuildContract {
        contract_version: BUILD_CONTRACT_VERSION,
        adapter,
        profile_id: request.profile_id.to_owned(),
        target: target_display.to_owned(),
        argv: redact_machine_paths(argv, root, &scratch.directory),
        environment_policy: "env_clear+adapter_allowlist+machine_scratch",
        network_policy: if adapter == "cargo-build" {
            "offline_frozen"
        } else if adapter == "dotnet-build" {
            "no_restore"
        } else {
            "compiler_only"
        },
        execution_class,
    };
    let contract_bytes = serde_json::to_vec(&contract)?;
    let provider_id = format!("{adapter}.{}", request.profile_id);
    let mut artifacts = vec![
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "build_contract",
            "contract",
            "Build execution contract",
            None,
            &contract_bytes,
        ),
        ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "build_target",
            "target",
            "Canonical project-relative Build target",
            Some(target_display),
            target_display.as_bytes(),
        ),
    ];
    if let Some(manifest) = &artifact_manifest {
        artifacts.push(ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "build_artifact_manifest",
            "artifact-manifest",
            "Build artifact manifest",
            None,
            manifest.manifest_fingerprint.as_bytes(),
        ));
    }
    if let Some(bundle) = &runtime_artifact_bundle {
        artifacts.push(ArtifactNode::from_provider_key(
            request.project_key,
            &provider_id,
            "runtime_artifact_bundle",
            "runtime-artifact-bundle",
            "Content-addressed Runtime artifact bundle",
            None,
            bundle.canonical_manifest_bytes(),
        ));
    }
    let evidence = EvidenceSnapshot::new(
        request.project_key,
        EvidencePlane::Build,
        EvidenceProvider {
            id: provider_id.clone(),
            version: format!("{version}+sha256.{}", executable.sha256),
            contract_version: BUILD_CONTRACT_VERSION,
            authority: EvidenceAuthority::Deterministic,
        },
        &source_after,
        coverage,
        request.upstream.clone(),
        artifacts,
        Vec::new(),
        findings,
    )
    .map_err(|error| AppError::Provider(error.to_string()))?;
    Ok(BuildRunReport {
        schema_version: BUILD_RUN_SCHEMA_VERSION,
        project_key: request.project_key.to_owned(),
        profile_id: request.profile_id.to_owned(),
        provider_id,
        toolchain_version: version,
        executable_sha256: executable.sha256,
        execution_class,
        output_kind,
        contract,
        process: BuildProcessSummary {
            duration_ms: process.duration.as_millis(),
            exit_code: process.status.code(),
            stdout_bytes: process.stdout.total_bytes,
            stderr_bytes: process.stderr.total_bytes,
            stdout_sha256: process.stdout.sha256,
            stdout_excerpt: sanitize_output_excerpt(
                &process.stdout.bytes,
                root,
                &scratch.directory,
            ),
            stderr_sha256: process.stderr.sha256,
            stderr_excerpt: sanitize_output_excerpt(
                &process.stderr.bytes,
                root,
                &scratch.directory,
            ),
            output_truncated,
        },
        artifact_manifest,
        runtime_artifact_bundle,
        status,
        evidence,
    })
}

fn infrastructure_failure(adapter: &str, stdout: &[u8], stderr: &[u8]) -> bool {
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    match adapter {
        "cargo-build" => {
            (output.contains("linker `") && output.contains("not found"))
                || output.contains("rustc not found")
                || (output.contains("attempting to make an http request")
                    && output.contains("--frozen"))
                || output.contains("no matching package named")
                    && (output.contains("offline mode") || output.contains("--frozen"))
        }
        "dotnet-build" => {
            output.contains("netsdk1004")
                || output.contains("assets file") && output.contains("not found")
                || output.contains("the sdk '") && output.contains("specified could not be found")
                || output.contains("msb4236")
        }
        _ => false,
    }
}

fn require_trust(request: &BuildRequest<'_>, class: ExecutionClass) -> Result<(), AppError> {
    if !request.trust_local_executable {
        return Err(AppError::Provider(
            "Build Evidence 需要显式传入 --trust-local-executable".to_owned(),
        ));
    }
    if matches!(class, ExecutionClass::RepositoryBuildCode) && !request.trust_repository_build_code
    {
        return Err(AppError::Provider(
            "dotnet/cargo 构建可能执行仓库控制的 MSBuild task 或 build.rs；需要显式传入 --trust-repository-build-code"
                .to_owned(),
        ));
    }
    Ok(())
}

fn require_probe_success(stage: &str, process: &provider::ProcessResult) -> Result<(), AppError> {
    if process.stdout.truncated || process.stderr.truncated || !process.status.success() {
        return Err(AppError::Provider(format!(
            "{stage} 未产生完整成功证据：exit_code={:?} stdout_truncated={} stderr_truncated={}",
            process.status.code(),
            process.stdout.truncated,
            process.stderr.truncated
        )));
    }
    Ok(())
}

fn validate_profile_id(profile_id: &str) -> Result<(), AppError> {
    if profile_id.is_empty()
        || profile_id.len() > 96
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AppError::Provider(
            "Build profile ID 只能包含 ASCII 字母、数字、点、横线和下划线".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_project_file(
    root: &Path,
    relative: &Path,
    extensions: &[&str],
) -> Result<PathBuf, AppError> {
    let path = resolve_project_path(root, relative)?;
    if !path.is_file()
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                extensions
                    .iter()
                    .any(|item| value.eq_ignore_ascii_case(item))
            })
    {
        return Err(AppError::Provider(format!(
            "Build target 不是允许的项目文件：{}",
            relative.display()
        )));
    }
    Ok(path)
}

fn resolve_project_directory(root: &Path, relative: &Path) -> Result<PathBuf, AppError> {
    let path = resolve_project_path(root, relative)?;
    if !path.is_dir() {
        return Err(AppError::Provider(format!(
            "Python source root 不是目录：{}",
            relative.display()
        )));
    }
    Ok(path)
}

fn resolve_project_path(root: &Path, relative: &Path) -> Result<PathBuf, AppError> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AppError::RepositoryPathOutsideRoot(relative.to_owned()));
    }
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err(AppError::RepositoryPathOutsideRoot(path));
    }
    Ok(path)
}

fn project_relative_path(root: &Path, path: &Path) -> Result<String, AppError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AppError::RepositoryPathOutsideRoot(path.to_owned()))?;
    let normalized = relative.to_string_lossy().replace('\\', "/");
    Ok(if normalized.is_empty() {
        ".".to_owned()
    } else {
        normalized
    })
}

fn build_environment(scratch: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut environment = vec![
        ("HOME", scratch.to_owned()),
        ("USERPROFILE", scratch.to_owned()),
        ("TEMP", scratch.to_owned()),
        ("TMP", scratch.to_owned()),
        ("TMPDIR", scratch.to_owned()),
        ("DOTNET_CLI_HOME", scratch.to_owned()),
        ("DOTNET_CLI_TELEMETRY_OPTOUT", PathBuf::from("1")),
        ("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", PathBuf::from("1")),
        ("DOTNET_NOLOGO", PathBuf::from("1")),
        (
            "DOTNET_CLI_WORKLOAD_UPDATE_NOTIFY_DISABLE",
            PathBuf::from("1"),
        ),
        ("APPDATA", scratch.join("appdata/roaming")),
        ("LOCALAPPDATA", scratch.join("appdata/local")),
    ];
    if let Some(nuget_packages) = machine_cache_path("NUGET_PACKAGES", ".nuget/packages") {
        environment.push(("NUGET_PACKAGES", nuget_packages.clone()));
        environment.push((
            "NuGetPackageRoot",
            PathBuf::from(dotnet_directory_path(&nuget_packages)),
        ));
    }
    if let Some(cargo_home) = machine_cache_path("CARGO_HOME", ".cargo") {
        environment.push(("CARGO_HOME", cargo_home));
    }
    if let Some(rustup_home) = machine_cache_path("RUSTUP_HOME", ".rustup") {
        environment.push(("RUSTUP_HOME", rustup_home));
    }
    environment
}

fn machine_cache_path(variable: &str, user_relative: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from).or_else(|| {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .map(|home| home.join(user_relative))
    })
}

fn prepare_dotnet_environment(scratch: &Path) -> Result<(), AppError> {
    let nuget = scratch.join("appdata/roaming/NuGet");
    fs::create_dir_all(&nuget)?;
    fs::create_dir_all(scratch.join("appdata/local"))?;
    fs::write(
        nuget.join("NuGet.Config"),
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?><configuration />\n",
    )?;
    Ok(())
}

fn build_artifact_manifest(root: &Path) -> Result<ArtifactManifest, AppError> {
    let canonical_root = root.canonicalize()?;
    let mut directories = vec![canonical_root.clone()];
    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    while let Some(directory) = directories.pop() {
        let mut children = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(AppError::Provider(format!(
                    "Build artifact 包含链接，拒绝纳入证据：{}",
                    path.display()
                )));
            }
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                return Err(AppError::Provider(
                    "Build artifact 解析后越出 machine scratch".to_owned(),
                ));
            }
            if metadata.is_dir() {
                directories.push(canonical);
                continue;
            }
            if !metadata.is_file() {
                return Err(AppError::Provider(
                    "Build artifact 不是普通文件或目录".to_owned(),
                ));
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| AppError::Provider("Build artifact 总大小溢出".to_owned()))?;
            if entries.len() >= MAX_ARTIFACT_FILES || total_bytes > MAX_ARTIFACT_BYTES {
                return Err(AppError::Provider(
                    "Build artifact manifest 超过文件数或总字节安全上限".to_owned(),
                ));
            }
            entries.push(ArtifactEntry {
                relative_path: project_relative_path(&canonical_root, &canonical)?,
                size: metadata.len(),
                sha256: provider::hash_file(&canonical)?,
            });
        }
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let manifest_fingerprint = content_fingerprint(&serde_json::to_vec(&entries)?);
    Ok(ArtifactManifest {
        entries,
        total_bytes,
        manifest_fingerprint,
    })
}

fn redact_machine_paths(argv: &[String], root: &Path, scratch: &Path) -> Vec<String> {
    let root = provider::provider_cli_path(root);
    let scratch = provider::provider_cli_path(scratch);
    argv.iter()
        .map(|argument| {
            argument
                .replace(&root, "<PROJECT_ROOT>")
                .replace(&scratch, "<MACHINE_SCRATCH>")
        })
        .collect()
}

fn sanitize_output_excerpt(bytes: &[u8], root: &Path, scratch: &Path) -> String {
    let excerpt = String::from_utf8_lossy(bytes)
        .chars()
        .take(4_096)
        .collect::<String>();
    redact_machine_paths(&[excerpt], root, scratch)
        .pop()
        .unwrap_or_default()
}

struct BuildScratch {
    base: PathBuf,
    directory: PathBuf,
}

impl BuildScratch {
    fn create(kind: &str) -> Result<Self, AppError> {
        let base = std::env::temp_dir().canonicalize()?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let directory = base.join(format!(
            "project-brain-build-{kind}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self { base, directory })
    }
}

impl Drop for BuildScratch {
    fn drop(&mut self) {
        if self.directory.starts_with(&self.base) && self.directory != self.base {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BuildRequest, ExecutionClass, infrastructure_failure, prepare_dotnet_restore_state,
        require_trust, validate_profile_id,
    };
    use std::{fs, path::Path};

    fn request() -> BuildRequest<'static> {
        BuildRequest {
            project_root: Path::new("."),
            install_root: None,
            project_key: "project",
            profile_id: "main",
            executable: Path::new("tool"),
            target: Path::new("target"),
            trust_local_executable: true,
            trust_repository_build_code: false,
            timeout_seconds: 1,
            upstream: Vec::new(),
        }
    }

    #[test]
    fn repository_build_code_requires_a_separate_explicit_trust_bit() {
        let request = request();
        assert!(require_trust(&request, ExecutionClass::CompilerOnly).is_ok());
        assert!(require_trust(&request, ExecutionClass::RepositoryBuildCode).is_err());
    }

    #[test]
    fn build_profile_identity_is_bounded_and_shell_free() {
        assert!(validate_profile_id("godot-debug").is_ok());
        assert!(validate_profile_id("bad/profile").is_err());
        assert!(validate_profile_id("x;echo").is_err());
    }

    #[test]
    fn unavailable_toolchain_is_not_misclassified_as_a_project_build_violation() {
        assert!(infrastructure_failure(
            "cargo-build",
            b"",
            b"error: linker `link.exe` not found"
        ));
        assert!(infrastructure_failure(
            "dotnet-build",
            b"error NETSDK1004: Assets file 'obj/project.assets.json' not found",
            b""
        ));
        assert!(!infrastructure_failure(
            "cargo-build",
            b"",
            b"error[E0308]: mismatched types"
        ));
    }

    #[test]
    fn dotnet_restore_state_copies_only_allowed_metadata() {
        let scratch = super::BuildScratch::create("restore-state-test").unwrap();
        let root = scratch.directory.join("project");
        let source = root.join("obj");
        let destination = scratch.directory.join("isolated-obj");
        fs::create_dir_all(&source).unwrap();
        fs::write(root.join("game.csproj"), b"<Project />").unwrap();
        fs::write(source.join("project.assets.json"), b"assets").unwrap();
        fs::write(source.join("game.csproj.nuget.g.props"), b"props").unwrap();
        fs::write(source.join("generated.dll"), b"not restore state").unwrap();

        prepare_dotnet_restore_state(&root, &root.join("game.csproj"), &destination, false)
            .unwrap();

        assert!(destination.join("project.assets.json").is_file());
        assert!(destination.join("game.csproj.nuget.g.props").is_file());
        assert!(!destination.join("generated.dll").exists());
    }
}
