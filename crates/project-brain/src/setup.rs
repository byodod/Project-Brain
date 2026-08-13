use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use brain_core::SemanticProviderProfile;

use crate::{error::AppError, provider};

const INSTALL_SCHEMA_VERSION: u32 = 1;
const REGISTRY_SCHEMA_VERSION: u32 = 1;
const CODEX_INTEGRATION_VERSION: u32 = 1;
const INSTALL_ROOT_ENV: &str = "PROJECT_BRAIN_INSTALL_ROOT";
const LAUNCHED_ENV: &str = "PROJECT_BRAIN_LAUNCHED";
const REQUIRED_CODEX_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
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
    pub codex_hooks: CheckState,
    pub codex_trust_state: &'static str,
    pub issues: Vec<String>,
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

pub fn doctor(
    explicit_install_root: Option<&Path>,
    explicit_codex_home: Option<&Path>,
    project_root: &Path,
    project_key: &str,
    provider_profiles: &[SemanticProviderProfile],
) -> DoctorReport {
    let mut issues = Vec::new();
    let install_root = match resolve_install_root(explicit_install_root) {
        Ok(root) => root,
        Err(error) => {
            return DoctorReport {
                schema_version: INSTALL_SCHEMA_VERSION,
                status: "broken",
                install_root: PathBuf::new(),
                launcher: CheckState::Fail,
                payload: CheckState::Fail,
                project_registration: CheckState::Fail,
                providers: CheckState::Fail,
                codex_hooks: CheckState::Fail,
                codex_trust_state: "not_programmatically_verifiable",
                issues: vec![error.to_string()],
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
    let codex_hooks_valid = codex_integration_valid(
        &install_root,
        resolve_codex_home(explicit_codex_home).ok().as_deref(),
    );
    if !codex_hooks_valid {
        issues.push("Codex 用户级 Hook 缺失、重复或发生漂移".to_owned());
    }
    DoctorReport {
        schema_version: INSTALL_SCHEMA_VERSION,
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
        codex_hooks: codex_hooks_valid.into(),
        codex_trust_state: "not_programmatically_verifiable",
        issues,
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
        MutationLock, ProjectRegistry, append_managed_groups, handler_hashes, install_codex_hooks,
        managed_handlers, observed_managed_hashes, read_json, remove_managed_handlers,
        stable_launcher_path,
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
