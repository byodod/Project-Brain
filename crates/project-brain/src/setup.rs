use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use brain_core::SemanticProviderProfile;

use crate::{error::AppError, provider};

const INSTALL_SCHEMA_VERSION: u32 = 1;
const DOCTOR_SCHEMA_VERSION: u32 = 2;
const REGISTRY_SCHEMA_VERSION: u32 = 1;
const CODEX_INTEGRATION_VERSION: u32 = 1;
const CLAUDE_INTEGRATION_VERSION: u32 = 1;
const PRIME_INTEGRATION_VERSION: u32 = 1;
const INSTALL_ROOT_ENV: &str = "PROJECT_BRAIN_INSTALL_ROOT";
const LAUNCHED_ENV: &str = "PROJECT_BRAIN_LAUNCHED";
const REQUIRED_CODEX_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];
const REQUIRED_CLAUDE_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];
const REQUIRED_PRIME_EVENTS: [&str; 5] = [
    "session_start",
    "input",
    "before_agent_start",
    "tool_call",
    "tool_result",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InstallManifest {
    schema_version: u32,
    current: String,
    previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProjectRegistration {
    canonical_root: PathBuf,
    project_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ProjectRegistry {
    schema_version: u32,
    projects: Vec<ProjectRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CodexIntegrationManifest {
    schema_version: u32,
    integration_version: u32,
    target_path: PathBuf,
    managed_handler_hashes: BTreeMap<String, String>,
    before_hash: String,
    after_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ClaudeIntegrationManifest {
    schema_version: u32,
    integration_version: u32,
    target_path: PathBuf,
    managed_handler_hashes: BTreeMap<String, String>,
    before_hash: String,
    after_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrimeIntegrationManifest {
    schema_version: u32,
    integration_version: u32,
    api_contract: String,
    target_path: PathBuf,
    target_sha256: String,
    launcher_path: PathBuf,
    launcher_sha256: String,
    managed_events: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallReport {
    pub schema_version: u32,
    pub install_root: PathBuf,
    pub stable_launcher: PathBuf,
    pub payload: PathBuf,
    pub current_version: String,
    pub changed: bool,
}

#[derive(Debug, Serialize)]
pub struct RollbackReport {
    pub schema_version: u32,
    pub install_root: PathBuf,
    pub current_version: String,
    pub previous_version: String,
    pub stable_launcher_unchanged: bool,
}

#[derive(Debug, Serialize)]
pub struct BootstrapReport {
    pub schema_version: u32,
    pub project_key: String,
    pub canonical_root: PathBuf,
    pub registered: bool,
    pub codex_hooks_installed: bool,
}

#[derive(Debug, Serialize)]
pub struct HookInstallReport {
    pub schema_version: u32,
    pub target_path: PathBuf,
    pub changed: bool,
    pub managed_handler_count: usize,
    pub trust_state: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub install_root: PathBuf,
    pub launcher: CheckState,
    pub payload: CheckState,
    pub project_registration: CheckState,
    pub providers: CheckState,
    pub adapter: &'static str,
    pub adapter_hooks: CheckState,
    pub adapter_trust_state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_coverage: Option<crate::scip_index::SemanticCoverageDoctorReport>,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum DoctorAdapter {
    Codex,
    ClaudeCode,
    PrimeAgent,
}

impl DoctorAdapter {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::PrimeAgent => "prime_agent",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::PrimeAgent => "Prime Agent",
        }
    }
}

const fn adapter_trust_state(adapter: DoctorAdapter, valid: bool) -> &'static str {
    if matches!(adapter, DoctorAdapter::PrimeAgent) && valid {
        "extension_contract_and_launcher_verified"
    } else {
        "not_programmatically_verifiable"
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass,
    Fail,
}

impl From<bool> for CheckState {
    fn from(value: bool) -> Self {
        if value { Self::Pass } else { Self::Fail }
    }
}

impl DoctorReport {
    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

pub fn delegate_if_installed_launcher() -> Result<Option<ExitCode>, AppError> {
    if env::var_os(LAUNCHED_ENV).is_some() {
        return Ok(None);
    }
    let executable = env::current_exe()?;
    let Some(bin_dir) = executable.parent() else {
        return Ok(None);
    };
    if bin_dir.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return Ok(None);
    }
    let Some(root) = bin_dir.parent() else {
        return Ok(None);
    };
    let manifest_path = root.join("state/install.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest: InstallManifest = read_json(&manifest_path)?;
    validate_install_manifest(&manifest)?;
    let payload = version_payload(root, &manifest.current, &executable)?;
    if !payload.is_file() {
        return Err(AppError::Setup(format!(
            "当前安装 payload 不存在：{}",
            payload.display()
        )));
    }

    let status = Command::new(payload)
        .args(env::args_os().skip(1))
        .env(LAUNCHED_ENV, "1")
        .env(INSTALL_ROOT_ENV, root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    let code = status.code().unwrap_or(1).clamp(0, 255);
    let code = u8::try_from(code).unwrap_or(1);
    Ok(Some(ExitCode::from(code)))
}

pub fn install(explicit_root: Option<&Path>) -> Result<InstallReport, AppError> {
    let root = resolve_install_root(explicit_root)?;
    let _lock = MutationLock::acquire(&root.join("state/install.lock"))?;
    let executable = env::current_exe()?;
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let payload = version_payload(&root, &version, &executable)?;
    let stable_launcher = stable_launcher_path(&root, &executable)?;
    fs::create_dir_all(payload.parent().expect("payload has parent"))?;
    fs::create_dir_all(stable_launcher.parent().expect("launcher has parent"))?;
    fs::create_dir_all(root.join("state/integrations"))?;

    let mut changed = install_versioned_payload(&executable, &payload, &version)?;
    self_check(&payload, &root)?;
    if !stable_launcher.is_file() {
        copy_file_atomically(&executable, &stable_launcher)?;
        changed = true;
    }

    let manifest_path = root.join("state/install.json");
    let manifest_before_hash = target_hash(&manifest_path)?;
    let previous = if manifest_path.is_file() {
        let existing: InstallManifest = read_json(&manifest_path)?;
        validate_install_manifest(&existing)?;
        if existing.current == version {
            existing.previous
        } else {
            Some(existing.current)
        }
    } else {
        changed = true;
        None
    };
    let manifest = InstallManifest {
        schema_version: INSTALL_SCHEMA_VERSION,
        current: version.clone(),
        previous,
    };
    let manifest_bytes = pretty_json_bytes(&manifest)?;
    if read_optional(&manifest_path)? != manifest_bytes {
        atomic_replace(&manifest_path, &manifest_bytes, Some(&manifest_before_hash))?;
        changed = true;
    }

    Ok(InstallReport {
        schema_version: INSTALL_SCHEMA_VERSION,
        install_root: root,
        stable_launcher,
        payload,
        current_version: version,
        changed,
    })
}

pub fn rollback(explicit_root: Option<&Path>) -> Result<RollbackReport, AppError> {
    let root = resolve_install_root(explicit_root)?;
    let _lock = MutationLock::acquire(&root.join("state/install.lock"))?;
    let manifest_path = root.join("state/install.json");
    let manifest_before_hash = target_hash(&manifest_path)?;
    let mut manifest = ensure_install_ready(&root)?;
    let previous = manifest
        .previous
        .take()
        .ok_or_else(|| AppError::Setup("没有可回滚的上一版本".to_owned()))?;
    let executable = env::current_exe()?;
    let candidate = version_payload(&root, &previous, &executable)?;
    if !candidate.is_file() {
        return Err(AppError::Setup(format!(
            "上一版本 payload 不存在：{}",
            candidate.display()
        )));
    }
    self_check(&candidate, &root)?;
    let current = std::mem::replace(&mut manifest.current, previous.clone());
    manifest.previous = Some(current.clone());
    atomic_replace(
        &manifest_path,
        &pretty_json_bytes(&manifest)?,
        Some(&manifest_before_hash),
    )?;
    Ok(RollbackReport {
        schema_version: INSTALL_SCHEMA_VERSION,
        install_root: root,
        current_version: previous,
        previous_version: current,
        stable_launcher_unchanged: true,
    })
}

pub fn bootstrap(
    explicit_install_root: Option<&Path>,
    explicit_codex_home: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    provider_profiles: &[SemanticProviderProfile],
    install_codex: bool,
) -> Result<BootstrapReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    ensure_install_ready(&install_root)?;
    let canonical_root = project_root.canonicalize()?;
    let registered = register_project(&install_root, &canonical_root, project_key)?;
    let hook_report = if install_codex {
        match install_codex_hooks(Some(&install_root), explicit_codex_home) {
            Ok(report) => Some(report),
            Err(error) => {
                if registered {
                    let _ = unregister_project(&install_root, &canonical_root, project_key);
                }
                return Err(error);
            }
        }
    } else {
        None
    };
    if install_codex {
        let report = doctor(
            Some(&install_root),
            DoctorAdapter::Codex,
            explicit_codex_home,
            &canonical_root,
            project_key,
            provider_profiles,
        );
        if report.status != "ready" {
            if hook_report.as_ref().is_some_and(|report| report.changed) {
                let _ = uninstall_codex_hooks(Some(&install_root), explicit_codex_home, false);
            }
            if registered {
                let _ = unregister_project(&install_root, &canonical_root, project_key);
            }
            return Err(AppError::Setup(format!(
                "bootstrap doctor 未通过：{}",
                report.issues.join("；")
            )));
        }
    }
    Ok(BootstrapReport {
        schema_version: REGISTRY_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        canonical_root,
        registered,
        codex_hooks_installed: hook_report.is_some(),
    })
}

pub fn registered_project_for_cwd(
    explicit_install_root: Option<&Path>,
    cwd: &Path,
) -> Result<Option<(PathBuf, String)>, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let registry = read_registry(&install_root)?;
    let Ok(canonical_cwd) = cwd.canonicalize() else {
        return Ok(None);
    };
    let selected = registry
        .projects
        .into_iter()
        .filter(|entry| canonical_cwd.starts_with(&entry.canonical_root))
        .max_by_key(|entry| entry.canonical_root.components().count());
    Ok(selected.map(|entry| (entry.canonical_root, entry.project_key)))
}

pub fn install_codex_hooks(
    explicit_install_root: Option<&Path>,
    explicit_codex_home: Option<&Path>,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let install = ensure_install_ready(&install_root)?;
    let launcher = stable_launcher_path(&install_root, &env::current_exe()?)?;
    if !launcher.is_file() {
        return Err(AppError::Setup(format!(
            "稳定 launcher 不存在：{}；请重新执行 project-brain install",
            launcher.display()
        )));
    }
    let codex_home = resolve_codex_home(explicit_codex_home)?;
    let target = codex_home.join("hooks.json");
    let integration_path = install_root.join("state/integrations/codex.json");
    fs::create_dir_all(&codex_home)?;
    fs::create_dir_all(integration_path.parent().expect("integration has parent"))?;
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/codex.lock"))?;

    let original = if target.is_file() {
        fs::read(&target)?
    } else {
        b"{}\n".to_vec()
    };
    let mut document: Value = serde_json::from_slice(&original)?;
    require_object(&document, "Codex hooks.json 顶层必须是 JSON object")?;
    let expected_handlers = managed_handlers(&launcher);

    if integration_path.is_file() {
        let manifest: CodexIntegrationManifest = read_json(&integration_path)?;
        validate_integration_manifest(&manifest, &target)?;
        let observed = observed_managed_hashes(&document);
        if observed == manifest.managed_handler_hashes
            && manifest.managed_handler_hashes == handler_hashes(&expected_handlers)?
        {
            return Ok(HookInstallReport {
                schema_version: CODEX_INTEGRATION_VERSION,
                target_path: target,
                changed: false,
                managed_handler_count: expected_handlers.len(),
                trust_state: "not_programmatically_verifiable",
            });
        }
        return Err(AppError::IntegrationDrift(target));
    }
    if !observed_managed_hashes(&document).is_empty() {
        return Err(AppError::IntegrationDrift(target));
    }

    append_managed_groups(&mut document, &expected_handlers)?;
    let updated = pretty_json_bytes(&document)?;
    let before_hash = digest_bytes(&original);
    let after_hash = digest_bytes(&updated);
    atomic_replace(&target, &updated, Some(&before_hash))?;
    let manifest = CodexIntegrationManifest {
        schema_version: INSTALL_SCHEMA_VERSION,
        integration_version: CODEX_INTEGRATION_VERSION,
        target_path: target.clone(),
        managed_handler_hashes: handler_hashes(&expected_handlers)?,
        before_hash,
        after_hash,
    };
    if let Err(error) = atomic_replace(&integration_path, &pretty_json_bytes(&manifest)?, None) {
        let _ = atomic_replace(&target, &original, Some(&manifest.after_hash));
        return Err(error);
    }
    let _ = install;

    Ok(HookInstallReport {
        schema_version: CODEX_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: expected_handlers.len(),
        trust_state: "not_programmatically_verifiable",
    })
}

pub fn uninstall_codex_hooks(
    explicit_install_root: Option<&Path>,
    explicit_codex_home: Option<&Path>,
    force: bool,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let codex_home = resolve_codex_home(explicit_codex_home)?;
    let target = codex_home.join("hooks.json");
    let integration_path = install_root.join("state/integrations/codex.json");
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/codex.lock"))?;
    if !target.is_file() || !integration_path.is_file() {
        return Ok(HookInstallReport {
            schema_version: CODEX_INTEGRATION_VERSION,
            target_path: target,
            changed: false,
            managed_handler_count: 0,
            trust_state: "not_programmatically_verifiable",
        });
    }

    let original = fs::read(&target)?;
    let mut document: Value = serde_json::from_slice(&original)?;
    let manifest: CodexIntegrationManifest = read_json(&integration_path)?;
    validate_integration_manifest(&manifest, &target)?;
    let observed = observed_managed_hashes(&document);
    if !force && observed != manifest.managed_handler_hashes {
        return Err(AppError::IntegrationDrift(target));
    }
    let removed = remove_managed_handlers(
        &mut document,
        &manifest.managed_handler_hashes.values().cloned().collect(),
        force,
    );
    if removed == 0 && !force {
        return Err(AppError::IntegrationDrift(target));
    }
    let updated = pretty_json_bytes(&document)?;
    atomic_replace(&target, &updated, Some(&digest_bytes(&original)))?;
    fs::remove_file(&integration_path)?;

    Ok(HookInstallReport {
        schema_version: CODEX_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: 0,
        trust_state: "not_programmatically_verifiable",
    })
}

pub fn install_claude_hooks(
    explicit_install_root: Option<&Path>,
    explicit_claude_home: Option<&Path>,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let _install = ensure_install_ready(&install_root)?;
    let launcher = stable_launcher_path(&install_root, &env::current_exe()?)?;
    if !launcher.is_file() {
        return Err(AppError::Setup(format!(
            "稳定 launcher 不存在：{}；请重新执行 project-brain install",
            launcher.display()
        )));
    }
    let claude_home = resolve_claude_home(explicit_claude_home)?;
    let target = claude_home.join("settings.json");
    let integration_path = install_root.join("state/integrations/claude-code.json");
    fs::create_dir_all(&claude_home)?;
    fs::create_dir_all(integration_path.parent().expect("integration has parent"))?;
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/claude-code.lock"))?;

    let original = if target.is_file() {
        fs::read(&target)?
    } else {
        b"{}\n".to_vec()
    };
    let mut document: Value = serde_json::from_slice(&original)?;
    require_object(&document, "Claude settings.json 顶层必须是 JSON object")?;
    let expected_handlers = managed_claude_handlers(&launcher);

    if integration_path.is_file() {
        let manifest: ClaudeIntegrationManifest = read_json(&integration_path)?;
        validate_claude_integration_manifest(&manifest, &target)?;
        let observed = observed_claude_managed_hashes(&document);
        if observed == manifest.managed_handler_hashes
            && manifest.managed_handler_hashes == handler_hashes(&expected_handlers)?
        {
            return Ok(HookInstallReport {
                schema_version: CLAUDE_INTEGRATION_VERSION,
                target_path: target,
                changed: false,
                managed_handler_count: expected_handlers.len(),
                trust_state: "not_programmatically_verifiable",
            });
        }
        return Err(AppError::IntegrationDrift(target));
    }
    if !observed_claude_managed_hashes(&document).is_empty() {
        return Err(AppError::IntegrationDrift(target));
    }

    append_claude_managed_groups(&mut document, &expected_handlers)?;
    let updated = pretty_json_bytes(&document)?;
    let before_hash = digest_bytes(&original);
    let after_hash = digest_bytes(&updated);
    atomic_replace(&target, &updated, Some(&before_hash))?;
    let manifest = ClaudeIntegrationManifest {
        schema_version: INSTALL_SCHEMA_VERSION,
        integration_version: CLAUDE_INTEGRATION_VERSION,
        target_path: target.clone(),
        managed_handler_hashes: handler_hashes(&expected_handlers)?,
        before_hash,
        after_hash,
    };
    if let Err(error) = atomic_replace(&integration_path, &pretty_json_bytes(&manifest)?, None) {
        let _ = atomic_replace(&target, &original, Some(&manifest.after_hash));
        return Err(error);
    }

    Ok(HookInstallReport {
        schema_version: CLAUDE_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: expected_handlers.len(),
        trust_state: "not_programmatically_verifiable",
    })
}

pub fn uninstall_claude_hooks(
    explicit_install_root: Option<&Path>,
    explicit_claude_home: Option<&Path>,
    force: bool,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let claude_home = resolve_claude_home(explicit_claude_home)?;
    let target = claude_home.join("settings.json");
    let integration_path = install_root.join("state/integrations/claude-code.json");
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/claude-code.lock"))?;
    if !target.is_file() || !integration_path.is_file() {
        return Ok(HookInstallReport {
            schema_version: CLAUDE_INTEGRATION_VERSION,
            target_path: target,
            changed: false,
            managed_handler_count: 0,
            trust_state: "not_programmatically_verifiable",
        });
    }

    let original = fs::read(&target)?;
    let mut document: Value = serde_json::from_slice(&original)?;
    let manifest: ClaudeIntegrationManifest = read_json(&integration_path)?;
    validate_claude_integration_manifest(&manifest, &target)?;
    let observed = observed_claude_managed_hashes(&document);
    if !force && observed != manifest.managed_handler_hashes {
        return Err(AppError::IntegrationDrift(target));
    }
    let removed = remove_claude_managed_handlers(
        &mut document,
        &manifest.managed_handler_hashes.values().cloned().collect(),
        force,
    );
    if removed == 0 && !force {
        return Err(AppError::IntegrationDrift(target));
    }
    let updated = pretty_json_bytes(&document)?;
    atomic_replace(&target, &updated, Some(&digest_bytes(&original)))?;
    fs::remove_file(&integration_path)?;

    Ok(HookInstallReport {
        schema_version: CLAUDE_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: 0,
        trust_state: "not_programmatically_verifiable",
    })
}

pub fn install_prime_extension(
    explicit_install_root: Option<&Path>,
    explicit_prime_home: Option<&Path>,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let _install = ensure_install_ready(&install_root)?;
    let launcher = stable_launcher_path(&install_root, &env::current_exe()?)?;
    if !launcher.is_file() {
        return Err(AppError::Setup(format!(
            "稳定 launcher 不存在：{}；请重新执行 project-brain install",
            launcher.display()
        )));
    }

    let prime_home = resolve_prime_home(explicit_prime_home)?;
    let extension_root = prime_home.join("extensions");
    let extension_directory = extension_root.join("project-brain");
    let target = extension_directory.join("index.ts");
    let integration_path = install_root.join("state/integrations/prime-agent.json");
    let expected = render_prime_extension(&launcher)?;
    let expected_hash = digest_bytes(&expected);
    let launcher_hash = digest_bytes(&fs::read(&launcher)?);
    if !prime_launcher_fixture_valid(&launcher) {
        return Err(AppError::Setup(format!(
            "Prime Agent stable launcher capability roundtrip 失败：{}",
            launcher.display()
        )));
    }
    let expected_manifest = PrimeIntegrationManifest {
        schema_version: INSTALL_SCHEMA_VERSION,
        integration_version: PRIME_INTEGRATION_VERSION,
        api_contract: "prime-agent-extension-v1".to_owned(),
        target_path: target.clone(),
        target_sha256: expected_hash.clone(),
        launcher_path: launcher.clone(),
        launcher_sha256: launcher_hash,
        managed_events: REQUIRED_PRIME_EVENTS
            .iter()
            .map(ToString::to_string)
            .collect(),
    };

    fs::create_dir_all(&extension_root)?;
    fs::create_dir_all(integration_path.parent().expect("integration has parent"))?;
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/prime-agent.lock"))?;

    if integration_path.is_file() {
        let manifest_bytes = fs::read(&integration_path)?;
        let manifest: PrimeIntegrationManifest = serde_json::from_slice(&manifest_bytes)?;
        validate_prime_integration_manifest(&manifest, &target)?;
        if !prime_extension_directory_exact(&extension_directory, &target)
            || digest_bytes(&fs::read(&target)?) != manifest.target_sha256
            || manifest.target_sha256 != expected_hash
        {
            return Err(AppError::IntegrationDrift(target));
        }
        if manifest == expected_manifest {
            return Ok(HookInstallReport {
                schema_version: PRIME_INTEGRATION_VERSION,
                target_path: target,
                changed: false,
                managed_handler_count: REQUIRED_PRIME_EVENTS.len(),
                trust_state: "extension_contract_and_launcher_verified",
            });
        }
        atomic_replace(
            &integration_path,
            &pretty_json_bytes(&expected_manifest)?,
            Some(&digest_bytes(&manifest_bytes)),
        )?;
        return Ok(HookInstallReport {
            schema_version: PRIME_INTEGRATION_VERSION,
            target_path: target,
            changed: true,
            managed_handler_count: REQUIRED_PRIME_EVENTS.len(),
            trust_state: "extension_contract_and_launcher_verified",
        });
    }

    if extension_directory.exists() {
        return Err(AppError::IntegrationDrift(target));
    }
    fs::create_dir(&extension_directory)?;
    if let Err(error) = atomic_replace(&target, &expected, None) {
        let _ = fs::remove_dir(&extension_directory);
        return Err(error);
    }
    if let Err(error) = atomic_replace(
        &integration_path,
        &pretty_json_bytes(&expected_manifest)?,
        None,
    ) {
        if target_hash_exact(&target).as_deref() == Some(expected_hash.as_str()) {
            let _ = fs::remove_file(&target);
            let _ = fs::remove_dir(&extension_directory);
        }
        return Err(error);
    }

    Ok(HookInstallReport {
        schema_version: PRIME_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: REQUIRED_PRIME_EVENTS.len(),
        trust_state: "extension_contract_and_launcher_verified",
    })
}

pub fn uninstall_prime_extension(
    explicit_install_root: Option<&Path>,
    explicit_prime_home: Option<&Path>,
    force: bool,
) -> Result<HookInstallReport, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let prime_home = resolve_prime_home(explicit_prime_home)?;
    let extension_directory = prime_home.join("extensions/project-brain");
    let target = extension_directory.join("index.ts");
    let integration_path = install_root.join("state/integrations/prime-agent.json");
    let _lock = MutationLock::acquire(&install_root.join("state/integrations/prime-agent.lock"))?;
    let target_exists = fs::symlink_metadata(&target).is_ok();
    let manifest_exists = integration_path.is_file();

    if !target_exists && !manifest_exists {
        return Ok(HookInstallReport {
            schema_version: PRIME_INTEGRATION_VERSION,
            target_path: target,
            changed: false,
            managed_handler_count: 0,
            trust_state: "extension_contract_and_launcher_verified",
        });
    }
    if target_exists && !manifest_exists {
        return Err(AppError::IntegrationDrift(target));
    }
    if !force && !target_exists {
        return Err(AppError::IntegrationDrift(target));
    }

    if manifest_exists {
        let manifest: PrimeIntegrationManifest = read_json(&integration_path)?;
        validate_prime_integration_manifest(&manifest, &target)?;
        if !force
            && (!prime_extension_directory_exact(&extension_directory, &target)
                || target_hash_exact(&target).as_deref() != Some(manifest.target_sha256.as_str()))
        {
            return Err(AppError::IntegrationDrift(target));
        }
    }

    if target_exists {
        fs::remove_file(&target)?;
    }
    if manifest_exists && let Err(error) = fs::remove_file(&integration_path) {
        return Err(error.into());
    }
    let _ = fs::remove_dir(&extension_directory);

    Ok(HookInstallReport {
        schema_version: PRIME_INTEGRATION_VERSION,
        target_path: target,
        changed: true,
        managed_handler_count: 0,
        trust_state: "extension_contract_and_launcher_verified",
    })
}

pub fn doctor(
    explicit_install_root: Option<&Path>,
    adapter: DoctorAdapter,
    explicit_agent_home: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    provider_profiles: &[SemanticProviderProfile],
) -> DoctorReport {
    let mut issues = Vec::new();
    let install_root = match resolve_install_root(explicit_install_root) {
        Ok(root) => root,
        Err(error) => {
            return DoctorReport {
                schema_version: DOCTOR_SCHEMA_VERSION,
                status: "broken",
                install_root: PathBuf::new(),
                launcher: CheckState::Fail,
                payload: CheckState::Fail,
                project_registration: CheckState::Fail,
                providers: CheckState::Fail,
                adapter: adapter.name(),
                adapter_hooks: CheckState::Fail,
                adapter_trust_state: "not_programmatically_verifiable",
                semantic_coverage: None,
                issues: vec![error.to_string()],
                warnings: Vec::new(),
            };
        }
    };
    let executable = env::current_exe().unwrap_or_default();
    let launcher_exists =
        stable_launcher_path(&install_root, &executable).is_ok_and(|path| path.is_file());
    if !launcher_exists {
        issues.push("稳定 launcher 不存在".to_owned());
    }
    let payload_exists = ensure_install_ready(&install_root)
        .map(|manifest| {
            version_payload(&install_root, &manifest.current, &executable)
                .is_ok_and(|path| path.is_file())
        })
        .unwrap_or(false);
    if !payload_exists {
        issues.push("当前版本 payload 不存在或安装清单无效".to_owned());
    }
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_owned());
    let project_registered = read_registry(&install_root).is_ok_and(|registry| {
        registry
            .projects
            .iter()
            .any(|entry| entry.canonical_root == canonical_root && entry.project_key == project_key)
    });
    if !project_registered {
        issues.push("当前项目未在本机注册或 project_key 不匹配".to_owned());
    }
    let providers = provider::doctor(
        Some(&install_root),
        &canonical_root,
        project_key,
        provider_profiles,
    );
    if !providers.ready {
        issues.extend(providers.issues);
    }
    let adapter_hooks_valid = match adapter {
        DoctorAdapter::Codex => codex_integration_valid(
            &install_root,
            resolve_codex_home(explicit_agent_home).ok().as_deref(),
        ),
        DoctorAdapter::ClaudeCode => claude_integration_valid(
            &install_root,
            resolve_claude_home(explicit_agent_home).ok().as_deref(),
        ),
        DoctorAdapter::PrimeAgent => prime_integration_valid(
            &install_root,
            resolve_prime_home(explicit_agent_home).ok().as_deref(),
            &canonical_root,
        ),
    };
    if !adapter_hooks_valid {
        issues.push(format!(
            "{} 用户级 Hook 缺失、重复或发生漂移",
            adapter.display_name()
        ));
    }
    DoctorReport {
        schema_version: DOCTOR_SCHEMA_VERSION,
        status: if issues.is_empty() {
            "ready"
        } else {
            "degraded"
        },
        install_root,
        launcher: launcher_exists.into(),
        payload: payload_exists.into(),
        project_registration: project_registered.into(),
        providers: providers.ready.into(),
        adapter: adapter.name(),
        adapter_hooks: adapter_hooks_valid.into(),
        adapter_trust_state: adapter_trust_state(adapter, adapter_hooks_valid),
        semantic_coverage: None,
        issues,
        warnings: Vec::new(),
    }
}

pub(crate) fn ensure_install_ready(root: &Path) -> Result<InstallManifest, AppError> {
    let path = root.join("state/install.json");
    if !path.is_file() {
        return Err(AppError::Setup(format!(
            "机器级安装不存在：{}；请先执行 project-brain install",
            root.display()
        )));
    }
    let manifest: InstallManifest = read_json(&path)?;
    validate_install_manifest(&manifest)?;
    let executable = env::current_exe()?;
    let launcher = stable_launcher_path(root, &executable)?;
    let payload = version_payload(root, &manifest.current, &executable)?;
    if !launcher.is_file() || !payload.is_file() {
        return Err(AppError::Setup(format!(
            "机器级安装不完整：launcher={} payload={}",
            launcher.display(),
            payload.display()
        )));
    }
    Ok(manifest)
}

fn validate_install_manifest(manifest: &InstallManifest) -> Result<(), AppError> {
    if manifest.schema_version != INSTALL_SCHEMA_VERSION || manifest.current.trim().is_empty() {
        return Err(AppError::Setup("机器级安装清单无效或版本不兼容".to_owned()));
    }
    Ok(())
}

fn validate_integration_manifest(
    manifest: &CodexIntegrationManifest,
    target: &Path,
) -> Result<(), AppError> {
    if manifest.schema_version != INSTALL_SCHEMA_VERSION
        || manifest.integration_version != CODEX_INTEGRATION_VERSION
        || manifest.target_path != target
        || manifest.managed_handler_hashes.len() != REQUIRED_CODEX_EVENTS.len()
    {
        return Err(AppError::IntegrationDrift(target.to_owned()));
    }
    Ok(())
}

fn validate_claude_integration_manifest(
    manifest: &ClaudeIntegrationManifest,
    target: &Path,
) -> Result<(), AppError> {
    if manifest.schema_version != INSTALL_SCHEMA_VERSION
        || manifest.integration_version != CLAUDE_INTEGRATION_VERSION
        || manifest.target_path != target
        || manifest.managed_handler_hashes.len() != REQUIRED_CLAUDE_EVENTS.len()
    {
        return Err(AppError::IntegrationDrift(target.to_owned()));
    }
    Ok(())
}

fn validate_prime_integration_manifest(
    manifest: &PrimeIntegrationManifest,
    target: &Path,
) -> Result<(), AppError> {
    let expected_events = REQUIRED_PRIME_EVENTS
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if manifest.schema_version != INSTALL_SCHEMA_VERSION
        || manifest.integration_version != PRIME_INTEGRATION_VERSION
        || manifest.api_contract != "prime-agent-extension-v1"
        || manifest.target_path != target
        || manifest.target_sha256.len() != 64
        || manifest.launcher_sha256.len() != 64
        || manifest.managed_events != expected_events
    {
        return Err(AppError::IntegrationDrift(target.to_owned()));
    }
    Ok(())
}

pub(crate) fn resolve_install_root(explicit: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }
    if let Some(path) = env::var_os(INSTALL_ROOT_ENV) {
        return absolute_path(Path::new(&path));
    }
    #[cfg(target_os = "windows")]
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local).join("ProjectBrain"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = user_home() {
        return Ok(home.join("Library/Application Support/ProjectBrain"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(data) = env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(data).join("project-brain"));
        }
        if let Some(home) = user_home() {
            return Ok(home.join(".local/share/project-brain"));
        }
    }
    Err(AppError::Setup(
        "无法确定机器级安装目录；请传入 --install-root".to_owned(),
    ))
}

/// 解析一个预期为目录的边界路径，即使末尾目录尚未创建，也先解析最近的现有祖先。
///
/// 这避免把 macOS `/var` 与 `/private/var`、Windows 大小写/短名称，以及现有目录
/// 中的符号链接当成不同边界。返回值可直接作为后续写入根，确保检查与实际写入使用
/// 同一条规范路径。
pub(crate) fn canonical_directory_boundary(path: &Path) -> Result<PathBuf, AppError> {
    let normalized = normalize_absolute_path(path)?;
    let mut cursor = normalized.as_path();
    let mut missing = Vec::<OsString>::new();

    loop {
        match fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut resolved = cursor.canonicalize()?;
                if !resolved.is_dir() {
                    return Err(AppError::Setup(format!(
                        "机器级目录边界的现有祖先不是目录：{}",
                        cursor.display()
                    )));
                }
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = cursor.file_name().ok_or_else(|| {
                    AppError::Setup(format!(
                        "找不到机器级目录边界的现有祖先：{}",
                        normalized.display()
                    ))
                })?;
                missing.push(component.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    AppError::Setup(format!(
                        "找不到机器级目录边界的现有祖先：{}",
                        normalized.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = absolute_path(path)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    return Err(AppError::Setup(format!(
                        "机器级目录边界试图越过文件系统根：{}",
                        path.display()
                    )));
                }
                normalized.pop();
            }
        }
    }
    Ok(normalized)
}

fn resolve_codex_home(explicit: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }
    if let Some(path) = env::var_os("CODEX_HOME") {
        return absolute_path(Path::new(&path));
    }
    user_home()
        .map(|home| home.join(".codex"))
        .ok_or_else(|| AppError::Setup("无法确定 Codex home；请传入 --codex-home".to_owned()))
}

fn resolve_claude_home(explicit: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }
    if let Some(path) = env::var_os("CLAUDE_CONFIG_DIR") {
        return absolute_path(Path::new(&path));
    }
    user_home()
        .map(|home| home.join(".claude"))
        .ok_or_else(|| AppError::Setup("无法确定 Claude home；请传入 --claude-home".to_owned()))
}

fn resolve_prime_home(explicit: Option<&Path>) -> Result<PathBuf, AppError> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }
    if let Some(path) = env::var_os("PRIME_AGENT_CODING_AGENT_DIR") {
        return absolute_path(Path::new(&path));
    }
    user_home()
        .map(|home| home.join(".prime/agent"))
        .ok_or_else(|| AppError::Setup("无法确定 Prime Agent home；请传入 --prime-home".to_owned()))
}

fn user_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn absolute_path(path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    Ok(env::current_dir()?.join(path))
}

fn version_payload(root: &Path, version: &str, executable: &Path) -> Result<PathBuf, AppError> {
    let file_name = executable
        .file_name()
        .ok_or_else(|| AppError::Setup("当前可执行文件没有文件名".to_owned()))?;
    Ok(root.join("versions").join(version).join(file_name))
}

fn stable_launcher_path(root: &Path, executable: &Path) -> Result<PathBuf, AppError> {
    let file_name = executable
        .file_name()
        .ok_or_else(|| AppError::Setup("当前可执行文件没有文件名".to_owned()))?;
    Ok(root.join("bin").join(file_name))
}

fn self_check(payload: &Path, root: &Path) -> Result<(), AppError> {
    let status = Command::new(payload)
        .args(["capabilities", "codex"])
        .env(LAUNCHED_ENV, "1")
        .env(INSTALL_ROOT_ENV, root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(AppError::Setup(format!(
            "候选 payload 自检失败：{}",
            payload.display()
        )));
    }
    Ok(())
}

fn register_project(root: &Path, project_root: &Path, project_key: &str) -> Result<bool, AppError> {
    let _lock = MutationLock::acquire(&root.join("state/projects.lock"))?;
    let registry_path = root.join("state/projects.json");
    let registry_before_hash = target_hash(&registry_path)?;
    let mut registry = read_registry(root)?;
    if let Some(existing) = registry
        .projects
        .iter()
        .find(|entry| entry.canonical_root == project_root)
    {
        if existing.project_key == project_key {
            return Ok(false);
        }
        return Err(AppError::Setup(format!(
            "项目根已注册为另一个 project_key：{}",
            project_root.display()
        )));
    }
    registry.projects.push(ProjectRegistration {
        canonical_root: project_root.to_owned(),
        project_key: project_key.to_owned(),
    });
    registry.projects.sort_by(|left, right| {
        left.canonical_root
            .cmp(&right.canonical_root)
            .then(left.project_key.cmp(&right.project_key))
    });
    atomic_replace(
        &registry_path,
        &pretty_json_bytes(&registry)?,
        Some(&registry_before_hash),
    )?;
    Ok(true)
}

fn unregister_project(root: &Path, project_root: &Path, project_key: &str) -> Result<(), AppError> {
    let _lock = MutationLock::acquire(&root.join("state/projects.lock"))?;
    let registry_path = root.join("state/projects.json");
    let registry_before_hash = target_hash(&registry_path)?;
    let mut registry = read_registry(root)?;
    registry
        .projects
        .retain(|entry| entry.canonical_root != project_root || entry.project_key != project_key);
    atomic_replace(
        &registry_path,
        &pretty_json_bytes(&registry)?,
        Some(&registry_before_hash),
    )
}

fn read_registry(root: &Path) -> Result<ProjectRegistry, AppError> {
    let path = root.join("state/projects.json");
    if !path.is_file() {
        return Ok(ProjectRegistry {
            schema_version: REGISTRY_SCHEMA_VERSION,
            projects: Vec::new(),
        });
    }
    let registry: ProjectRegistry = read_json(&path)?;
    if registry.schema_version != REGISTRY_SCHEMA_VERSION {
        return Err(AppError::Setup("项目注册表 schema 不兼容".to_owned()));
    }
    Ok(registry)
}

fn managed_handlers(launcher: &Path) -> BTreeMap<String, Value> {
    REQUIRED_CODEX_EVENTS
        .into_iter()
        .map(|event| {
            let event_arg = event_arg(event);
            let unix = format!(
                "{} dispatch codex {event_arg}",
                quote_posix(&launcher.to_string_lossy())
            );
            let windows = format!(
                "& {} dispatch codex {event_arg}",
                quote_powershell(&launcher.to_string_lossy())
            );
            let mut handler = json!({
                "type": "command",
                "command": unix,
                "commandWindows": windows,
                "timeout": 10,
                "statusMessage": format!("Project Brain: {event}")
            });
            if matches!(event, "SessionStart" | "UserPromptSubmit") {
                handler
                    .as_object_mut()
                    .expect("handler object")
                    .insert("additionalContextLimit".to_owned(), json!(8000));
            }
            (event.to_owned(), handler)
        })
        .collect()
}

fn managed_claude_handlers(launcher: &Path) -> BTreeMap<String, Value> {
    REQUIRED_CLAUDE_EVENTS
        .into_iter()
        .map(|event| {
            let event_arg = event_arg(event);
            (
                event.to_owned(),
                json!({
                    "type": "command",
                    "command": launcher,
                    "args": ["dispatch", "claude-code", event_arg],
                    "timeout": 10,
                    "statusMessage": "Project Brain deterministic governance"
                }),
            )
        })
        .collect()
}

fn render_prime_extension(launcher: &Path) -> Result<Vec<u8>, AppError> {
    let launcher_json = serde_json::to_string(&launcher.to_string_lossy().into_owned())?;
    let source = PRIME_EXTENSION_TEMPLATE.replace("__PROJECT_BRAIN_LAUNCHER__", &launcher_json);
    Ok(source.into_bytes())
}

const PRIME_EXTENSION_TEMPLATE: &str = r#"// Managed by Project Brain. Manual edits are treated as integration drift.
import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";

const LAUNCHER = __PROJECT_BRAIN_LAUNCHER__;
const MAX_BYTES = 1024 * 1024;
const TIMEOUT_MS = 10000;
const instanceId = randomUUID();
let pendingContext = [];

function sessionId(ctx) {
  return ctx.sessionManager.getSessionFile() ?? `prime-ephemeral-${instanceId}`;
}

function textItems(value, key) {
  if (!value || !Array.isArray(value[key])) return [];
  return value[key].filter((item) => typeof item === "string" && item.length > 0);
}

function invokeBrain(eventName, payload, cwd) {
  return new Promise((resolve, reject) => {
    const request = Buffer.from(JSON.stringify(payload), "utf8");
    if (request.length > MAX_BYTES) {
      reject(new Error("Project Brain request exceeds 1 MiB"));
      return;
    }

    const child = spawn(LAUNCHER, ["dispatch", "prime-agent", eventName], {
      cwd,
      shell: false,
      windowsHide: true,
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout = [];
    const stderr = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let settled = false;
    const timer = setTimeout(() => {
      child.kill();
      fail(new Error("Project Brain launcher timed out"));
    }, TIMEOUT_MS);

    function fail(error) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(error);
    }

    child.on("error", fail);
    child.stdout.on("data", (chunk) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes > MAX_BYTES) {
        child.kill();
        fail(new Error("Project Brain stdout exceeds 1 MiB"));
        return;
      }
      stdout.push(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderrBytes += chunk.length;
      if (stderrBytes <= MAX_BYTES) stderr.push(chunk);
    });
    child.on("close", (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      const stderrText = Buffer.concat(stderr).toString("utf8").trim();
      if (code !== 0) {
        reject(new Error(`Project Brain exited ${code}: ${stderrText}`));
        return;
      }
      const text = Buffer.concat(stdout).toString("utf8").trim();
      if (text.length === 0) {
        resolve(null);
        return;
      }
      try {
        resolve(JSON.parse(text));
      } catch {
        reject(new Error("Project Brain returned invalid JSON"));
      }
    });
    child.stdin.on("error", fail);
    child.stdin.end(request);
  });
}

export default function projectBrainExtension(pi) {
  pi.on("session_start", async (event, ctx) => {
    try {
      const source = event.reason === "resume" ? "resume"
        : event.reason === "new" ? "clear"
        : event.reason === "reload" ? "compact"
        : "startup";
      const output = await invokeBrain("session-start", {
        session_id: sessionId(ctx),
        cwd: ctx.cwd,
        source,
      }, ctx.cwd);
      pendingContext.push(...textItems(output, "context"));
    } catch (error) {
      pendingContext.push(`Project Brain session check degraded: ${String(error)}`);
    }
  });

  pi.on("input", async (event, ctx) => {
    try {
      const output = await invokeBrain("user-prompt-submit", {
        session_id: sessionId(ctx),
        cwd: ctx.cwd,
        source: event.source,
        prompt: event.text,
      }, ctx.cwd);
      pendingContext.push(...textItems(output, "context"));
    } catch (error) {
      pendingContext.push(`Project Brain intent check degraded: ${String(error)}`);
    }
    return { action: "continue" };
  });

  pi.on("before_agent_start", () => {
    if (pendingContext.length === 0) return;
    const content = pendingContext.join("\n\n");
    pendingContext = [];
    return {
      message: {
        customType: "project-brain-context",
        content,
        display: true,
      },
    };
  });

  pi.on("tool_call", async (event, ctx) => {
    try {
      const output = await invokeBrain("pre-tool-use", {
        session_id: sessionId(ctx),
        cwd: ctx.cwd,
        tool_name: event.toolName,
        tool_use_id: event.toolCallId,
        tool_input: event.input,
      }, ctx.cwd);
      if (output?.block === true) {
        return { block: true, reason: output.reason ?? "Blocked by Project Brain" };
      }
      return;
    } catch (error) {
      return { block: true, reason: `Project Brain governance failed closed: ${String(error)}` };
    }
  });

  pi.on("tool_result", async (event, ctx) => {
    let feedback = [];
    try {
      const output = await invokeBrain("post-tool-use", {
        session_id: sessionId(ctx),
        cwd: ctx.cwd,
        tool_name: event.toolName,
        tool_use_id: event.toolCallId,
        tool_input: event.input,
        tool_response: { success: !event.isError },
      }, ctx.cwd);
      feedback = textItems(output, "feedback");
    } catch (error) {
      feedback = [`Project Brain post-tool audit degraded: ${String(error)}`];
    }
    if (feedback.length === 0) return;
    return {
      content: [
        ...event.content,
        { type: "text", text: `Project Brain feedback:\n${feedback.join("\n")}` },
      ],
    };
  });
}
"#;

fn event_arg(event: &str) -> &'static str {
    match event {
        "SessionStart" => "session-start",
        "UserPromptSubmit" => "user-prompt-submit",
        "PreToolUse" => "pre-tool-use",
        "PostToolUse" => "post-tool-use",
        "Stop" => "stop",
        _ => unreachable!("required event"),
    }
}

fn append_managed_groups(
    document: &mut Value,
    handlers: &BTreeMap<String, Value>,
) -> Result<(), AppError> {
    let object = document
        .as_object_mut()
        .ok_or_else(|| AppError::Setup("Codex hooks.json 顶层必须是 JSON object".to_owned()))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| AppError::Setup("Codex hooks.json 的 hooks 必须是 object".to_owned()))?;

    for (event, handler) in handlers {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| AppError::Setup(format!("Codex hooks.{event} 必须是 array")))?;
        let matcher = match event.as_str() {
            "SessionStart" => Some("startup|resume|compact"),
            "PreToolUse" | "PostToolUse" => Some("Bash|apply_patch|Edit|Write"),
            _ => None,
        };
        let mut group = Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".to_owned(), json!(matcher));
        }
        group.insert("hooks".to_owned(), Value::Array(vec![handler.clone()]));
        groups.push(Value::Object(group));
    }
    Ok(())
}

fn append_claude_managed_groups(
    document: &mut Value,
    handlers: &BTreeMap<String, Value>,
) -> Result<(), AppError> {
    let object = document
        .as_object_mut()
        .ok_or_else(|| AppError::Setup("Claude settings.json 顶层必须是 JSON object".to_owned()))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| AppError::Setup("Claude settings.json 的 hooks 必须是 object".to_owned()))?;

    for (event, handler) in handlers {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                AppError::Setup(format!("Claude settings.json hooks.{event} 必须是 array"))
            })?;
        let mut group = Map::new();
        if matches!(event.as_str(), "PreToolUse" | "PostToolUse") {
            group.insert("matcher".to_owned(), json!("Bash|Edit|Write|NotebookEdit"));
        }
        group.insert("hooks".to_owned(), Value::Array(vec![handler.clone()]));
        groups.push(Value::Object(group));
    }
    Ok(())
}

fn observed_managed_hashes(document: &Value) -> BTreeMap<String, String> {
    let mut observed = BTreeMap::new();
    let Some(hooks) = document.get("hooks").and_then(Value::as_object) else {
        return observed;
    };
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for handler in handlers {
                if is_managed_signature(handler) {
                    let hash = hash_value(handler).unwrap_or_default();
                    if observed.insert(event.clone(), hash).is_some() {
                        observed.insert(format!("{event}#duplicate"), String::new());
                    }
                }
            }
        }
    }
    observed
}

fn observed_claude_managed_hashes(document: &Value) -> BTreeMap<String, String> {
    let mut observed = BTreeMap::new();
    let Some(hooks) = document.get("hooks").and_then(Value::as_object) else {
        return observed;
    };
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for handler in handlers {
                if is_claude_managed_signature(handler) {
                    let hash = hash_value(handler).unwrap_or_default();
                    if observed.insert(event.clone(), hash).is_some() {
                        observed.insert(format!("{event}#duplicate"), String::new());
                    }
                }
            }
        }
    }
    observed
}

fn handler_hashes(
    handlers: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, String>, AppError> {
    handlers
        .iter()
        .map(|(event, handler)| Ok((event.clone(), hash_value(handler)?)))
        .collect()
}

fn remove_managed_handlers(
    document: &mut Value,
    expected_hashes: &BTreeSet<String>,
    force: bool,
) -> usize {
    let Some(hooks) = document.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    let mut removed = 0;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = handlers.len();
            handlers.retain(|handler| {
                if !is_managed_signature(handler) {
                    return true;
                }
                let hash = hash_value(handler).unwrap_or_default();
                !(force || expected_hashes.contains(&hash))
            });
            removed += before - handlers.len();
            !(before > 0 && handlers.is_empty())
        });
    }
    removed
}

fn remove_claude_managed_handlers(
    document: &mut Value,
    expected_hashes: &BTreeSet<String>,
    force: bool,
) -> usize {
    let Some(hooks) = document.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    let mut removed = 0;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = handlers.len();
            handlers.retain(|handler| {
                if !is_claude_managed_signature(handler) {
                    return true;
                }
                let hash = hash_value(handler).unwrap_or_default();
                !(force || expected_hashes.contains(&hash))
            });
            removed += before - handlers.len();
            !(before > 0 && handlers.is_empty())
        });
    }
    removed
}

fn is_managed_signature(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(" dispatch codex "))
        || handler
            .get("commandWindows")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(" dispatch codex "))
}

fn is_claude_managed_signature(handler: &Value) -> bool {
    let marker_matches = handler.get("statusMessage").and_then(Value::as_str)
        == Some("Project Brain deterministic governance");
    let args_match = handler
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|args| {
            args.len() == 3
                && args[0] == "dispatch"
                && args[1] == "claude-code"
                && args[2].as_str().is_some_and(|event| {
                    matches!(
                        event,
                        "session-start"
                            | "user-prompt-submit"
                            | "pre-tool-use"
                            | "post-tool-use"
                            | "stop"
                    )
                })
        });
    marker_matches && args_match
}

fn codex_integration_valid(install_root: &Path, codex_home: Option<&Path>) -> bool {
    let Some(codex_home) = codex_home else {
        return false;
    };
    let target = codex_home.join("hooks.json");
    let integration = install_root.join("state/integrations/codex.json");
    let Ok(document) = read_json::<Value>(&target) else {
        return false;
    };
    let Ok(manifest) = read_json::<CodexIntegrationManifest>(&integration) else {
        return false;
    };
    validate_integration_manifest(&manifest, &target).is_ok()
        && observed_managed_hashes(&document) == manifest.managed_handler_hashes
}

fn claude_integration_valid(install_root: &Path, claude_home: Option<&Path>) -> bool {
    let Some(claude_home) = claude_home else {
        return false;
    };
    let target = claude_home.join("settings.json");
    let integration = install_root.join("state/integrations/claude-code.json");
    let Ok(document) = read_json::<Value>(&target) else {
        return false;
    };
    let Ok(manifest) = read_json::<ClaudeIntegrationManifest>(&integration) else {
        return false;
    };
    validate_claude_integration_manifest(&manifest, &target).is_ok()
        && observed_claude_managed_hashes(&document) == manifest.managed_handler_hashes
}

fn prime_integration_valid(
    install_root: &Path,
    prime_home: Option<&Path>,
    project_root: &Path,
) -> bool {
    let Some(prime_home) = prime_home else {
        return false;
    };
    let extension_root = prime_home.join("extensions");
    let extension_directory = extension_root.join("project-brain");
    let target = extension_directory.join("index.ts");
    let integration = install_root.join("state/integrations/prime-agent.json");
    let Ok(manifest) = read_json::<PrimeIntegrationManifest>(&integration) else {
        return false;
    };
    if validate_prime_integration_manifest(&manifest, &target).is_err()
        || !prime_extension_directory_exact(&extension_directory, &target)
        || target_hash_exact(&target).as_deref() != Some(manifest.target_sha256.as_str())
        || prime_extension_conflict_exists(&extension_root)
    {
        return false;
    }
    let Ok(expected_launcher) =
        stable_launcher_path(install_root, &env::current_exe().unwrap_or_default())
    else {
        return false;
    };
    if manifest.launcher_path != expected_launcher {
        return false;
    }
    let Ok(launcher_bytes) = fs::read(&manifest.launcher_path) else {
        return false;
    };
    if digest_bytes(&launcher_bytes) != manifest.launcher_sha256 {
        return false;
    }
    let Ok(expected) = render_prime_extension(&manifest.launcher_path) else {
        return false;
    };
    if digest_bytes(&expected) != manifest.target_sha256 {
        return false;
    }
    let target_outside_project = canonical_directory_boundary(&extension_directory)
        .ok()
        .zip(canonical_directory_boundary(project_root).ok())
        .is_some_and(|(extension, project)| !extension.starts_with(project));
    target_outside_project && prime_launcher_fixture_valid(&manifest.launcher_path)
}

fn prime_extension_directory_exact(extension_directory: &Path, target: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(target) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let Ok(entries) = fs::read_dir(extension_directory) else {
        return false;
    };
    let entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.len() == 1 && entries[0].path() == target
}

fn prime_extension_conflict_exists(extension_root: &Path) -> bool {
    [
        "project-brain.ts",
        "project-brain.js",
        "project_brain.ts",
        "project_brain.js",
    ]
    .iter()
    .any(|name| extension_root.join(name).exists())
}

fn prime_launcher_fixture_valid(launcher: &Path) -> bool {
    let Ok(output) = Command::new(launcher)
        .args(["capabilities", "prime-agent"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&output.stdout).is_ok_and(|value| {
        value.get("deny_tool").and_then(Value::as_str) == Some("supported")
            && value.get("continue_after_stop").and_then(Value::as_str) == Some("unsupported")
    })
}

fn target_hash_exact(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    fs::read(path).ok().map(|bytes| digest_bytes(&bytes))
}

fn quote_posix(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\"'\"'"))
}

fn quote_powershell(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

fn require_object(value: &Value, message: &str) -> Result<(), AppError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(AppError::Setup(message.to_owned()))
    }
}

fn install_versioned_payload(
    source: &Path,
    target: &Path,
    version: &str,
) -> Result<bool, AppError> {
    let source_bytes = fs::read(source)?;
    if target.is_file() {
        if fs::read(target)? == source_bytes {
            fs::set_permissions(target, fs::metadata(source)?.permissions())?;
            return Ok(false);
        }
        return Err(AppError::Setup(format!(
            "版本 {version} 已存在但内容不同；版本目录不可原地覆盖，请发布新版本号"
        )));
    }
    atomic_replace(target, &source_bytes, None)?;
    fs::set_permissions(target, fs::metadata(source)?.permissions())?;
    Ok(true)
}

fn copy_file_atomically(source: &Path, target: &Path) -> Result<(), AppError> {
    atomic_replace(target, &fs::read(source)?, None)?;
    fs::set_permissions(target, fs::metadata(source)?.permissions())?;
    Ok(())
}

pub(crate) fn atomic_replace(
    target: &Path,
    bytes: &[u8],
    expected_current_hash: Option<&str>,
) -> Result<(), AppError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(expected) = expected_current_hash {
        let current = if target.is_file() {
            digest_bytes(&fs::read(target)?)
        } else {
            digest_bytes(b"{}\n")
        };
        if current != expected {
            return Err(AppError::ConcurrentModification(target.to_owned()));
        }
    }
    let mut file = AtomicWriteFile::options().open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(expected) = expected_current_hash {
        let current = if target.is_file() {
            digest_bytes(&fs::read(target)?)
        } else {
            digest_bytes(b"{}\n")
        };
        if current != expected {
            return Err(AppError::ConcurrentModification(target.to_owned()));
        }
    }
    file.commit()?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_optional(path: &Path) -> Result<Vec<u8>, AppError> {
    if path.is_file() {
        Ok(fs::read(path)?)
    } else {
        Ok(Vec::new())
    }
}

pub(crate) fn pretty_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, AppError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn hash_value(value: &Value) -> Result<String, AppError> {
    Ok(digest_bytes(&serde_json::to_vec(value)?))
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) struct MutationLock {
    _file: fs::File,
}

impl MutationLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(AppError::from)?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                AppError::Setup(format!("另一个状态变更正在进行：{}", path.display()))
            }
            fs::TryLockError::Error(error) => error.into(),
        })?;
        Ok(Self { _file: file })
    }
}

pub(crate) fn target_hash(path: &Path) -> Result<String, AppError> {
    if path.is_file() {
        Ok(digest_bytes(&fs::read(path)?))
    } else {
        Ok(digest_bytes(b"{}\n"))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use serde_json::{Value, json};

    use super::{
        MutationLock, ProjectRegistry, append_managed_groups, canonical_directory_boundary,
        handler_hashes, install_codex_hooks, managed_handlers, observed_managed_hashes, read_json,
        remove_managed_handlers, stable_launcher_path,
    };

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-setup-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn canonical_directory_boundary_resolves_missing_suffixes_against_existing_ancestor() {
        let root = temp_root("canonical-boundary");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();

        let project_boundary = canonical_directory_boundary(&project).unwrap();
        let nested =
            canonical_directory_boundary(&project.join("missing").join("..").join("machine-state"))
                .unwrap();
        let sibling = canonical_directory_boundary(&root.join("machine-state")).unwrap();

        assert!(nested.starts_with(&project_boundary));
        assert!(!sibling.starts_with(&project_boundary));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_hook_merge_preserves_existing_structure_and_is_exactly_five() {
        let launcher = std::path::Path::new("C:/Program Files/Project Brain/project-brain.exe");
        let handlers = managed_handlers(launcher);
        let mut document = json!({
            "description": "user hooks",
            "custom": {"preserve": true},
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "user-policy"}]
                }]
            }
        });
        append_managed_groups(&mut document, &handlers).unwrap();
        assert_eq!(document["custom"]["preserve"], true);
        assert_eq!(document["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            observed_managed_hashes(&document),
            handler_hashes(&handlers).unwrap()
        );
        let windows = document["hooks"]["SessionStart"][0]["hooks"][0]["commandWindows"]
            .as_str()
            .unwrap();
        assert!(windows.starts_with("& 'C:/Program Files/Project Brain/"));
    }

    #[test]
    fn managed_hook_uninstall_preserves_user_handlers_added_later() {
        let handlers = managed_handlers(std::path::Path::new("C:/pb/project-brain.exe"));
        let hashes = handler_hashes(&handlers).unwrap();
        let mut document = json!({"hooks": {}});
        append_managed_groups(&mut document, &handlers).unwrap();
        document["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks": [{"type": "command", "command": "user-stop"}]}));
        let removed =
            remove_managed_handlers(&mut document, &hashes.values().cloned().collect(), false);
        assert_eq!(removed, 5);
        assert_eq!(document["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(
            document["hooks"]["Stop"][0]["hooks"][0]["command"],
            "user-stop"
        );
    }

    #[test]
    fn malformed_hooks_json_is_never_replaced() {
        let root = temp_root("malformed");
        let install_root = root.join("install");
        let codex_home = root.join("codex");
        fs::create_dir_all(install_root.join("state")).unwrap();
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            install_root.join("state/install.json"),
            "{\"schema_version\":1,\"current\":\"0.1.0\",\"previous\":null}\n",
        )
        .unwrap();
        let launcher =
            stable_launcher_path(&install_root, &std::env::current_exe().unwrap()).unwrap();
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(&launcher, b"fixture launcher").unwrap();
        let payload = install_root
            .join("versions/0.1.0")
            .join(std::env::current_exe().unwrap().file_name().unwrap());
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::write(payload, b"fixture payload").unwrap();
        fs::write(codex_home.join("hooks.json"), b"{ invalid").unwrap();
        let before = fs::read(codex_home.join("hooks.json")).unwrap();
        assert!(install_codex_hooks(Some(&install_root), Some(&codex_home)).is_err());
        assert_eq!(fs::read(codex_home.join("hooks.json")).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_registry_schema_is_explicit() {
        let registry: ProjectRegistry = serde_json::from_value(json!({
            "schema_version": 1,
            "projects": []
        }))
        .unwrap();
        assert_eq!(registry.schema_version, 1);
        let value: Value = serde_json::to_value(registry).unwrap();
        assert_eq!(value["projects"], json!([]));
    }

    #[test]
    fn read_json_rejects_invalid_json() {
        let root = temp_root("invalid-json");
        let path = root.join("value.json");
        fs::write(&path, "not json").unwrap();
        assert!(read_json::<Value>(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mutation_lock_survives_persistent_lock_file_and_releases_on_drop() {
        let root = temp_root("mutation-lock");
        let path = root.join("state.lock");
        let first = MutationLock::acquire(&path).unwrap();
        assert!(MutationLock::acquire(&path).is_err());
        drop(first);
        assert!(path.is_file());
        assert!(MutationLock::acquire(&path).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
