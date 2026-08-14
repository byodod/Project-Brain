use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brain_evidence::{
    ArtifactNode, EvidenceAuthority, EvidenceFinding, EvidenceInputManifestV1, EvidencePlane,
    EvidenceProvider, EvidenceSnapshot, FindingAuthority, InputPathState, content_fingerprint,
};
use brain_provider_protocol::{
    PROVIDER_PROCESS_PROTOCOL_VERSION, PROVIDER_RUN_REQUEST_SCHEMA_VERSION, ProviderDescriptorV1,
    ProviderRunRequestV1, ProviderRunResponseV1, ProviderRunStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    error::AppError,
    evidence_inputs, execution, provider,
    setup::{
        MutationLock, atomic_replace, ensure_install_ready, pretty_json_bytes,
        resolve_install_root, target_hash,
    },
};

const REGISTRY_SCHEMA_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ExecutableIdentity {
    path: PathBuf,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EvidenceProviderBinding {
    project_key: String,
    profile_id: String,
    descriptor: ProviderDescriptorV1,
    descriptor_hash: String,
    executable: ExecutableIdentity,
    authority_ceiling: EvidenceAuthority,
    registration_id: String,
    revision: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EvidenceProviderRegistry {
    schema_version: u32,
    bindings: Vec<EvidenceProviderBinding>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BindReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    descriptor: ProviderDescriptorV1,
    descriptor_hash: String,
    executable: PathBuf,
    executable_sha256: String,
    authority_ceiling: EvidenceAuthority,
    registration_id: String,
    revision: u64,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct UnbindReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListReport {
    schema_version: u32,
    project_key: String,
    bindings: Vec<BindingStatus>,
}

#[derive(Debug, Serialize)]
struct BindingStatus {
    profile_id: String,
    provider_id: String,
    provider_version: String,
    capabilities: Vec<EvidencePlane>,
    authority_ceiling: EvidenceAuthority,
    executable: PathBuf,
    state: &'static str,
    issue: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RunReport {
    schema_version: u32,
    project_key: String,
    profile_id: String,
    provider_id: String,
    registration_id: String,
    registration_revision: u64,
    descriptor_hash: String,
    executable_sha256: String,
    duration_ms: u128,
    stdout_sha256: String,
    stderr_sha256: String,
    pub(crate) evidence: EvidenceSnapshot,
    input_manifest: EvidenceInputManifestV1,
}

impl RunReport {
    pub(crate) fn input_manifest(&self) -> &EvidenceInputManifestV1 {
        &self.input_manifest
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "绑定安全边界需要显式携带项目、身份、权限、替换与信任参数"
)]
pub(crate) fn bind(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profile_id: &str,
    executable: &Path,
    authority_ceiling: EvidenceAuthority,
    replace: bool,
    trust_local_executable: bool,
    timeout_seconds: u64,
) -> Result<BindReport, AppError> {
    validate_profile_id(profile_id)?;
    if !trust_local_executable {
        return Err(AppError::Provider(
            "绑定 Evidence Provider 需要显式传入 --trust-local-executable".to_owned(),
        ));
    }
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let pinned = provider::pin_external_executable(
        &project_root.canonicalize()?,
        executable,
        "Evidence Provider executable",
    )?;
    let process = provider::run_process(
        &pinned.canonical_path,
        None,
        &["describe".to_owned()],
        &install_root,
        Some(project_root),
        Duration::from_secs(timeout_seconds),
    )?;
    require_clean_success(&process, "describe")?;
    let descriptor: ProviderDescriptorV1 = serde_json::from_slice(&process.stdout.bytes)?;
    descriptor
        .validate()
        .map_err(|error| protocol_error(&error))?;
    verify_executable(&pinned.canonical_path, &pinned.sha256)?;
    let descriptor_hash = content_fingerprint(&serde_json::to_vec(&descriptor)?);
    let executable = ExecutableIdentity {
        path: pinned.canonical_path,
        sha256: pinned.sha256,
    };
    let registration_id = registration_id(
        project_key,
        profile_id,
        &descriptor_hash,
        &executable.sha256,
        authority_ceiling,
    );
    let mut binding = EvidenceProviderBinding {
        project_key: project_key.to_owned(),
        profile_id: profile_id.to_owned(),
        descriptor: descriptor.clone(),
        descriptor_hash: descriptor_hash.clone(),
        executable,
        authority_ceiling,
        registration_id,
        revision: 1,
    };
    let _lock = MutationLock::acquire(&install_root.join("state/evidence-providers.lock"))?;
    let path = registry_path(&install_root);
    let before_hash = target_hash(&path)?;
    let mut registry = read_registry(&install_root)?;
    let existing = registry
        .bindings
        .iter()
        .position(|item| item.project_key == project_key && item.profile_id == profile_id);
    let changed = match existing {
        Some(index) if equivalent(&registry.bindings[index], &binding) => {
            binding.revision = registry.bindings[index].revision;
            false
        }
        Some(_) if !replace => {
            return Err(AppError::Provider(format!(
                "Evidence Provider profile={profile_id:?} 已绑定且内容不同；请显式传入 --replace"
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
        registry.bindings.sort_by(|left, right| {
            (&left.project_key, &left.profile_id).cmp(&(&right.project_key, &right.profile_id))
        });
        atomic_replace(&path, &pretty_json_bytes(&registry)?, Some(&before_hash))?;
    }
    Ok(BindReport {
        schema_version: REGISTRY_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile_id.to_owned(),
        descriptor,
        descriptor_hash,
        executable: binding.executable.path,
        executable_sha256: binding.executable.sha256,
        authority_ceiling,
        registration_id: binding.registration_id,
        revision: binding.revision,
        changed,
    })
}

pub(crate) fn unbind(
    explicit_install_root: Option<&Path>,
    project_key: &str,
    profile_id: &str,
) -> Result<UnbindReport, AppError> {
    validate_profile_id(profile_id)?;
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let _lock = MutationLock::acquire(&install_root.join("state/evidence-providers.lock"))?;
    let path = registry_path(&install_root);
    let before_hash = target_hash(&path)?;
    let mut registry = read_registry(&install_root)?;
    let before = registry.bindings.len();
    registry
        .bindings
        .retain(|item| item.project_key != project_key || item.profile_id != profile_id);
    let changed = registry.bindings.len() != before;
    if changed {
        atomic_replace(&path, &pretty_json_bytes(&registry)?, Some(&before_hash))?;
    }
    Ok(UnbindReport {
        schema_version: REGISTRY_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile_id.to_owned(),
        changed,
    })
}

pub(crate) fn list(
    explicit_install_root: Option<&Path>,
    project_key: &str,
) -> Result<ListReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let registry = read_registry(&install_root)?;
    let bindings = registry
        .bindings
        .into_iter()
        .filter(|item| item.project_key == project_key)
        .map(|binding| {
            let issue = validate_binding(&binding)
                .err()
                .map(|error| error.to_string());
            BindingStatus {
                profile_id: binding.profile_id,
                provider_id: binding.descriptor.provider_id,
                provider_version: binding.descriptor.provider_version,
                capabilities: binding.descriptor.capabilities,
                authority_ceiling: binding.authority_ceiling,
                executable: binding.executable.path,
                state: if issue.is_none() { "ready" } else { "drifted" },
                issue,
            }
        })
        .collect();
    Ok(ListReport {
        schema_version: REGISTRY_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        bindings,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn run(
    explicit_install_root: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    profile_id: &str,
    plane: EvidencePlane,
    contract_path: &Path,
    config_path: Option<&Path>,
    timeout_seconds: u64,
) -> Result<RunReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let registry = read_registry(&install_root)?;
    let binding = registry
        .bindings
        .into_iter()
        .find(|item| item.project_key == project_key && item.profile_id == profile_id)
        .ok_or_else(|| {
            AppError::Provider(format!(
                "Evidence Provider profile={profile_id:?} 尚未在本机绑定"
            ))
        })?;
    validate_binding(&binding)?;
    if !binding.descriptor.capabilities.contains(&plane) {
        return Err(AppError::Provider(format!(
            "Provider {} 未声明 plane={} 能力",
            binding.descriptor.provider_id,
            plane.as_str()
        )));
    }
    let contract_bytes = read_bounded_file(contract_path, MAX_CONFIG_BYTES, "input contract")?;
    let contract: brain_evidence::InputDependencyContractV1 =
        serde_json::from_slice(&contract_bytes)?;
    contract.validate()?;
    if contract.project_key != project_key
        || contract.profile_id != profile_id
        || contract.provider_contract_id != binding.descriptor.provider_id
        || contract.provider_contract_version != binding.descriptor.provider_contract_version
    {
        return Err(AppError::Provider(
            "Input dependency contract 与绑定的 project/profile/provider 不一致".to_owned(),
        ));
    }
    let root = project_root.canonicalize()?;
    let input_manifest = evidence_inputs::resolve_stable(&root, &contract)?;
    let opaque_config = config_path.map_or_else(
        || Ok(Value::Object(serde_json::Map::new())),
        |path| {
            let bytes = read_bounded_file(path, MAX_CONFIG_BYTES, "provider config")?;
            Ok::<Value, AppError>(serde_json::from_slice(&bytes)?)
        },
    )?;
    let scratch = ProviderScratch::create(&install_root, project_key)?;
    stage_inputs(&root, &scratch.project, &input_manifest)?;
    let timeout_ms = timeout_seconds.saturating_mul(1_000);
    let request_id = content_fingerprint(
        format!(
            "{}\0{}\0{}\0{}",
            project_key, profile_id, input_manifest.manifest_hash, scratch.nonce
        )
        .as_bytes(),
    );
    let opaque_config_hash = content_fingerprint(&serde_json::to_vec(&opaque_config)?);
    let request = ProviderRunRequestV1 {
        schema_version: PROVIDER_RUN_REQUEST_SCHEMA_VERSION,
        protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
        request_id,
        provider_id: binding.descriptor.provider_id.clone(),
        profile_id: profile_id.to_owned(),
        project_key: project_key.to_owned(),
        plane,
        source_fingerprint: input_manifest.source_fingerprint_at_creation.clone(),
        input_manifest: input_manifest.clone(),
        staged_project_root: transport_path(&scratch.project),
        output_root: transport_path(&scratch.output),
        opaque_config,
        opaque_config_hash,
        timeout_ms,
    };
    request.validate().map_err(|error| protocol_error(&error))?;
    let mut request_bytes = serde_json::to_vec(&request)?;
    request_bytes.push(b'\n');
    let environment = provider::provider_environment(Some(&root))?;
    let process = execution::run_contained_with_input(
        &binding.executable.path,
        None,
        &["run".to_owned()],
        &scratch.directory,
        Duration::from_secs(timeout_seconds),
        &environment,
        false,
        Some(&request_bytes),
    )?;
    require_clean_success(&process, "run")?;
    let response: ProviderRunResponseV1 = serde_json::from_slice(&process.stdout.bytes)?;
    response
        .validate_against(&request, &binding.descriptor)
        .map_err(|error| protocol_error(&error))?;
    if response.status != ProviderRunStatus::Succeeded {
        return Err(AppError::Provider(format!(
            "Evidence Provider 返回失败：{}: {}",
            response.error_code.as_deref().unwrap_or("provider_failed"),
            response.error_message.as_deref().unwrap_or("no message")
        )));
    }
    verify_executable(&binding.executable.path, &binding.executable.sha256)?;
    let current_source = crate::git::worktree_fingerprint(&root)?;
    if current_source != request.source_fingerprint {
        return Err(AppError::Provider(
            "Evidence Provider 运行期间权威 Source 发生变化；结果已丢弃".to_owned(),
        ));
    }
    let candidate = response
        .candidate
        .ok_or_else(|| AppError::Provider("成功响应缺少 candidate".to_owned()))?;
    let contract_version = u16::try_from(candidate.provider_contract_version)
        .map_err(|_| AppError::Provider("provider contract version 超出 Evidence v1".to_owned()))?;
    let mut artifacts = candidate.artifacts;
    let payload_bytes = serde_json::to_vec(&candidate.payload)?;
    artifacts.push(ArtifactNode::from_provider_key(
        project_key,
        &binding.descriptor.provider_id,
        "provider_payload",
        &format!("payload:{}", candidate.payload_schema),
        &candidate.payload_schema,
        None,
        &payload_bytes,
    ));
    let findings = candidate
        .findings
        .into_iter()
        .map(|finding| EvidenceFinding {
            code: finding.code,
            severity: finding.severity,
            authority: if binding.authority_ceiling == EvidenceAuthority::Deterministic
                && finding.deterministic_violation_claim
            {
                FindingAuthority::DeterministicViolation
            } else {
                FindingAuthority::Advisory
            },
            message: finding.message,
            artifact_id: finding.artifact_id,
            path: finding.path,
        })
        .collect();
    let evidence = EvidenceSnapshot::new(
        project_key,
        plane,
        EvidenceProvider {
            id: binding.descriptor.provider_id.clone(),
            version: binding.descriptor.provider_version.clone(),
            contract_version,
            authority: binding.authority_ceiling,
        },
        &request.source_fingerprint,
        candidate.coverage,
        candidate.upstream,
        artifacts,
        candidate.edges,
        findings,
    )?;
    Ok(RunReport {
        schema_version: REGISTRY_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        profile_id: profile_id.to_owned(),
        provider_id: binding.descriptor.provider_id,
        registration_id: binding.registration_id,
        registration_revision: binding.revision,
        descriptor_hash: binding.descriptor_hash,
        executable_sha256: binding.executable.sha256,
        duration_ms: process.duration.as_millis(),
        stdout_sha256: process.stdout.sha256,
        stderr_sha256: process.stderr.sha256,
        evidence,
        input_manifest,
    })
}

fn read_registry(install_root: &Path) -> Result<EvidenceProviderRegistry, AppError> {
    let path = registry_path(install_root);
    if !path.is_file() {
        return Ok(EvidenceProviderRegistry {
            schema_version: REGISTRY_SCHEMA_VERSION,
            bindings: Vec::new(),
        });
    }
    let registry: EvidenceProviderRegistry = serde_json::from_slice(&fs::read(path)?)?;
    if registry.schema_version != REGISTRY_SCHEMA_VERSION {
        return Err(AppError::Provider(
            "Evidence Provider registry schema 不兼容".to_owned(),
        ));
    }
    let mut keys = std::collections::BTreeSet::new();
    for binding in &registry.bindings {
        if !keys.insert((&binding.project_key, &binding.profile_id)) {
            return Err(AppError::Provider(
                "Evidence Provider registry 存在重复 project/profile".to_owned(),
            ));
        }
    }
    Ok(registry)
}

fn validate_binding(binding: &EvidenceProviderBinding) -> Result<(), AppError> {
    binding
        .descriptor
        .validate()
        .map_err(|error| protocol_error(&error))?;
    let descriptor_hash = content_fingerprint(&serde_json::to_vec(&binding.descriptor)?);
    if descriptor_hash != binding.descriptor_hash
        || registration_id(
            &binding.project_key,
            &binding.profile_id,
            &binding.descriptor_hash,
            &binding.executable.sha256,
            binding.authority_ceiling,
        ) != binding.registration_id
    {
        return Err(AppError::Provider(
            "Evidence Provider binding identity 已损坏".to_owned(),
        ));
    }
    verify_executable(&binding.executable.path, &binding.executable.sha256)
}

fn equivalent(left: &EvidenceProviderBinding, right: &EvidenceProviderBinding) -> bool {
    left.registration_id == right.registration_id
        && left.project_key == right.project_key
        && left.profile_id == right.profile_id
        && left.descriptor == right.descriptor
        && left.executable == right.executable
        && left.authority_ceiling == right.authority_ceiling
}

fn registration_id(
    project_key: &str,
    profile_id: &str,
    descriptor_hash: &str,
    executable_hash: &str,
    authority: EvidenceAuthority,
) -> String {
    let mut digest = Sha256::new();
    for part in [
        "project-brain/evidence-provider-registration/v1",
        project_key,
        profile_id,
        descriptor_hash,
        executable_hash,
        authority.as_str(),
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("evidence_provider_{:x}", digest.finalize())
}

fn stage_inputs(
    project_root: &Path,
    staged_root: &Path,
    manifest: &EvidenceInputManifestV1,
) -> Result<(), AppError> {
    fs::create_dir_all(staged_root)?;
    for entry in &manifest.entries {
        if entry.state == InputPathState::Absent {
            continue;
        }
        let relative = Path::new(&entry.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AppError::Provider("Provider staging path 非法".to_owned()));
        }
        let source = project_root.join(relative);
        let canonical = source.canonicalize()?;
        let metadata = fs::symlink_metadata(&canonical)?;
        if !canonical.starts_with(project_root)
            || metadata.file_type().is_symlink()
            || !metadata.is_file()
            || evidence_content_hash(&canonical)? != entry.content_sha256.as_deref().unwrap_or("")
            || metadata.len() != entry.size.unwrap_or(u64::MAX)
        {
            return Err(AppError::Provider(format!(
                "Provider staging input 与 manifest 不一致：{}",
                entry.path
            )));
        }
        let target = staged_root.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&canonical, &target)?;
        if evidence_content_hash(&target)? != entry.content_sha256.as_deref().unwrap_or("") {
            return Err(AppError::Provider(
                "Provider staging copy 校验失败".to_owned(),
            ));
        }
    }
    Ok(())
}

fn evidence_content_hash(path: &Path) -> Result<String, AppError> {
    Ok(format!("sha256_{}", provider::hash_file(path)?))
}

fn require_clean_success(
    process: &execution::ProcessResult,
    operation: &str,
) -> Result<(), AppError> {
    if !process.status.success()
        || process.timed_out
        || process.stdout.truncated
        || process.stderr.truncated
        || process.stdout.bytes.is_empty()
    {
        return Err(AppError::Provider(format!(
            "Evidence Provider {operation} 未完整成功：exit_code={:?}",
            process.status.code()
        )));
    }
    Ok(())
}

fn verify_executable(path: &Path, expected: &str) -> Result<(), AppError> {
    if provider::hash_file(path)? != expected {
        return Err(AppError::Provider(
            "Evidence Provider executable SHA-256 已漂移".to_owned(),
        ));
    }
    Ok(())
}

fn validate_profile_id(profile_id: &str) -> Result<(), AppError> {
    if profile_id.is_empty()
        || profile_id.len() > 128
        || !profile_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(AppError::Provider(
            "Evidence Provider profile ID 格式非法".to_owned(),
        ));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, max: u64, label: &str) -> Result<Vec<u8>, AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max {
        return Err(AppError::Provider(format!("{label} 必须是有界普通文件")));
    }
    Ok(fs::read(path)?)
}

fn registry_path(install_root: &Path) -> PathBuf {
    install_root.join("state/evidence-providers.json")
}

fn transport_path(path: &Path) -> String {
    provider::provider_cli_path(path)
}

fn protocol_error(error: &brain_provider_protocol::ProviderProtocolError) -> AppError {
    AppError::Provider(error.to_string())
}

struct ProviderScratch {
    directory: PathBuf,
    project: PathBuf,
    output: PathBuf,
    nonce: u128,
}

impl ProviderScratch {
    fn create(install_root: &Path, project_key: &str) -> Result<Self, AppError> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let key = content_fingerprint(project_key.as_bytes());
        let root = install_root.join("state/evidence-provider-runs");
        fs::create_dir_all(&root)?;
        let directory = root.join(format!("{}-{}-{nonce}", &key[..16], std::process::id()));
        fs::create_dir(&directory)?;
        let project = directory.join("project");
        let output = directory.join("output");
        fs::create_dir(&output)?;
        Ok(Self {
            directory,
            project,
            output,
            nonce,
        })
    }
}

impl Drop for ProviderScratch {
    fn drop(&mut self) {
        if let Some(root) = self.directory.parent()
            && self.directory.starts_with(root)
            && self.directory != root
        {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}
