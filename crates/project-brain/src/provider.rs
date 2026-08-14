use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_core::SemanticProviderProfile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::AppError,
    git,
    setup::{
        MutationLock, atomic_replace, ensure_install_ready, pretty_json_bytes,
        resolve_install_root, target_hash,
    },
};

pub(crate) use crate::execution::ProcessResult;

#[cfg(test)]
pub(crate) use crate::execution::CapturedOutput;
#[cfg(test)]
use crate::execution::MAX_CAPTURE_BYTES;

const PROVIDER_SCHEMA_VERSION: u32 = 1;
const MAX_SCIP_BYTES: u64 = 512 * 1024 * 1024;
const MAX_AUDIT_BYTES: u64 = 4 * 1024 * 1024;
const AUDIT_RETAIN_BYTES: usize = 2 * 1024 * 1024;
const MAX_LAUNCHER_PACKAGE_FILES: usize = 20_000;
const MAX_LAUNCHER_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProviderArtifact {
    canonical_path: PathBuf,
    sha256: String,
}

pub(crate) struct PinnedExternalExecutable {
    pub canonical_path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProviderBinding {
    registration_id: String,
    revision: u64,
    project_key: String,
    profile_id: String,
    producer: String,
    executable: ProviderArtifact,
    launcher_script: Option<ProviderArtifact>,
    #[serde(default)]
    launcher_package_manifest_sha256: Option<String>,
    resolved_version: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProviderRegistry {
    schema_version: u32,
    bindings: Vec<ProviderBinding>,
}

#[derive(Debug, Serialize)]
pub struct ProviderBindReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    producer: String,
    executable: PathBuf,
    launcher_script: Option<PathBuf>,
    launcher_package_manifest_sha256: Option<String>,
    executable_sha256: String,
    resolved_version: String,
    registration_id: String,
    revision: u64,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ProviderUnbindReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ProviderListReport {
    schema_version: u32,
    project_key: String,
    profiles: Vec<ProviderStatus>,
}

#[derive(Debug, Serialize)]
struct ProviderStatus {
    profile_id: String,
    producer: String,
    state: &'static str,
    executable: Option<PathBuf>,
    launcher_script: Option<PathBuf>,
    resolved_version: Option<String>,
    issue: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderExecutionReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    producer: String,
    registration_id: String,
    registration_revision: u64,
    executable_sha256: String,
    launcher_package_manifest_sha256: Option<String>,
    resolved_version: String,
    duration_ms: u128,
    exit_code: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    stdout_truncated: bool,
    stderr_truncated: bool,
    stdout_sha256: String,
    stderr_sha256: String,
    artifact_sha256: String,
    output_bytes: u64,
    source_fingerprint_before: String,
    source_fingerprint_after: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderDoctorReport {
    pub ready: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderTrustStatus {
    pub profile_id: String,
    pub ready: bool,
    pub registration_id: Option<String>,
    pub registration_revision: Option<u64>,
    pub executable_sha256: Option<String>,
    pub launcher_package_manifest_sha256: Option<String>,
    pub issue: Option<String>,
}

pub fn trust_status(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profiles: &[SemanticProviderProfile],
) -> BTreeMap<String, ProviderTrustStatus> {
    if profiles.is_empty() {
        return BTreeMap::new();
    }
    let registry = resolve_install_root(explicit_install_root)
        .and_then(|root| read_registry(&root))
        .map_err(|error| error.to_string());
    let canonical_root = project_root
        .canonicalize()
        .map_err(|error| error.to_string());
    profiles
        .iter()
        .map(|profile| {
            let status = match (&registry, &canonical_root) {
                (Ok(registry), Ok(root)) => {
                    let binding = registry.bindings.iter().find(|binding| {
                        binding.project_key == project_key && binding.profile_id == profile.id
                    });
                    if let Some(binding) = binding {
                        let issue = validate_binding(binding, profile)
                            .and_then(|()| {
                                reject_repository_artifact(
                                    root,
                                    &binding.executable,
                                    "Provider executable",
                                )?;
                                if let Some(script) = &binding.launcher_script {
                                    reject_repository_artifact(
                                        root,
                                        script,
                                        "Provider launcher script",
                                    )?;
                                }
                                Ok(())
                            })
                            .err()
                            .map(|error| error.to_string());
                        ProviderTrustStatus {
                            profile_id: profile.id.clone(),
                            ready: issue.is_none(),
                            registration_id: Some(binding.registration_id.clone()),
                            registration_revision: Some(binding.revision),
                            executable_sha256: Some(binding.executable.sha256.clone()),
                            launcher_package_manifest_sha256: binding
                                .launcher_package_manifest_sha256
                                .clone(),
                            issue,
                        }
                    } else {
                        ProviderTrustStatus {
                            profile_id: profile.id.clone(),
                            ready: false,
                            registration_id: None,
                            registration_revision: None,
                            executable_sha256: None,
                            launcher_package_manifest_sha256: None,
                            issue: Some(format!("provider profile={} 尚未在本机绑定", profile.id)),
                        }
                    }
                }
                (Err(error), _) | (_, Err(error)) => ProviderTrustStatus {
                    profile_id: profile.id.clone(),
                    ready: false,
                    registration_id: None,
                    registration_revision: None,
                    executable_sha256: None,
                    launcher_package_manifest_sha256: None,
                    issue: Some(error.clone()),
                },
            };
            (profile.id.clone(), status)
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct ProviderAuditRecord<'a> {
    schema_version: u32,
    timestamp_unix_ms: u128,
    project_key: &'a str,
    profile_id: &'a str,
    stage: &'a str,
    outcome: &'a str,
    duration_ms: Option<u128>,
    exit_code: Option<i32>,
    stdout_bytes: Option<usize>,
    stderr_bytes: Option<usize>,
    stdout_truncated: Option<bool>,
    stderr_truncated: Option<bool>,
    stdout_sha256: Option<String>,
    stderr_sha256: Option<String>,
    registration_id: Option<&'a str>,
    registration_revision: Option<u64>,
    executable_sha256: Option<&'a str>,
    artifact_sha256: Option<&'a str>,
    source_fingerprint_before: Option<&'a str>,
    source_fingerprint_after: Option<&'a str>,
    failure_kind: Option<&'a str>,
}

pub struct ProviderRun {
    install_root: PathBuf,
    temp_dir: PathBuf,
    output_path: PathBuf,
    report: ProviderExecutionReport,
}

impl ProviderRun {
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn report(&self) -> &ProviderExecutionReport {
        &self.report
    }

    pub fn source_fingerprint(&self) -> &str {
        &self.report.source_fingerprint_after
    }

    pub fn registration_id(&self) -> &str {
        &self.report.registration_id
    }

    pub fn executable_sha256(&self) -> &str {
        &self.report.executable_sha256
    }

    pub fn artifact_sha256(&self) -> &str {
        &self.report.artifact_sha256
    }

    pub fn registration_revision(&self) -> u64 {
        self.report.registration_revision
    }
}

impl Drop for ProviderRun {
    fn drop(&mut self) {
        let runs_root = self.install_root.join("state/provider-runs");
        if self.temp_dir.starts_with(&runs_root) && self.temp_dir != runs_root {
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "绑定流程按探测、审计、CAS 写入顺序保留在同一事务函数中"
)]
pub fn bind(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profiles: &[SemanticProviderProfile],
    profile_id: &str,
    executable: &Path,
    launcher_script: Option<&Path>,
    replace: bool,
    trust_local_executable: bool,
    timeout_seconds: u64,
) -> Result<ProviderBindReport, AppError> {
    if !trust_local_executable {
        return Err(AppError::Provider(
            "绑定本机可执行文件需要显式传入 --trust-local-executable".to_owned(),
        ));
    }
    let profile = configured_profile(profiles, profile_id)?;
    ensure_known_producer(&profile.producer)?;
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let project_root = project_root.canonicalize()?;
    let executable = pinned_artifact(executable, "Provider executable")?;
    reject_repository_artifact(&project_root, &executable, "Provider executable")?;
    reject_command_script(&executable.canonical_path)?;
    let launcher_script = launcher_script
        .map(|path| pinned_artifact(path, "Provider launcher script"))
        .transpose()?;
    if let Some(script) = &launcher_script {
        reject_repository_artifact(&project_root, script, "Provider launcher script")?;
    }
    if launcher_script.is_some() && !profile.producer.eq_ignore_ascii_case("scip-python") {
        return Err(AppError::Provider(
            "launcher script 只用于通过原生 node executable 启动 scip-python".to_owned(),
        ));
    }
    let launcher_package_manifest_sha256 = match (
        profile.producer.eq_ignore_ascii_case("scip-python"),
        launcher_script.as_ref(),
    ) {
        (true, Some(script)) => Some(scip_python_package_manifest(script)?),
        _ => None,
    };

    let probe = run_process(
        &executable.canonical_path,
        launcher_script
            .as_ref()
            .map(|item| item.canonical_path.as_path()),
        &["--version".to_owned()],
        &install_root,
        None,
        Duration::from_secs(timeout_seconds),
    )?;
    if !probe.status.success() {
        let error = format!("Provider 版本探测失败，exit_code={:?}", probe.status.code());
        append_audit(
            &install_root,
            &audit_for_process(
                project_key,
                profile_id,
                "version_probe",
                "failed",
                &probe,
                Some("non_zero_exit"),
            )?,
        )?;
        return Err(AppError::Provider(error));
    }
    let resolved_version = version_text(&probe)?;
    if let Err(error) = verify_probe_identity(
        &profile.producer,
        &executable,
        launcher_script.as_ref(),
        &resolved_version,
    ) {
        append_audit(
            &install_root,
            &audit_for_process(
                project_key,
                profile_id,
                "version_probe",
                "failed",
                &probe,
                Some("identity_mismatch"),
            )?,
        )?;
        return Err(error);
    }
    append_audit(
        &install_root,
        &audit_for_process(
            project_key,
            profile_id,
            "version_probe",
            "success",
            &probe,
            None,
        )?,
    )?;
    let mut binding = ProviderBinding {
        registration_id: registration_id(
            project_key,
            &profile.id,
            &profile.producer,
            &executable,
            launcher_script.as_ref(),
            launcher_package_manifest_sha256.as_deref(),
        ),
        revision: 1,
        project_key: project_key.to_owned(),
        profile_id: profile.id.clone(),
        producer: profile.producer.clone(),
        executable,
        launcher_script,
        launcher_package_manifest_sha256,
        resolved_version: resolved_version.clone(),
    };

    let _lock = MutationLock::acquire(&install_root.join("state/providers.lock"))?;
    let registry_path = install_root.join("state/providers.json");
    let before_hash = target_hash(&registry_path)?;
    let mut registry = read_registry(&install_root)?;
    let existing = registry.bindings.iter().position(|item| {
        item.project_key == project_key && item.profile_id.eq_ignore_ascii_case(profile_id)
    });
    let changed = match existing {
        Some(index) if binding_equivalent(&registry.bindings[index], &binding) => {
            binding
                .registration_id
                .clone_from(&registry.bindings[index].registration_id);
            binding.revision = registry.bindings[index].revision;
            false
        }
        Some(_) if !replace => {
            return Err(AppError::Provider(format!(
                "profile={profile_id:?} 已绑定且内容不同；请显式传入 --replace"
            )));
        }
        Some(index) => {
            binding.revision = registry.bindings[index].revision.saturating_add(1);
            registry.bindings[index] = binding.clone();
            true
        }
        None => {
            registry.bindings.push(binding.clone());
            true
        }
    };
    if changed {
        sort_bindings(&mut registry.bindings);
        atomic_replace(
            &registry_path,
            &pretty_json_bytes(&registry)?,
            Some(&before_hash),
        )?;
    }

    Ok(ProviderBindReport {
        schema_version: PROVIDER_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile.id.clone(),
        producer: profile.producer.clone(),
        executable: binding.executable.canonical_path,
        launcher_script: binding
            .launcher_script
            .map(|artifact| artifact.canonical_path),
        launcher_package_manifest_sha256: binding.launcher_package_manifest_sha256,
        executable_sha256: binding.executable.sha256,
        resolved_version,
        registration_id: binding.registration_id,
        revision: binding.revision,
        changed,
    })
}

pub fn unbind(
    explicit_install_root: Option<&Path>,
    project_key: &str,
    profile_id: &str,
) -> Result<ProviderUnbindReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let _lock = MutationLock::acquire(&install_root.join("state/providers.lock"))?;
    let registry_path = install_root.join("state/providers.json");
    let before_hash = target_hash(&registry_path)?;
    let mut registry = read_registry(&install_root)?;
    let before = registry.bindings.len();
    registry.bindings.retain(|item| {
        item.project_key != project_key || !item.profile_id.eq_ignore_ascii_case(profile_id)
    });
    let changed = before != registry.bindings.len();
    if changed {
        atomic_replace(
            &registry_path,
            &pretty_json_bytes(&registry)?,
            Some(&before_hash),
        )?;
    }
    Ok(ProviderUnbindReport {
        schema_version: PROVIDER_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile_id.to_owned(),
        changed,
    })
}

pub fn list(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profiles: &[SemanticProviderProfile],
) -> Result<ProviderListReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let registry = read_registry(&install_root)?;
    let canonical_root = project_root.canonicalize()?;
    let profiles = profiles
        .iter()
        .map(|profile| provider_status(project_key, profile, &registry, Some(&canonical_root)))
        .collect();
    Ok(ProviderListReport {
        schema_version: PROVIDER_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profiles,
    })
}

pub fn doctor(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profiles: &[SemanticProviderProfile],
) -> ProviderDoctorReport {
    let install_root = match resolve_install_root(explicit_install_root) {
        Ok(root) => root,
        Err(error) => {
            return ProviderDoctorReport {
                ready: false,
                issues: vec![error.to_string()],
            };
        }
    };
    let registry = match read_registry(&install_root) {
        Ok(registry) => registry,
        Err(error) => {
            return ProviderDoctorReport {
                ready: false,
                issues: vec![error.to_string()],
            };
        }
    };
    let mut issues = Vec::new();
    let canonical_root = project_root.canonicalize().ok();
    for profile in profiles {
        let status = provider_status(project_key, profile, &registry, canonical_root.as_deref());
        if status.state != "ready" {
            issues.push(
                status
                    .issue
                    .unwrap_or_else(|| format!("provider profile={} 未就绪", profile.id)),
            );
        }
    }
    ProviderDoctorReport {
        ready: issues.is_empty(),
        issues,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "执行流程集中维护锁、隔离目录、进程、输出校验与审计的清理不变量"
)]
pub fn execute(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profiles: &[SemanticProviderProfile],
    profile_id: &str,
    timeout_seconds: u64,
) -> Result<ProviderRun, AppError> {
    let profile = configured_profile(profiles, profile_id)?;
    ensure_known_producer(&profile.producer)?;
    let project_root = project_root.canonicalize()?;
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let registry = read_registry(&install_root)?;
    let binding = registry
        .bindings
        .into_iter()
        .find(|item| item.project_key == project_key && item.profile_id == profile.id)
        .ok_or_else(|| {
            AppError::Provider(format!(
                "profile={} 尚未在本机绑定；请先执行 provider bind",
                profile.id
            ))
        })?;
    validate_binding(&binding, profile)?;
    reject_repository_artifact(&project_root, &binding.executable, "Provider executable")?;
    if let Some(script) = &binding.launcher_script {
        reject_repository_artifact(&project_root, script, "Provider launcher script")?;
    }

    let worktree_key = digest_path(&project_root);
    let lock_name = format!("{}--{}--{}.lock", project_key, profile.id, worktree_key);
    let _lock = MutationLock::acquire(&install_root.join("state/provider-locks").join(lock_name))?;
    let runs_root = install_root.join("state/provider-runs");
    fs::create_dir_all(&runs_root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp_dir = runs_root.join(format!(
        "{}--{}--{}--{nonce}",
        project_key,
        profile.id,
        std::process::id()
    ));
    fs::create_dir(&temp_dir)?;
    secure_directory(&temp_dir)?;
    let output_path = temp_dir.join("index.scip");
    let arguments =
        provider_arguments(&profile.producer, &project_root, project_key, &output_path)?;
    let source_fingerprint_before = git::worktree_fingerprint(&project_root)?;
    let process = match run_process(
        &binding.executable.canonical_path,
        binding
            .launcher_script
            .as_ref()
            .map(|item| item.canonical_path.as_path()),
        &arguments,
        &temp_dir,
        Some(&project_root),
        Duration::from_secs(timeout_seconds),
    ) {
        Ok(process) => process,
        Err(error) => {
            let record = ProviderAuditRecord {
                schema_version: PROVIDER_SCHEMA_VERSION,
                timestamp_unix_ms: unix_ms()?,
                project_key,
                profile_id,
                stage: "index_process",
                outcome: "failed",
                duration_ms: None,
                exit_code: None,
                stdout_bytes: None,
                stderr_bytes: None,
                stdout_truncated: None,
                stderr_truncated: None,
                stdout_sha256: None,
                stderr_sha256: None,
                registration_id: Some(&binding.registration_id),
                registration_revision: Some(binding.revision),
                executable_sha256: Some(&binding.executable.sha256),
                artifact_sha256: None,
                source_fingerprint_before: Some(&source_fingerprint_before),
                source_fingerprint_after: None,
                failure_kind: Some("spawn_or_timeout"),
            };
            append_audit(&install_root, &record)?;
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
    };
    if !process.status.success() {
        append_audit(
            &install_root,
            &audit_for_process(
                project_key,
                profile_id,
                "index_process",
                "failed",
                &process,
                Some("non_zero_exit"),
            )?,
        )?;
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(AppError::Provider(format!(
            "provider profile={} 退出失败，exit_code={:?}，stdout_sha256={}，stderr_sha256={}",
            profile.id,
            process.status.code(),
            process.stdout.sha256,
            process.stderr.sha256
        )));
    }
    let source_fingerprint_after = git::worktree_fingerprint(&project_root)?;
    if source_fingerprint_before != source_fingerprint_after {
        append_audit(
            &install_root,
            &audit_for_process(
                project_key,
                profile_id,
                "index_process",
                "failed",
                &process,
                Some("source_changed"),
            )?,
        )?;
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(AppError::Provider(
            "源码在 Provider 索引期间发生变化；输出已丢弃".to_owned(),
        ));
    }
    validate_output(&output_path)?;
    secure_file(&output_path)?;
    let output_bytes = fs::metadata(&output_path)?.len();
    let artifact_sha256 = hash_file(&output_path)?;
    let report = ProviderExecutionReport {
        schema_version: PROVIDER_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile.id.clone(),
        producer: profile.producer.clone(),
        registration_id: binding.registration_id,
        registration_revision: binding.revision,
        executable_sha256: binding.executable.sha256,
        launcher_package_manifest_sha256: binding.launcher_package_manifest_sha256,
        resolved_version: binding.resolved_version,
        duration_ms: process.duration.as_millis(),
        exit_code: process.status.code(),
        stdout_bytes: process.stdout.total_bytes,
        stderr_bytes: process.stderr.total_bytes,
        stdout_truncated: process.stdout.truncated,
        stderr_truncated: process.stderr.truncated,
        stdout_sha256: process.stdout.sha256.clone(),
        stderr_sha256: process.stderr.sha256.clone(),
        artifact_sha256,
        output_bytes,
        source_fingerprint_before,
        source_fingerprint_after,
    };
    append_audit(
        &install_root,
        &audit_for_process(
            project_key,
            profile_id,
            "index_process",
            "success",
            &process,
            None,
        )?,
    )?;
    Ok(ProviderRun {
        install_root,
        temp_dir,
        output_path,
        report,
    })
}

pub fn record_import_failure(
    explicit_install_root: Option<&Path>,
    run: &ProviderRun,
    _error: &AppError,
) -> Result<(), AppError> {
    record_import_event(
        explicit_install_root,
        run,
        "scip_import_or_commit",
        "failed",
        Some("validation_or_store"),
    )
}

pub fn record_import_prepared(
    explicit_install_root: Option<&Path>,
    run: &ProviderRun,
) -> Result<(), AppError> {
    record_import_event(
        explicit_install_root,
        run,
        "semantic_commit_prepared",
        "success",
        None,
    )
}

pub fn record_import_committed(
    explicit_install_root: Option<&Path>,
    run: &ProviderRun,
) -> Result<(), AppError> {
    record_import_event(
        explicit_install_root,
        run,
        "semantic_commit",
        "success",
        None,
    )
}

pub fn record_stability_observed(
    explicit_install_root: Option<&Path>,
    run: &ProviderRun,
) -> Result<(), AppError> {
    record_import_event(
        explicit_install_root,
        run,
        "stability_probe",
        "success",
        None,
    )
}

fn record_import_event(
    explicit_install_root: Option<&Path>,
    run: &ProviderRun,
    stage: &str,
    outcome: &str,
    failure_kind: Option<&str>,
) -> Result<(), AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let record = ProviderAuditRecord {
        schema_version: PROVIDER_SCHEMA_VERSION,
        timestamp_unix_ms: unix_ms()?,
        project_key: &run.report.project_key,
        profile_id: &run.report.profile_id,
        stage,
        outcome,
        duration_ms: None,
        exit_code: None,
        stdout_bytes: None,
        stderr_bytes: None,
        stdout_truncated: None,
        stderr_truncated: None,
        stdout_sha256: None,
        stderr_sha256: None,
        registration_id: Some(&run.report.registration_id),
        registration_revision: Some(run.report.registration_revision),
        executable_sha256: Some(&run.report.executable_sha256),
        artifact_sha256: Some(&run.report.artifact_sha256),
        source_fingerprint_before: Some(&run.report.source_fingerprint_before),
        source_fingerprint_after: Some(&run.report.source_fingerprint_after),
        failure_kind,
    };
    append_audit(&install_root, &record)
}

fn configured_profile<'a>(
    profiles: &'a [SemanticProviderProfile],
    profile_id: &str,
) -> Result<&'a SemanticProviderProfile, AppError> {
    profiles
        .iter()
        .find(|profile| profile.id.eq_ignore_ascii_case(profile_id))
        .ok_or_else(|| {
            AppError::Provider(format!(
                "仓库未声明 semantic provider profile={profile_id:?}"
            ))
        })
}

fn ensure_known_producer(producer: &str) -> Result<(), AppError> {
    if ["rust-analyzer", "scip-dotnet", "scip-python"]
        .iter()
        .any(|known| producer.eq_ignore_ascii_case(known))
    {
        Ok(())
    } else {
        Err(AppError::Provider(format!(
            "producer={producer:?} 没有内建安全 argv 契约；仍可使用 index-scip 手工导入"
        )))
    }
}

fn verify_probe_identity(
    producer: &str,
    executable: &ProviderArtifact,
    launcher_script: Option<&ProviderArtifact>,
    version: &str,
) -> Result<(), AppError> {
    let mut identity = format!(
        "{} {}",
        executable.canonical_path.to_string_lossy(),
        version
    )
    .to_ascii_lowercase();
    if let Some(script) = launcher_script {
        identity.push(' ');
        identity.push_str(&script.canonical_path.to_string_lossy().to_ascii_lowercase());
    }
    if identity.contains(&producer.to_ascii_lowercase()) {
        Ok(())
    } else {
        Err(AppError::Provider(format!(
            "版本 probe 与 producer={producer:?} 身份不匹配；请确认绑定了正确工具"
        )))
    }
}

fn pinned_artifact(path: &Path, label: &str) -> Result<ProviderArtifact, AppError> {
    if !path.is_absolute() {
        return Err(AppError::Provider(format!(
            "{label} 必须使用机器绝对路径：{}",
            path.display()
        )));
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|error| AppError::Provider(format!("{label} 无法解析：{error}")))?;
    let metadata = fs::metadata(&canonical_path)?;
    if !metadata.is_file() {
        return Err(AppError::Provider(format!(
            "{label} 不是普通文件：{}",
            canonical_path.display()
        )));
    }
    Ok(ProviderArtifact {
        sha256: hash_file(&canonical_path)?,
        canonical_path,
    })
}

pub(crate) fn pin_external_executable(
    project_root: &Path,
    path: &Path,
    label: &str,
) -> Result<PinnedExternalExecutable, AppError> {
    let artifact = pinned_artifact(path, label)?;
    reject_repository_artifact(project_root, &artifact, label)?;
    reject_command_script(&artifact.canonical_path)?;
    Ok(PinnedExternalExecutable {
        canonical_path: artifact.canonical_path,
        sha256: artifact.sha256,
    })
}

fn scip_python_package_manifest(script: &ProviderArtifact) -> Result<String, AppError> {
    let package_root = script
        .canonical_path
        .parent()
        .ok_or_else(|| AppError::Provider("scip-python launcher script 缺少父目录".to_owned()))?;
    let package_json_path = package_root.join("package.json");
    let package_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&package_json_path).map_err(|error| {
            AppError::Provider(format!(
                "scip-python launcher 必须位于官方包根目录，无法读取 {}：{error}",
                package_json_path.display()
            ))
        })?)?;
    if package_json.get("name").and_then(serde_json::Value::as_str)
        != Some("@sourcegraph/scip-python")
    {
        return Err(AppError::Provider(
            "scip-python launcher 所在 package.json 的 name 不匹配".to_owned(),
        ));
    }
    let bin_entry = package_json
        .get("bin")
        .and_then(|value| {
            value.as_str().or_else(|| {
                value
                    .as_object()
                    .and_then(|entries| entries.get("scip-python"))
                    .and_then(serde_json::Value::as_str)
            })
        })
        .ok_or_else(|| {
            AppError::Provider("scip-python package.json 缺少 scip-python bin 入口".to_owned())
        })?;
    let declared_script = package_root
        .join(bin_entry)
        .canonicalize()
        .map_err(|error| AppError::Provider(format!("scip-python bin 入口无法解析：{error}")))?;
    if declared_script != script.canonical_path {
        return Err(AppError::Provider(
            "--script 必须指向 scip-python package.json 声明的 bin 入口".to_owned(),
        ));
    }

    let mut pending = vec![package_root.to_owned()];
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(AppError::Provider(format!(
                    "scip-python 包内不允许符号链接：{}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                total_bytes = total_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| AppError::Provider("scip-python 包大小溢出".to_owned()))?;
                if total_bytes > MAX_LAUNCHER_PACKAGE_BYTES {
                    return Err(AppError::Provider(format!(
                        "scip-python 包超过 {MAX_LAUNCHER_PACKAGE_BYTES} 字节安全上限"
                    )));
                }
                let relative = path
                    .strip_prefix(package_root)
                    .map_err(|_| AppError::Provider("scip-python 包文件越过包根目录".to_owned()))?;
                files.push((
                    relative.to_string_lossy().replace('\\', "/"),
                    path,
                    metadata.len(),
                ));
                if files.len() > MAX_LAUNCHER_PACKAGE_FILES {
                    return Err(AppError::Provider(format!(
                        "scip-python 包超过 {MAX_LAUNCHER_PACKAGE_FILES} 个文件安全上限"
                    )));
                }
            } else {
                return Err(AppError::Provider(format!(
                    "scip-python 包包含不支持的文件类型：{}",
                    path.display()
                )));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    digest.update(b"project-brain/scip-python-package/v1\0");
    for (relative, path, size) in files {
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(size.to_le_bytes());
        digest.update(hash_file(&path)?.as_bytes());
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn reject_repository_artifact(
    project_root: &Path,
    artifact: &ProviderArtifact,
    label: &str,
) -> Result<(), AppError> {
    let project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_owned());
    if artifact.canonical_path.starts_with(&project_root) {
        return Err(AppError::Provider(format!(
            "{label} 位于受索引仓库内，拒绝建立执行信任：{}",
            artifact.canonical_path.display()
        )));
    }
    Ok(())
}

fn registration_id(
    project_key: &str,
    profile_id: &str,
    producer: &str,
    executable: &ProviderArtifact,
    launcher_script: Option<&ProviderArtifact>,
    launcher_package_manifest_sha256: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    let mut parts = vec![
        project_key,
        profile_id,
        producer,
        &executable.sha256,
        launcher_script.map_or("", |script| script.sha256.as_str()),
    ];
    if let Some(manifest) = launcher_package_manifest_sha256 {
        digest.update(b"project-brain/provider-registration/v2\0");
        parts.push(manifest);
    } else {
        digest.update(b"project-brain/provider-registration/v1\0");
    }
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("provider_{:x}", digest.finalize())
}

fn digest_path(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    format!("{digest:x}")[..16].to_owned()
}

fn binding_equivalent(left: &ProviderBinding, right: &ProviderBinding) -> bool {
    left.registration_id == right.registration_id
        && left.project_key == right.project_key
        && left.profile_id == right.profile_id
        && left.producer == right.producer
        && left.executable == right.executable
        && left.launcher_script == right.launcher_script
        && left.launcher_package_manifest_sha256 == right.launcher_package_manifest_sha256
        && left.resolved_version == right.resolved_version
}

fn reject_command_script(path: &Path) -> Result<(), AppError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        return Err(AppError::Provider(
            "拒绝直接执行 .cmd/.bat shell shim；请绑定原生 executable，并用 --script 提供已固定哈希的脚本入口"
                .to_owned(),
        ));
    }
    Ok(())
}

fn read_registry(install_root: &Path) -> Result<ProviderRegistry, AppError> {
    let path = install_root.join("state/providers.json");
    if !path.is_file() {
        return Ok(ProviderRegistry {
            schema_version: PROVIDER_SCHEMA_VERSION,
            bindings: Vec::new(),
        });
    }
    let registry: ProviderRegistry = serde_json::from_slice(&fs::read(&path)?)?;
    if registry.schema_version != PROVIDER_SCHEMA_VERSION {
        return Err(AppError::Provider(
            "Provider 注册表 schema 不兼容".to_owned(),
        ));
    }
    let mut keys = std::collections::BTreeSet::new();
    for binding in &registry.bindings {
        if !keys.insert((binding.project_key.clone(), binding.profile_id.clone())) {
            return Err(AppError::Provider(format!(
                "Provider 注册表存在重复绑定：{}/{}",
                binding.project_key, binding.profile_id
            )));
        }
    }
    Ok(registry)
}

fn sort_bindings(bindings: &mut [ProviderBinding]) {
    bindings.sort_by(|left, right| {
        left.project_key
            .cmp(&right.project_key)
            .then(left.profile_id.cmp(&right.profile_id))
    });
}

fn provider_status(
    project_key: &str,
    profile: &SemanticProviderProfile,
    registry: &ProviderRegistry,
    project_root: Option<&Path>,
) -> ProviderStatus {
    let Some(binding) = registry
        .bindings
        .iter()
        .find(|item| item.project_key == project_key && item.profile_id == profile.id)
    else {
        return ProviderStatus {
            profile_id: profile.id.clone(),
            producer: profile.producer.clone(),
            state: "missing",
            executable: None,
            launcher_script: None,
            resolved_version: None,
            issue: Some(format!("provider profile={} 尚未在本机绑定", profile.id)),
        };
    };
    let issue = validate_binding(binding, profile)
        .and_then(|()| {
            if let Some(root) = project_root {
                reject_repository_artifact(root, &binding.executable, "Provider executable")?;
                if let Some(script) = &binding.launcher_script {
                    reject_repository_artifact(root, script, "Provider launcher script")?;
                }
            }
            Ok(())
        })
        .err()
        .map(|error| error.to_string());
    ProviderStatus {
        profile_id: profile.id.clone(),
        producer: profile.producer.clone(),
        state: if issue.is_none() { "ready" } else { "drifted" },
        executable: Some(binding.executable.canonical_path.clone()),
        launcher_script: binding
            .launcher_script
            .as_ref()
            .map(|item| item.canonical_path.clone()),
        resolved_version: Some(binding.resolved_version.clone()),
        issue,
    }
}

fn validate_binding(
    binding: &ProviderBinding,
    profile: &SemanticProviderProfile,
) -> Result<(), AppError> {
    if binding.producer != profile.producer || binding.profile_id != profile.id {
        return Err(AppError::Provider(format!(
            "provider profile={} 的仓库契约与机器绑定不一致",
            profile.id
        )));
    }
    let expected_registration_id = registration_id(
        &binding.project_key,
        &binding.profile_id,
        &binding.producer,
        &binding.executable,
        binding.launcher_script.as_ref(),
        binding.launcher_package_manifest_sha256.as_deref(),
    );
    if binding.registration_id != expected_registration_id {
        return Err(AppError::Provider(format!(
            "provider profile={} 的 registration_id 与固定内容不一致；请显式重新绑定",
            profile.id
        )));
    }
    validate_artifact(&binding.executable, "Provider executable")?;
    if let Some(script) = &binding.launcher_script {
        validate_artifact(script, "Provider launcher script")?;
    }
    if profile.producer.eq_ignore_ascii_case("scip-python")
        && let Some(script) = binding.launcher_script.as_ref()
    {
        let expected = binding
            .launcher_package_manifest_sha256
            .as_ref()
            .ok_or_else(|| {
                AppError::Provider("scip-python 绑定缺少包清单哈希；请显式重新绑定".to_owned())
            })?;
        let actual = scip_python_package_manifest(script)?;
        if &actual != expected {
            return Err(AppError::Provider(
                "scip-python 包内容发生漂移；请显式重新绑定".to_owned(),
            ));
        }
    } else if binding.launcher_package_manifest_sha256.is_some() {
        return Err(AppError::Provider(format!(
            "provider profile={} 不应包含 launcher 包清单",
            profile.id
        )));
    }
    Ok(())
}

fn validate_artifact(artifact: &ProviderArtifact, label: &str) -> Result<(), AppError> {
    if !artifact.canonical_path.is_file() {
        return Err(AppError::Provider(format!(
            "{label} 已不存在：{}",
            artifact.canonical_path.display()
        )));
    }
    let actual = hash_file(&artifact.canonical_path)?;
    if actual != artifact.sha256 {
        return Err(AppError::Provider(format!(
            "{label} 内容发生漂移；请显式重新绑定：{}",
            artifact.canonical_path.display()
        )));
    }
    Ok(())
}

/// Production Qualification 专用的同路径内容固定探针。
///
/// 返回值与后续校验都复用正式 Provider binding 的 `pinned_artifact` / `validate_artifact`
/// 实现，避免资格测试复制一套更宽松的哈希逻辑。
pub(crate) fn qualification_pin_artifact(path: &Path) -> Result<String, AppError> {
    Ok(pinned_artifact(path, "qualification provider artifact")?.sha256)
}

pub(crate) fn qualification_validate_pinned_artifact(
    path: &Path,
    expected_sha256: &str,
) -> Result<(), AppError> {
    let canonical_path = path.canonicalize().map_err(|error| {
        AppError::Provider(format!("qualification provider artifact 无法解析：{error}"))
    })?;
    validate_artifact(
        &ProviderArtifact {
            canonical_path,
            sha256: expected_sha256.to_owned(),
        },
        "qualification provider artifact",
    )
}

fn provider_arguments(
    producer: &str,
    project_root: &Path,
    project_key: &str,
    output_path: &Path,
) -> Result<Vec<String>, AppError> {
    let root = provider_cli_path(project_root);
    let output = provider_cli_path(output_path);
    if producer.eq_ignore_ascii_case("rust-analyzer") {
        Ok(vec!["scip".to_owned(), root, "--output".to_owned(), output])
    } else if producer.eq_ignore_ascii_case("scip-dotnet") {
        Ok(vec![
            "index".to_owned(),
            "--working-directory".to_owned(),
            root,
            "--skip-dotnet-restore".to_owned(),
            "--output".to_owned(),
            output,
        ])
    } else if producer.eq_ignore_ascii_case("scip-python") {
        Ok(vec![
            "index".to_owned(),
            "--cwd".to_owned(),
            root,
            "--project-name".to_owned(),
            project_key.to_owned(),
            "--output".to_owned(),
            output,
        ])
    } else {
        Err(AppError::Provider(format!(
            "producer={producer:?} 没有安全 argv 契约"
        )))
    }
}

pub(crate) fn provider_cli_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        value.into_owned()
    }
}

fn provider_environment(
    repository_root: Option<&Path>,
) -> Result<Vec<(OsString, OsString)>, AppError> {
    const ALLOWED: [&str; 22] = [
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "HOME",
        "TEMP",
        "TMP",
        "TMPDIR",
        "PATHEXT",
        "DOTNET_ROOT",
        "NUGET_PACKAGES",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTC",
        "VIRTUAL_ENV",
        "LANG",
        "LC_ALL",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "CommonProgramFiles",
    ];
    let mut environment = vec![
        (OsString::from("NO_COLOR"), OsString::from("1")),
        (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
    ];
    for name in ALLOWED {
        if let Some(value) = std::env::var_os(name) {
            environment.push((OsString::from(name), value));
        }
    }
    if let Some(path) = sanitized_path(repository_root)? {
        environment.push((OsString::from("PATH"), path));
    }
    Ok(environment)
}

fn sanitized_path(repository_root: Option<&Path>) -> Result<Option<std::ffi::OsString>, AppError> {
    let Some(path) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    let repository_root = repository_root.and_then(|root| root.canonicalize().ok());
    let entries = std::env::split_paths(&path)
        .filter(|entry| !entry.as_os_str().is_empty() && entry != Path::new("."))
        .filter(|entry| {
            let Some(root) = &repository_root else {
                return true;
            };
            entry
                .canonicalize()
                .map_or(true, |canonical| !canonical.starts_with(root))
        })
        .collect::<Vec<_>>();
    std::env::join_paths(entries)
        .map(Some)
        .map_err(|error| AppError::Provider(format!("无法构造安全 Provider PATH：{error}")))
}

pub(crate) fn run_process(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    repository_root: Option<&Path>,
    timeout: Duration,
) -> Result<ProcessResult, AppError> {
    run_process_with_environment(
        executable,
        launcher_script,
        arguments,
        cwd,
        repository_root,
        timeout,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_process_with_environment(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    repository_root: Option<&Path>,
    timeout: Duration,
    environment: &[(&str, &Path)],
) -> Result<ProcessResult, AppError> {
    run_process_with_environment_inner(
        executable,
        launcher_script,
        arguments,
        cwd,
        repository_root,
        timeout,
        environment,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_process_with_environment_observing_timeout(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    repository_root: Option<&Path>,
    timeout: Duration,
    environment: &[(&str, &Path)],
) -> Result<ProcessResult, AppError> {
    run_process_with_environment_inner(
        executable,
        launcher_script,
        arguments,
        cwd,
        repository_root,
        timeout,
        environment,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_process_with_environment_inner(
    executable: &Path,
    launcher_script: Option<&Path>,
    arguments: &[String],
    cwd: &Path,
    repository_root: Option<&Path>,
    timeout: Duration,
    environment: &[(&str, &Path)],
    observe_timeout: bool,
) -> Result<ProcessResult, AppError> {
    let mut owned_environment = provider_environment(repository_root)?;
    for (name, value) in environment {
        owned_environment.push((OsString::from(name), value.as_os_str().to_os_string()));
    }
    crate::execution::run_contained(
        executable,
        launcher_script,
        arguments,
        cwd,
        timeout,
        &owned_environment,
        observe_timeout,
    )
}

#[cfg(test)]
fn drain_bounded(mut stream: impl std::io::Read) -> Result<CapturedOutput, std::io::Error> {
    let mut captured = Vec::new();
    let mut digest = Sha256::new();
    let mut total_bytes = 0_usize;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(CapturedOutput {
                truncated: total_bytes > captured.len(),
                bytes: captured,
                total_bytes,
                sha256: format!("{:x}", digest.finalize()),
            });
        }
        total_bytes = total_bytes.saturating_add(read);
        digest.update(&chunk[..read]);
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
        captured.extend_from_slice(&chunk[..read.min(remaining)]);
    }
}

pub(crate) fn version_text(process: &ProcessResult) -> Result<String, AppError> {
    let text = if process
        .stdout
        .bytes
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
    {
        &process.stdout.bytes
    } else {
        &process.stderr.bytes
    };
    let text = String::from_utf8_lossy(text);
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(AppError::Provider(
            "Provider --version 没有返回可记录的版本文本".to_owned(),
        ));
    }
    Ok(normalized.chars().take(512).collect())
}

fn validate_output(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AppError::Provider(format!("Provider 未生成 index.scip：{error}")))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(AppError::Provider(
            "Provider 输出必须是隔离目录中的普通文件，不能是链接".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_SCIP_BYTES {
        return Err(AppError::Provider(format!(
            "Provider 输出大小非法：{} bytes（允许 1..={MAX_SCIP_BYTES}）",
            metadata.len()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "与 Unix 权限收紧函数保持统一跨平台调用契约"
)]
fn secure_directory(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "与 Unix 权限收紧函数保持统一跨平台调用契约"
)]
fn secure_file(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

pub(crate) fn hash_file(path: &Path) -> Result<String, AppError> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        digest.update(&chunk[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn audit_for_process<'a>(
    project_key: &'a str,
    profile_id: &'a str,
    stage: &'a str,
    outcome: &'a str,
    process: &'a ProcessResult,
    failure_kind: Option<&'a str>,
) -> Result<ProviderAuditRecord<'a>, AppError> {
    Ok(ProviderAuditRecord {
        schema_version: PROVIDER_SCHEMA_VERSION,
        timestamp_unix_ms: unix_ms()?,
        project_key,
        profile_id,
        stage,
        outcome,
        duration_ms: Some(process.duration.as_millis()),
        exit_code: process.status.code(),
        stdout_bytes: Some(process.stdout.total_bytes),
        stderr_bytes: Some(process.stderr.total_bytes),
        stdout_truncated: Some(process.stdout.truncated),
        stderr_truncated: Some(process.stderr.truncated),
        stdout_sha256: Some(process.stdout.sha256.clone()),
        stderr_sha256: Some(process.stderr.sha256.clone()),
        registration_id: None,
        registration_revision: None,
        executable_sha256: None,
        artifact_sha256: None,
        source_fingerprint_before: None,
        source_fingerprint_after: None,
        failure_kind,
    })
}

fn append_audit(install_root: &Path, record: &ProviderAuditRecord<'_>) -> Result<(), AppError> {
    let _lock = MutationLock::acquire(&install_root.join("state/provider-audit.lock"))?;
    let path = install_root.join("state/provider-audit.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > MAX_AUDIT_BYTES)
    {
        let bytes = fs::read(&path)?;
        let start = bytes.len().saturating_sub(AUDIT_RETAIN_BYTES);
        let start = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(start, |offset| start + offset + 1);
        let mut retained = bytes[start..].to_vec();
        retained.extend_from_slice(&line);
        let before_hash = target_hash(&path)?;
        atomic_replace(&path, &retained, Some(&before_hash))?;
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&line)?;
    file.sync_data()?;
    Ok(())
}

fn unix_ms() -> Result<u128, AppError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use brain_core::{SemanticLanguageMapping, SemanticProviderFormat, SemanticProviderProfile};
    use sha2::Digest as _;

    use super::{
        MAX_CAPTURE_BYTES, PROVIDER_SCHEMA_VERSION, ProviderArtifact, ProviderBinding,
        ProviderRegistry, drain_bounded, pinned_artifact, provider_arguments, provider_cli_path,
        registration_id, reject_command_script, reject_repository_artifact,
        scip_python_package_manifest, trust_status, validate_binding, verify_probe_identity,
    };

    fn profile(producer: &str) -> SemanticProviderProfile {
        SemanticProviderProfile {
            id: "main".to_owned(),
            format: SemanticProviderFormat::Scip,
            producer: producer.to_owned(),
            contract_version: 1,
            language_mappings: vec![SemanticLanguageMapping {
                raw_language: None,
                language: "python".to_owned(),
                allow_missing_language: true,
            }],
        }
    }

    #[test]
    fn provider_argv_is_fixed_by_known_producer() {
        let root = Path::new("repo with spaces");
        let output = Path::new("run/index.scip");
        assert_eq!(
            provider_arguments("rust-analyzer", root, "p", output).unwrap(),
            ["scip", "repo with spaces", "--output", "run/index.scip"]
        );
        assert_eq!(
            provider_arguments("scip-dotnet", root, "p", output).unwrap(),
            [
                "index",
                "--working-directory",
                "repo with spaces",
                "--skip-dotnet-restore",
                "--output",
                "run/index.scip"
            ]
        );
        assert_eq!(
            provider_arguments("scip-python", root, "project-key", output).unwrap(),
            [
                "index",
                "--cwd",
                "repo with spaces",
                "--project-name",
                "project-key",
                "--output",
                "run/index.scip"
            ]
        );
        assert!(provider_arguments("custom", root, "p", output).is_err());
    }

    #[test]
    fn provider_cli_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            provider_cli_path(Path::new(r"\\?\C:\repo with spaces\index.scip")),
            r"C:\repo with spaces\index.scip"
        );
        assert_eq!(
            provider_cli_path(Path::new(r"\\?\UNC\server\share\index.scip")),
            r"\\server\share\index.scip"
        );
        assert_eq!(
            provider_cli_path(Path::new("repo with spaces/index.scip")),
            "repo with spaces/index.scip"
        );
    }

    #[test]
    fn command_script_shims_are_rejected_on_every_host_platform() {
        assert!(reject_command_script(Path::new("provider.cmd")).is_err());
        assert!(reject_command_script(Path::new("provider.BAT")).is_err());
        assert!(reject_command_script(Path::new("provider.exe")).is_ok());
        assert!(reject_command_script(Path::new("provider")).is_ok());
    }

    #[test]
    fn executable_drift_is_rejected() {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-provider-drift-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("provider.bin");
        fs::write(&executable, b"one").unwrap();
        let mut binding = ProviderBinding {
            registration_id: String::new(),
            revision: 1,
            project_key: "project".to_owned(),
            profile_id: "main".to_owned(),
            producer: "rust-analyzer".to_owned(),
            executable: ProviderArtifact {
                canonical_path: executable.clone(),
                sha256: super::hash_file(&executable).unwrap(),
            },
            launcher_script: None,
            launcher_package_manifest_sha256: None,
            resolved_version: "1".to_owned(),
        };
        binding.registration_id = registration_id(
            &binding.project_key,
            &binding.profile_id,
            &binding.producer,
            &binding.executable,
            binding.launcher_script.as_ref(),
            binding.launcher_package_manifest_sha256.as_deref(),
        );
        validate_binding(&binding, &profile("rust-analyzer")).unwrap();
        fs::write(&executable, b"two").unwrap();
        assert!(validate_binding(&binding, &profile("rust-analyzer")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hook_trust_status_requires_current_registration_and_executable_hash() {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-provider-hook-trust-{}-{nonce}",
            std::process::id()
        ));
        let project = root.join("project");
        let install = root.join("install");
        let tools = root.join("tools");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(install.join("state")).unwrap();
        fs::create_dir_all(&tools).unwrap();
        let executable = tools.join("scip-python-provider.bin");
        fs::write(&executable, b"trusted").unwrap();
        let mut binding = ProviderBinding {
            registration_id: String::new(),
            revision: 1,
            project_key: "project".to_owned(),
            profile_id: "main".to_owned(),
            producer: "rust-analyzer".to_owned(),
            executable: ProviderArtifact {
                canonical_path: executable.canonicalize().unwrap(),
                sha256: super::hash_file(&executable).unwrap(),
            },
            launcher_script: None,
            launcher_package_manifest_sha256: None,
            resolved_version: "1".to_owned(),
        };
        binding.registration_id = registration_id(
            &binding.project_key,
            &binding.profile_id,
            &binding.producer,
            &binding.executable,
            binding.launcher_script.as_ref(),
            binding.launcher_package_manifest_sha256.as_deref(),
        );
        let expected_registration_id = binding.registration_id.clone();
        let registry = ProviderRegistry {
            schema_version: PROVIDER_SCHEMA_VERSION,
            bindings: vec![binding],
        };
        fs::write(
            install.join("state/providers.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();

        let ready = trust_status(
            Some(&install),
            &project,
            "project",
            &[profile("rust-analyzer")],
        );
        assert!(ready["main"].ready);
        assert_eq!(
            ready["main"].registration_id.as_deref(),
            Some(expected_registration_id.as_str())
        );

        fs::write(&executable, b"drifted").unwrap();
        let drifted = trust_status(
            Some(&install),
            &project,
            "project",
            &[profile("rust-analyzer")],
        );
        assert!(!drifted["main"].ready);
        assert!(drifted["main"].issue.as_deref().unwrap().contains("漂移"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scip_python_package_manifest_covers_transitive_bundle_files() {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-scip-python-package-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::write(
            root.join("package.json"),
            br#"{"name":"@sourcegraph/scip-python","bin":{"scip-python":"index.js"}}"#,
        )
        .unwrap();
        fs::write(root.join("index.js"), b"require('./dist/scip-python')").unwrap();
        fs::write(root.join("dist/scip-python.js"), b"one").unwrap();
        let script = pinned_artifact(&root.join("index.js"), "script").unwrap();
        let before = scip_python_package_manifest(&script).unwrap();
        fs::write(root.join("dist/scip-python.js"), b"two").unwrap();
        let after = scip_python_package_manifest(&script).unwrap();
        assert_ne!(before, after);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_executables_and_relative_paths_are_rejected() {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-provider-repo-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("provider.bin");
        fs::write(&executable, b"provider").unwrap();
        let artifact = pinned_artifact(&executable, "test").unwrap();
        assert!(reject_repository_artifact(&root, &artifact, "test").is_err());
        assert!(pinned_artifact(Path::new("relative-provider"), "test").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn captured_output_is_bounded_but_hashes_the_full_stream() {
        let input = vec![b'x'; MAX_CAPTURE_BYTES + 17];
        let expected = format!("{:x}", super::Sha256::digest(&input));
        let captured = drain_bounded(std::io::Cursor::new(input)).unwrap();
        assert_eq!(captured.bytes.len(), MAX_CAPTURE_BYTES);
        assert_eq!(captured.total_bytes, MAX_CAPTURE_BYTES + 17);
        assert!(captured.truncated);
        assert_eq!(captured.sha256, expected);
    }

    #[test]
    fn version_probe_must_identify_the_configured_family() {
        let artifact = ProviderArtifact {
            canonical_path: PathBuf::from("/tools/not-the-provider"),
            sha256: "hash".to_owned(),
        };
        assert!(
            verify_probe_identity("rust-analyzer", &artifact, None, "rust-analyzer 1.92").is_ok()
        );
        assert!(verify_probe_identity("scip-dotnet", &artifact, None, "1.0").is_err());
    }
}
