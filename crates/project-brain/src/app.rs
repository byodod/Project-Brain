use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use brain_core::{
    ActionDescriptor, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, MemoryStatus,
    ProjectLanguageProfile, Rule, RuleEffect, RuleEngine, RuleStrength, RuleSymbolScope,
    SemanticLanguageMapping, SemanticProviderFormat, SemanticProviderProfile, StopReconcileConfig,
    SymbolResolutionPolicy, normalize_project_path,
};
use brain_evidence::{
    EvidenceAuthority, EvidenceCoverage, EvidenceFreshness, EvidencePlane, EvidenceReference,
    EvidenceSnapshot,
};
use brain_store::{
    BrainStore, EvidenceApplyResult, EvidenceHeadSummary, SemanticResolutionKind,
    inspect_database_storage,
};
use clap::ValueEnum;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    analyze, build,
    claude::{self, ClaudeHookInput},
    codex::{self, CodexHookInput},
    database::{self, DatabaseAccessLock, DatabaseCompactOptions},
    error::AppError,
    evidence::{CurrentSourceVerification, effective_evidence_freshness},
    git, godot, index,
    prime::{self, PrimeHookInput},
    provider, qualification, reconcile, runtime, scip_index, setup, test,
};

const BRAIN_DIRECTORY: &str = ".project-brain";
const CONFIG_FILE: &str = "config.json";
const DATABASE_FILE: &str = "brain.db";
const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AgentKind {
    Codex,
    ClaudeCode,
    PrimeAgent,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

/// 由用户显式选择的项目语言/语义 Provider 模板。
///
/// 这里刻意不提供 `Auto`：Project Brain 不根据仓库文件猜测语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum ProjectProfile {
    Rust,
    Dotnet,
    Python,
}

pub struct App {
    root: PathBuf,
    config: BrainConfig,
    store: BrainStore,
    _database_lock: DatabaseAccessLock,
}

#[derive(Debug, Serialize)]
struct EvidenceStatusHead {
    #[serde(flatten)]
    recorded: EvidenceHeadSummary,
    effective_freshness: EvidenceFreshness,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct EvidenceStatusReport<'a> {
    schema_version: u16,
    project_key: &'a str,
    current_source_fingerprint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_source_error: Option<&'a str>,
    heads: Vec<EvidenceStatusHead>,
}

impl App {
    pub fn install_machine(install_root: Option<&Path>) -> Result<(), AppError> {
        println!("{}", pretty_json(&setup::install(install_root)?)?);
        Ok(())
    }

    pub fn rollback_machine(install_root: Option<&Path>) -> Result<(), AppError> {
        println!("{}", pretty_json(&setup::rollback(install_root)?)?);
        Ok(())
    }

    pub fn install_hooks(
        install_root: Option<&Path>,
        agent_home: Option<&Path>,
        agent: AgentKind,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                println!(
                    "{}",
                    pretty_json(&setup::install_codex_hooks(install_root, agent_home)?)?
                );
            }
            AgentKind::ClaudeCode => {
                println!(
                    "{}",
                    pretty_json(&setup::install_claude_hooks(install_root, agent_home)?)?
                );
            }
            AgentKind::PrimeAgent => {
                println!(
                    "{}",
                    pretty_json(&setup::install_prime_extension(install_root, agent_home)?)?
                );
            }
        }
        Ok(())
    }

    pub fn uninstall_hooks(
        install_root: Option<&Path>,
        agent_home: Option<&Path>,
        agent: AgentKind,
        force: bool,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                println!(
                    "{}",
                    pretty_json(&setup::uninstall_codex_hooks(
                        install_root,
                        agent_home,
                        force,
                    )?)?
                );
            }
            AgentKind::ClaudeCode => {
                println!(
                    "{}",
                    pretty_json(&setup::uninstall_claude_hooks(
                        install_root,
                        agent_home,
                        force,
                    )?)?
                );
            }
            AgentKind::PrimeAgent => {
                println!(
                    "{}",
                    pretty_json(&setup::uninstall_prime_extension(
                        install_root,
                        agent_home,
                        force,
                    )?)?
                );
            }
        }
        Ok(())
    }

    pub fn bootstrap(
        &self,
        install_root: Option<&Path>,
        codex_home: Option<&Path>,
        install_codex: bool,
    ) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&setup::bootstrap(
                install_root,
                codex_home,
                &self.root,
                &self.config.project_key,
                &self.config.semantic_providers,
                install_codex,
            )?)?
        );
        Ok(())
    }

    pub fn doctor(
        &self,
        install_root: Option<&Path>,
        agent: AgentKind,
        agent_home: Option<&Path>,
        require_qualified: bool,
    ) -> Result<(), AppError> {
        let adapter = match agent {
            AgentKind::Codex => setup::DoctorAdapter::Codex,
            AgentKind::ClaudeCode => setup::DoctorAdapter::ClaudeCode,
            AgentKind::PrimeAgent => setup::DoctorAdapter::PrimeAgent,
        };
        let mut report = setup::doctor(
            install_root,
            adapter,
            agent_home,
            &self.root,
            &self.config.project_key,
            &self.config.semantic_providers,
        );
        let semantic_coverage = scip_index::doctor_coverage(
            &self.root,
            &self.config.project_key,
            &self.config.language_profiles,
            &self.config.semantic_providers,
            &self.store,
        );
        report
            .issues
            .extend(semantic_coverage.issues.iter().cloned());
        report
            .warnings
            .extend(semantic_coverage.warnings.iter().cloned());
        if !semantic_coverage.issues.is_empty() {
            report.status = "degraded";
        }
        report.semantic_coverage = Some(semantic_coverage);
        match qualification::status(install_root) {
            Ok(status) => {
                if !status.qualified {
                    let message = format!(
                        "当前二进制/控制面合同 {} 尚无 Qualified 证明",
                        status.current_target.target_hash
                    );
                    if require_qualified {
                        report.issues.push(message);
                        report.status = "degraded";
                    } else {
                        report.warnings.push(message);
                    }
                }
                report.qualification = Some(status);
            }
            Err(error) => {
                let message = format!("无法读取 Production Qualification 状态：{error}");
                if require_qualified {
                    report.issues.push(message);
                    report.status = "degraded";
                } else {
                    report.warnings.push(message);
                }
            }
        }
        println!("{}", pretty_json(&report)?);
        if report.is_ready() {
            Ok(())
        } else {
            Err(AppError::DoctorDegraded(report.issues.join("；")))
        }
    }

    pub fn qualification_run(
        &self,
        install_root: Option<&Path>,
        request_id: &str,
    ) -> Result<(), AppError> {
        let report = qualification::run(install_root, &self.root, &self.config, request_id)?;
        println!("{}", pretty_json(&report)?);
        if report.status == qualification::QualificationState::Qualified {
            Ok(())
        } else {
            Err(AppError::Qualification(format!(
                "运行 {} 的最终状态为 {}",
                report.run_id,
                report.status.as_str()
            )))
        }
    }

    pub fn qualification_status(install_root: Option<&Path>) -> Result<(), AppError> {
        println!("{}", pretty_json(&qualification::status(install_root)?)?);
        Ok(())
    }

    pub fn qualification_show(install_root: Option<&Path>, run_id: &str) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&qualification::show(install_root, run_id)?)?
        );
        Ok(())
    }

    pub fn dispatch_hook(
        install_root: Option<&Path>,
        agent: AgentKind,
        event: HookEvent,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                // 用户级 dispatcher 会在所有 Codex 项目中运行；无法解析的输入既不能定位
                // 已注册项目，也不能安全执行治理逻辑，因此保持静默 NO-OP。
                let input: CodexHookInput = match read_stdin_json_limited(MAX_HOOK_INPUT_BYTES) {
                    Ok(input) => input,
                    Err(_) => return Ok(()),
                };
                let Some((root, registered_project_key)) =
                    setup::registered_project_for_cwd(install_root, Path::new(input.cwd()))?
                else {
                    return Ok(());
                };
                let app = Self::open(Some(root))?;
                if app.config.project_key != registered_project_key {
                    let error = format!(
                        "本机注册 project_key={} 与仓库 project_key={} 不一致",
                        registered_project_key, app.config.project_key
                    );
                    if let Some(output) = codex::failure_output(event, &input, &error) {
                        println!("{}", pretty_json(&output)?);
                        return Ok(());
                    }
                    return Err(AppError::Setup(error));
                }
                let provider_trust = provider::trust_status(
                    install_root,
                    &app.root,
                    &app.config.project_key,
                    &app.config.semantic_providers,
                );
                let output = codex::handle_with_provider_trust(
                    &app.root,
                    &app.config,
                    &app.store,
                    &provider_trust,
                    event,
                    &input,
                )?;
                println!("{}", pretty_json(&output)?);
                Ok(())
            }
            AgentKind::ClaudeCode => {
                let input: ClaudeHookInput = match read_stdin_json_limited(MAX_HOOK_INPUT_BYTES) {
                    Ok(input) => input,
                    Err(_) => return Ok(()),
                };
                let Some((root, registered_project_key)) =
                    setup::registered_project_for_cwd(install_root, Path::new(input.cwd()))?
                else {
                    return Ok(());
                };
                let app = Self::open(Some(root))?;
                if app.config.project_key != registered_project_key {
                    let error = format!(
                        "本机注册 project_key={} 与仓库 project_key={} 不一致",
                        registered_project_key, app.config.project_key
                    );
                    if let Some(output) = claude::failure_output(event, &input, &error) {
                        println!("{}", pretty_json(&output)?);
                        return Ok(());
                    }
                    return Err(AppError::Setup(error));
                }
                let provider_trust = provider::trust_status(
                    install_root,
                    &app.root,
                    &app.config.project_key,
                    &app.config.semantic_providers,
                );
                let output = claude::handle_with_provider_trust(
                    &app.root,
                    &app.config,
                    &app.store,
                    &provider_trust,
                    event,
                    &input,
                )?;
                println!("{}", pretty_json(&output)?);
                Ok(())
            }
            AgentKind::PrimeAgent => Self::dispatch_prime_hook(install_root, event),
        }
    }

    fn dispatch_prime_hook(install_root: Option<&Path>, event: HookEvent) -> Result<(), AppError> {
        let input: PrimeHookInput = match read_stdin_json_limited(MAX_HOOK_INPUT_BYTES) {
            Ok(input) => input,
            Err(_) => return Ok(()),
        };
        let Some((root, registered_project_key)) =
            setup::registered_project_for_cwd(install_root, Path::new(input.cwd()))?
        else {
            return Ok(());
        };
        let app = Self::open(Some(root))?;
        if app.config.project_key != registered_project_key {
            println!(
                "{}",
                pretty_json(&prime::failure_output(
                    event,
                    &format!(
                        "本机注册 project_key={} 与仓库 project_key={} 不一致",
                        registered_project_key, app.config.project_key
                    )
                ))?
            );
            return Ok(());
        }
        let provider_trust = provider::trust_status(
            install_root,
            &app.root,
            &app.config.project_key,
            &app.config.semantic_providers,
        );
        let output = prime::handle_with_provider_trust(
            &app.root,
            &app.config,
            &app.store,
            &provider_trust,
            event,
            &input,
        );
        println!("{}", pretty_json(&output)?);
        Ok(())
    }

    pub fn capabilities(agent: AgentKind) -> Result<(), AppError> {
        let capabilities = match agent {
            AgentKind::Codex => codex::capabilities(),
            AgentKind::ClaudeCode => claude::capabilities(),
            AgentKind::PrimeAgent => prime::capabilities(),
        };
        println!("{}", pretty_json(&capabilities)?);
        Ok(())
    }

    pub fn init(
        explicit_root: Option<PathBuf>,
        profiles: &[ProjectProfile],
    ) -> Result<(), AppError> {
        let root = explicit_root.unwrap_or(env::current_dir()?);
        let brain_dir = root.join(BRAIN_DIRECTORY);
        let config_path = brain_dir.join(CONFIG_FILE);
        if config_path.exists() {
            return Err(AppError::AlreadyInitialized(config_path));
        }

        fs::create_dir_all(&brain_dir)?;
        let project_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned();
        let config = initial_config(project_name, generate_project_key(&root)?, profiles);
        config.validate()?;
        fs::write(&config_path, pretty_json(&config)?)?;
        fs::write(brain_dir.join(".gitignore"), "brain.db*\n.brain.db*\n")?;
        fs::write(
            brain_dir.join("envelope.json"),
            pretty_json(&reconcile::ChangeEnvelope::example())?,
        )?;
        BrainStore::open(&brain_dir.join(DATABASE_FILE))?;

        println!("Project Brain 已初始化：{}", brain_dir.display());
        Ok(())
    }

    pub fn open(explicit_root: Option<PathBuf>) -> Result<Self, AppError> {
        Self::open_with_database_access(explicit_root, None)
    }

    pub fn open_exclusive_maintenance(
        explicit_root: Option<PathBuf>,
        lock_timeout_seconds: u64,
    ) -> Result<Self, AppError> {
        Self::open_with_database_access(
            explicit_root,
            Some(Duration::from_secs(lock_timeout_seconds)),
        )
    }

    fn open_with_database_access(
        explicit_root: Option<PathBuf>,
        exclusive_timeout: Option<Duration>,
    ) -> Result<Self, AppError> {
        let start = explicit_root.unwrap_or(env::current_dir()?);
        let root = discover_root(&start).ok_or(AppError::ProjectNotInitialized)?;
        let brain_dir = root.join(BRAIN_DIRECTORY);
        let config_path = brain_dir.join(CONFIG_FILE);
        let config_bytes = fs::read(&config_path)?;
        let mut config: BrainConfig = serde_json::from_slice(&config_bytes)?;
        let migrated_project_key = config.project_key.is_empty();
        if migrated_project_key {
            config.project_key = legacy_project_key(&config)?;
        }
        config.validate()?;
        if migrated_project_key {
            fs::write(&config_path, pretty_json(&config)?)?;
        }
        let database = brain_dir.join(DATABASE_FILE);
        let database_lock = if let Some(timeout) = exclusive_timeout {
            DatabaseAccessLock::acquire_exclusive(&database, timeout)?
        } else {
            DatabaseAccessLock::acquire_shared(&database)?
        };
        database::ensure_no_pending_operation(&database)?;
        let store = BrainStore::open(&database)?;
        Ok(Self {
            root,
            config,
            store,
            _database_lock: database_lock,
        })
    }

    pub fn preflight(&self) -> Result<(), AppError> {
        let action: ActionDescriptor = read_stdin_json()?;
        let decision = RuleEngine::new(&self.config)?.evaluate(&action)?;
        self.store.record("preflight", &action, &decision)?;
        println!("{}", pretty_json(&decision)?);
        Ok(())
    }

    pub fn run_hook(
        explicit_root: Option<PathBuf>,
        install_root: Option<&Path>,
        agent: AgentKind,
        event: HookEvent,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                let input: CodexHookInput = match read_stdin_json() {
                    Ok(input) => input,
                    Err(error) => {
                        if let Some(output) = codex::failure_output(
                            event,
                            &CodexHookInput::default(),
                            &error.to_string(),
                        ) {
                            println!("{}", pretty_json(&output)?);
                            return Ok(());
                        }
                        return Err(error);
                    }
                };
                let app = match Self::open(explicit_root) {
                    Ok(app) => app,
                    Err(error) => {
                        if let Some(output) =
                            codex::failure_output(event, &input, &error.to_string())
                        {
                            println!("{}", pretty_json(&output)?);
                            return Ok(());
                        }
                        return Err(error);
                    }
                };
                let provider_trust = provider::trust_status(
                    install_root,
                    &app.root,
                    &app.config.project_key,
                    &app.config.semantic_providers,
                );
                let output = codex::handle_with_provider_trust(
                    &app.root,
                    &app.config,
                    &app.store,
                    &provider_trust,
                    event,
                    &input,
                )?;
                println!("{}", pretty_json(&output)?);
                Ok(())
            }
            AgentKind::ClaudeCode => {
                let input: ClaudeHookInput = match read_stdin_json() {
                    Ok(input) => input,
                    Err(error) => {
                        if let Some(output) = claude::failure_output(
                            event,
                            &ClaudeHookInput::default(),
                            &error.to_string(),
                        ) {
                            println!("{}", pretty_json(&output)?);
                            return Ok(());
                        }
                        return Err(error);
                    }
                };
                let app = match Self::open(explicit_root) {
                    Ok(app) => app,
                    Err(error) => {
                        if let Some(output) =
                            claude::failure_output(event, &input, &error.to_string())
                        {
                            println!("{}", pretty_json(&output)?);
                            return Ok(());
                        }
                        return Err(error);
                    }
                };
                let provider_trust = provider::trust_status(
                    install_root,
                    &app.root,
                    &app.config.project_key,
                    &app.config.semantic_providers,
                );
                let output = claude::handle_with_provider_trust(
                    &app.root,
                    &app.config,
                    &app.store,
                    &provider_trust,
                    event,
                    &input,
                )?;
                println!("{}", pretty_json(&output)?);
                Ok(())
            }
            AgentKind::PrimeAgent => Self::run_prime_hook(explicit_root, install_root, event),
        }
    }

    fn run_prime_hook(
        explicit_root: Option<PathBuf>,
        install_root: Option<&Path>,
        event: HookEvent,
    ) -> Result<(), AppError> {
        let input: PrimeHookInput = match read_stdin_json() {
            Ok(input) => input,
            Err(error) => {
                println!(
                    "{}",
                    pretty_json(&prime::failure_output(event, &error.to_string()))?
                );
                return Ok(());
            }
        };
        let app = match Self::open(explicit_root) {
            Ok(app) => app,
            Err(error) => {
                println!(
                    "{}",
                    pretty_json(&prime::failure_output(event, &error.to_string()))?
                );
                return Ok(());
            }
        };
        let provider_trust = provider::trust_status(
            install_root,
            &app.root,
            &app.config.project_key,
            &app.config.semantic_providers,
        );
        let output = prime::handle_with_provider_trust(
            &app.root,
            &app.config,
            &app.store,
            &provider_trust,
            event,
            &input,
        );
        println!("{}", pretty_json(&output)?);
        Ok(())
    }

    pub fn reconcile(&self, base: &str, envelope: &Path) -> Result<(), AppError> {
        let report = reconcile::evaluate_from_path(&self.root, base, envelope)?;
        println!("{}", pretty_json(&report)?);
        Ok(())
    }

    pub fn evidence_godot(
        &self,
        executable: &Path,
        trust_local_executable: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let report = godot::run(
            &self.root,
            &self.config.project_key,
            executable,
            trust_local_executable,
            timeout_seconds,
        )?;
        let persistence = self.persist_current_evidence_snapshot(report.evidence_snapshot())?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        Ok(())
    }

    pub fn evidence_status(&self) -> Result<(), AppError> {
        let current_source = CurrentSourceVerification::inspect(&self.root);
        let heads = self
            .store
            .list_evidence_head_summaries(&self.config.project_key)?
            .into_iter()
            .map(|recorded| {
                let effective = effective_evidence_freshness(
                    recorded.freshness,
                    &recorded.source_fingerprint,
                    &current_source,
                );
                EvidenceStatusHead {
                    recorded,
                    effective_freshness: effective.freshness,
                    effective_reason: effective.reason,
                }
            })
            .collect();
        println!(
            "{}",
            pretty_json(&EvidenceStatusReport {
                schema_version: 1,
                project_key: &self.config.project_key,
                current_source_fingerprint: current_source.fingerprint(),
                current_source_error: current_source.error(),
                heads,
            })?
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_runtime_godot(
        &self,
        install_root: Option<&Path>,
        bundle_fingerprint: &str,
        executable: &Path,
        trust_local_executable: bool,
        quit_after: u32,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let heads = self.store.list_evidence_heads(&self.config.project_key)?;
        let report = runtime::run_godot(&runtime::RuntimeRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            bundle_fingerprint,
            executable,
            trust_local_executable,
            quit_after,
            timeout_seconds,
            evidence_heads: &heads,
        })?;
        let succeeded = report.succeeded();
        let persistence = self.persist_current_evidence_snapshot(report.evidence_snapshot())?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if succeeded {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Runtime Evidence 已保存失败结果；运行合同未通过".to_owned(),
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_test_dotnet(
        &self,
        install_root: Option<&Path>,
        executable: &Path,
        profile: &str,
        build_profile: &str,
        target: &Path,
        test_assembly: &Path,
        trust_local_executable: bool,
        trust_repository_test_code: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let heads = self.store.list_evidence_heads(&self.config.project_key)?;
        let report = test::run_dotnet(&test::DotnetTestRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            profile_id: profile,
            build_profile_id: build_profile,
            executable,
            target,
            test_assembly,
            trust_local_executable,
            trust_repository_test_code,
            timeout_seconds,
            evidence_heads: &heads,
        })?;
        let passed = report.passed();
        let persistence = self.persist_current_evidence_snapshot(&report.evidence)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if passed {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Test Evidence 已保存非通过结果；测试合同未通过".to_owned(),
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_test_rust(
        &self,
        executable: &Path,
        profile: &str,
        build_profile: &str,
        manifest: &Path,
        trust_local_executable: bool,
        trust_repository_test_code: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let heads = self.store.list_evidence_heads(&self.config.project_key)?;
        let report = test::run_rust(&test::RustTestRequest {
            project_root: &self.root,
            project_key: &self.config.project_key,
            profile_id: profile,
            build_profile_id: build_profile,
            executable,
            manifest,
            trust_local_executable,
            trust_repository_test_code,
            timeout_seconds,
            evidence_heads: &heads,
        })?;
        let passed = report.passed();
        let persistence = self.persist_current_evidence_snapshot(&report.evidence)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if passed {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Rust Test Evidence 已保存非通过结果；测试合同未通过".to_owned(),
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_test_python(
        &self,
        executable: &Path,
        profile: &str,
        build_profile: &str,
        source_root: &Path,
        manifest: &Path,
        trust_local_executable: bool,
        trust_repository_test_code: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let heads = self.store.list_evidence_heads(&self.config.project_key)?;
        let report = test::run_python(&test::PythonTestRequest {
            project_root: &self.root,
            project_key: &self.config.project_key,
            profile_id: profile,
            build_profile_id: build_profile,
            executable,
            source_root,
            manifest,
            trust_local_executable,
            trust_repository_test_code,
            timeout_seconds,
            evidence_heads: &heads,
        })?;
        let passed = report.passed();
        let persistence = self.persist_current_evidence_snapshot(&report.evidence)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if passed {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Python Test Evidence 已保存非通过结果；测试合同未通过".to_owned(),
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_test_godot(
        &self,
        install_root: Option<&Path>,
        executable: &Path,
        profile: &str,
        build_profile: &str,
        target: &Path,
        scenario: &Path,
        trust_local_executable: bool,
        trust_repository_test_code: bool,
        quit_after: u32,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let heads = self.store.list_evidence_heads(&self.config.project_key)?;
        let report = test::run_godot_scenario(&test::GodotScenarioTestRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            profile_id: profile,
            build_profile_id: build_profile,
            executable,
            target,
            scenario,
            trust_local_executable,
            trust_repository_test_code,
            quit_after,
            timeout_seconds,
            evidence_heads: &heads,
        })?;
        let passed = report.passed();
        let persistence = self.persist_current_evidence_snapshot(&report.evidence)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if passed {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Godot Scenario Test Evidence 已保存非通过结果；测试合同未通过".to_owned(),
            ))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_build_dotnet(
        &self,
        install_root: Option<&Path>,
        executable: &Path,
        profile: &str,
        target: &Path,
        require_engine: bool,
        trust_local_executable: bool,
        trust_repository_build_code: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let upstream = if require_engine {
            vec![self.required_engine_reference()?]
        } else {
            Vec::new()
        };
        let report = build::run_dotnet(&build::BuildRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            profile_id: profile,
            executable,
            target,
            trust_local_executable,
            trust_repository_build_code,
            timeout_seconds,
            upstream,
        })?;
        self.persist_build_report(&report)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence_build_rust(
        &self,
        install_root: Option<&Path>,
        executable: &Path,
        profile: &str,
        manifest: &Path,
        trust_local_executable: bool,
        trust_repository_build_code: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let report = build::run_rust(&build::BuildRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            profile_id: profile,
            executable,
            target: manifest,
            trust_local_executable,
            trust_repository_build_code,
            timeout_seconds,
            upstream: Vec::new(),
        })?;
        self.persist_build_report(&report)
    }

    pub fn evidence_build_python(
        &self,
        install_root: Option<&Path>,
        executable: &Path,
        profile: &str,
        source_root: &Path,
        trust_local_executable: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let report = build::run_python(&build::BuildRequest {
            project_root: &self.root,
            install_root,
            project_key: &self.config.project_key,
            profile_id: profile,
            executable,
            target: source_root,
            trust_local_executable,
            trust_repository_build_code: false,
            timeout_seconds,
            upstream: Vec::new(),
        })?;
        self.persist_build_report(&report)
    }

    fn persist_build_report(&self, report: &build::BuildRunReport) -> Result<(), AppError> {
        let succeeded = report.succeeded();
        let persistence = self.persist_current_evidence_snapshot(report.evidence_snapshot())?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": 1,
                "run": report,
                "persistence": persistence,
            }))?
        );
        if succeeded {
            Ok(())
        } else {
            Err(AppError::Provider(
                "Build Evidence 已保存失败或未完成结果；构建合同未通过".to_owned(),
            ))
        }
    }

    fn persist_current_evidence_snapshot(
        &self,
        snapshot: &EvidenceSnapshot,
    ) -> Result<EvidenceApplyResult, AppError> {
        let current_source_fingerprint = git::worktree_fingerprint(&self.root)?;
        Ok(self
            .store
            .apply_evidence_snapshot_for_current_source(snapshot, &current_source_fingerprint)?)
    }

    fn required_engine_reference(&self) -> Result<EvidenceReference, AppError> {
        let current_source = CurrentSourceVerification::inspect(&self.root);
        let candidates = self
            .store
            .list_evidence_head_summaries(&self.config.project_key)?
            .into_iter()
            .filter(|head| {
                head.plane == EvidencePlane::Engine
                    && effective_evidence_freshness(
                        head.freshness,
                        &head.source_fingerprint,
                        &current_source,
                    )
                    .freshness
                        == EvidenceFreshness::Fresh
                    && head.coverage == EvidenceCoverage::Complete
                    && head.authority == EvidenceAuthority::Deterministic
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err(AppError::Provider(format!(
                "--require-engine 需要且只允许一个与当前 Source 指纹匹配的 effective-fresh+complete+deterministic Engine head，实际={}{}",
                candidates.len(),
                current_source
                    .error()
                    .map(|error| format!("；当前 Source fingerprint 无法验证：{error}"))
                    .unwrap_or_default()
            )));
        }
        let head = &candidates[0];
        Ok(EvidenceReference {
            plane: EvidencePlane::Engine,
            provider_id: head.provider_id.clone(),
            snapshot_fingerprint: head.snapshot_fingerprint.clone(),
        })
    }

    pub fn analyze(&self, base: &str) -> Result<(), AppError> {
        println!("{}", pretty_json(&analyze::evaluate(&self.root, base)?)?);
        Ok(())
    }

    pub fn index(&self) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&index::evaluate(
                &self.root,
                &self.config.project_key,
                &self.store,
            )?)?
        );
        Ok(())
    }

    pub fn index_scip(&self, provider: &str, input: &Path) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&scip_index::evaluate(
                &self.root,
                &self.config.project_key,
                &self.config.language_profiles,
                &self.config.semantic_providers,
                &self.store,
                provider,
                input,
            )?)?
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind_provider(
        &self,
        install_root: Option<&Path>,
        profile: &str,
        executable: &Path,
        script: Option<&Path>,
        replace: bool,
        trust_local_executable: bool,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&provider::bind(
                install_root,
                &self.root,
                &self.config.project_key,
                &self.config.semantic_providers,
                profile,
                executable,
                script,
                replace,
                trust_local_executable,
                timeout_seconds,
            )?)?
        );
        Ok(())
    }

    pub fn unbind_provider(
        &self,
        install_root: Option<&Path>,
        profile: &str,
    ) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&provider::unbind(
                install_root,
                &self.config.project_key,
                profile,
            )?)?
        );
        Ok(())
    }

    pub fn list_providers(&self, install_root: Option<&Path>) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&provider::list(
                install_root,
                &self.root,
                &self.config.project_key,
                &self.config.semantic_providers,
            )?)?
        );
        Ok(())
    }

    pub fn index_with_provider(
        &self,
        install_root: Option<&Path>,
        profile: &str,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        self.ensure_provider_preflight_qualification(install_root, profile)?;
        let run = provider::execute(
            install_root,
            &self.root,
            &self.config.project_key,
            &self.config.semantic_providers,
            profile,
            timeout_seconds,
        )?;
        let prepared = scip_index::prepare(
            &self.root,
            &self.config.project_key,
            &self.config.language_profiles,
            &self.config.semantic_providers,
            profile,
            run.output_path(),
        );
        let mut prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
        };
        let current_fingerprint = git::worktree_fingerprint(&self.root)?;
        if current_fingerprint != run.source_fingerprint() {
            let error = AppError::Provider(
                "源码在 Provider 输出验证后再次变化；拒绝提交 semantic snapshot".to_owned(),
            );
            provider::record_import_failure(install_root, &run, &error)?;
            return Err(error);
        }
        let qualification = match self.provider_commit_qualification(profile, &run) {
            Ok(qualification) => qualification,
            Err(error) => {
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
        };
        prepared.attest_trusted_provider(
            run.registration_id(),
            run.executable_sha256(),
            run.artifact_sha256(),
        );
        provider::record_import_prepared(install_root, &run)?;
        let imported = match scip_index::commit(&self.store, prepared) {
            Ok(imported) => imported,
            Err(error) => {
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
        };
        let _ = provider::record_import_committed(install_root, &run);
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "execution": run.report(),
                "qualification": qualification,
                "index": imported,
            }))?
        );
        Ok(())
    }

    pub fn verify_provider_stability(
        &self,
        install_root: Option<&Path>,
        profile: &str,
        runs: u8,
        timeout_seconds: u64,
    ) -> Result<(), AppError> {
        let mut observations = Vec::with_capacity(usize::from(runs));
        let mut baseline_documents: Option<BTreeSet<String>> = None;
        let mut divergent_document_sample = BTreeSet::new();
        let mut document_sets_equal = true;
        let mut semantic_snapshots_equal = true;
        let mut all_complete = true;
        let mut baseline_semantic_fingerprint: Option<String> = None;
        let mut baseline_source_fingerprint: Option<String> = None;
        let mut baseline_provider_binding: Option<(String, u64, String)> = None;

        for run_number in 1..=runs {
            let (run, prepared) =
                self.prepare_provider_stability_run(install_root, profile, timeout_seconds)?;
            let current_source = run.source_fingerprint().to_owned();
            if baseline_source_fingerprint
                .as_ref()
                .is_some_and(|baseline| baseline != &current_source)
            {
                let error =
                    AppError::Provider("稳定性验证期间源码指纹发生变化；所有观测作废".to_owned());
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
            baseline_source_fingerprint.get_or_insert(current_source);
            let current_binding = (
                run.registration_id().to_owned(),
                run.registration_revision(),
                run.executable_sha256().to_owned(),
            );
            if baseline_provider_binding
                .as_ref()
                .is_some_and(|baseline| baseline != &current_binding)
            {
                let error = AppError::Provider(
                    "稳定性验证期间 Provider 绑定、revision 或 executable SHA-256 发生变化；所有观测作废"
                        .to_owned(),
                );
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
            baseline_provider_binding.get_or_insert(current_binding);

            let evidence = prepared.stability_evidence();
            let documents = prepared.document_paths();
            if let Some(baseline) = &baseline_documents {
                if baseline != &documents {
                    document_sets_equal = false;
                    divergent_document_sample
                        .extend(baseline.symmetric_difference(&documents).take(200).cloned());
                }
            } else {
                baseline_documents = Some(documents);
            }
            if baseline_semantic_fingerprint
                .as_ref()
                .is_some_and(|baseline| baseline != &evidence.semantic_snapshot_fingerprint)
            {
                semantic_snapshots_equal = false;
            }
            baseline_semantic_fingerprint
                .get_or_insert_with(|| evidence.semantic_snapshot_fingerprint.clone());
            all_complete &= evidence.coverage_status == "complete";
            provider::record_stability_observed(install_root, &run)?;
            observations.push(serde_json::json!({
                "run": run_number,
                "execution": run.report(),
                "evidence": evidence,
            }));
        }

        let status =
            provider_stability_status(document_sets_equal, semantic_snapshots_equal, all_complete);
        let qualification = self.record_provider_stability_qualification(
            profile,
            runs,
            status,
            baseline_provider_binding,
            baseline_source_fingerprint,
            &observations,
        )?;
        let report = serde_json::json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "project_key": self.config.project_key,
            "provider_profile": profile,
            "status": status,
            "runs_requested": runs,
            "document_sets_equal": document_sets_equal,
            "semantic_snapshots_equal": semantic_snapshots_equal,
            "all_runs_coverage_complete": all_complete,
            "divergent_document_sample": divergent_document_sample,
            "snapshot_committed": false,
            "qualification": qualification,
            "observations": observations,
        });
        println!("{}", pretty_json(&report)?);
        if status == "stable_complete" {
            Ok(())
        } else {
            Err(AppError::Provider(format!(
                "Provider 稳定性验证未通过：status={status}；未提交 semantic snapshot"
            )))
        }
    }

    fn record_provider_stability_qualification(
        &self,
        profile: &str,
        runs: u8,
        status: &str,
        binding: Option<(String, u64, String)>,
        source_fingerprint: Option<String>,
        observations: &[serde_json::Value],
    ) -> Result<brain_store::ProviderQualificationRecord, AppError> {
        let binding = binding
            .ok_or_else(|| AppError::Provider("稳定性验证没有产生 Provider 绑定证据".to_owned()))?;
        let source_fingerprint = source_fingerprint
            .ok_or_else(|| AppError::Provider("稳定性验证没有产生源码指纹证据".to_owned()))?;
        let evidence_manifest = serde_json::to_vec(observations)?;
        Ok(self.store.record_provider_qualification(
            &self.config.project_key,
            profile,
            status,
            u64::from(runs),
            &binding.0,
            binding.1,
            &binding.2,
            &source_fingerprint,
            &format!("sha256_{:x}", Sha256::digest(evidence_manifest)),
        )?)
    }

    fn provider_commit_qualification(
        &self,
        profile: &str,
        run: &provider::ProviderRun,
    ) -> Result<Option<brain_store::ProviderQualificationRecord>, AppError> {
        let qualification = self
            .store
            .latest_provider_qualification(&self.config.project_key, profile)?;
        let Some(qualification) = qualification else {
            return Ok(None);
        };
        if qualification.status != "stable_complete" {
            return Err(AppError::Provider(format!(
                "Provider profile={profile} 最新稳定性资格为 {}；普通 index 不得用一次偶然输出覆盖，必须显式 verify-stability",
                qualification.status
            )));
        }
        if qualification.registration_id != run.registration_id()
            || qualification.registration_revision != run.registration_revision()
            || qualification.executable_sha256 != run.executable_sha256()
        {
            return Err(AppError::Provider(format!(
                "Provider profile={profile} 的 stable_complete 资格已因机器绑定或 executable 漂移而过期；必须重新 verify-stability"
            )));
        }
        Ok(Some(qualification))
    }

    fn ensure_provider_preflight_qualification(
        &self,
        install_root: Option<&Path>,
        profile: &str,
    ) -> Result<(), AppError> {
        let qualification = self
            .store
            .latest_provider_qualification(&self.config.project_key, profile)?;
        let Some(qualification) = qualification else {
            return Ok(());
        };
        if qualification.status != "stable_complete" {
            return Err(AppError::Provider(format!(
                "Provider profile={profile} 最新稳定性资格为 {}；在启动昂贵索引前拒绝，必须显式 verify-stability",
                qualification.status
            )));
        }
        let trust = provider::trust_status(
            install_root,
            &self.root,
            &self.config.project_key,
            &self.config.semantic_providers,
        );
        let current = trust.get(profile).ok_or_else(|| {
            AppError::Provider(format!("找不到 Provider profile={profile} 的机器绑定状态"))
        })?;
        if !current.ready
            || current.registration_id.as_deref() != Some(&qualification.registration_id)
            || current.registration_revision != Some(qualification.registration_revision)
            || current.executable_sha256.as_deref() != Some(&qualification.executable_sha256)
        {
            return Err(AppError::Provider(format!(
                "Provider profile={profile} 的 stable_complete 资格已过期；必须重新 verify-stability"
            )));
        }
        Ok(())
    }

    fn prepare_provider_stability_run(
        &self,
        install_root: Option<&Path>,
        profile: &str,
        timeout_seconds: u64,
    ) -> Result<(provider::ProviderRun, scip_index::PreparedScipIndex), AppError> {
        let run = provider::execute(
            install_root,
            &self.root,
            &self.config.project_key,
            &self.config.semantic_providers,
            profile,
            timeout_seconds,
        )?;
        let prepared = match scip_index::prepare(
            &self.root,
            &self.config.project_key,
            &self.config.language_profiles,
            &self.config.semantic_providers,
            profile,
            run.output_path(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                provider::record_import_failure(install_root, &run, &error)?;
                return Err(error);
            }
        };
        Ok((run, prepared))
    }

    pub fn provider_coverage(&self, require_indexed: bool) -> Result<(), AppError> {
        let report = scip_index::doctor_coverage(
            &self.root,
            &self.config.project_key,
            &self.config.language_profiles,
            &self.config.semantic_providers,
            &self.store,
        );
        println!("{}", pretty_json(&report)?);
        if report.issues.is_empty() && (!require_indexed || report.status == "ready") {
            Ok(())
        } else {
            let mut reasons = report.issues.clone();
            if reasons.is_empty() {
                reasons.push(format!(
                    "semantic coverage status={}，但 --require-indexed 要求 ready",
                    report.status
                ));
            }
            Err(AppError::DoctorDegraded(reasons.join("；")))
        }
    }

    pub fn symbols(
        &self,
        path: Option<&str>,
        include_removed: bool,
        limit: u32,
    ) -> Result<(), AppError> {
        let normalized_path = path.map(normalize_project_path);
        println!(
            "{}",
            pretty_json(&self.store.list_symbols(
                &self.config.project_key,
                normalized_path.as_deref(),
                include_removed,
                limit,
            )?)?
        );
        Ok(())
    }

    pub fn lineage_candidates(
        &self,
        state: Option<brain_symbols::LineageState>,
        snapshot: Option<&str>,
        ambiguity_group: Option<&str>,
        limit: u32,
    ) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "candidates": self.store.list_lineage_candidates(
                    &self.config.project_key,
                    state,
                    snapshot,
                    ambiguity_group,
                    limit,
                )?,
            }))?
        );
        Ok(())
    }

    pub fn database_stats(explicit_root: Option<PathBuf>) -> Result<(), AppError> {
        let start = explicit_root.unwrap_or(env::current_dir()?);
        let root = discover_root(&start).ok_or(AppError::ProjectNotInitialized)?;
        let brain_dir = root.join(BRAIN_DIRECTORY);
        let config: BrainConfig = serde_json::from_slice(&fs::read(brain_dir.join(CONFIG_FILE))?)?;
        config.validate()?;
        let database = brain_dir.join(DATABASE_FILE);
        let _database_lock = DatabaseAccessLock::acquire_shared(&database)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "config_schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": config.project_key,
                "database": inspect_database_storage(&database)?,
            }))?
        );
        Ok(())
    }

    pub fn database_compact(
        explicit_root: Option<PathBuf>,
        options: &DatabaseCompactOptions,
    ) -> Result<(), AppError> {
        let start = explicit_root.unwrap_or(env::current_dir()?);
        let root = discover_root(&start).ok_or(AppError::ProjectNotInitialized)?;
        let brain_dir = root.join(BRAIN_DIRECTORY);
        let config: BrainConfig = serde_json::from_slice(&fs::read(brain_dir.join(CONFIG_FILE))?)?;
        config.validate()?;
        let database = brain_dir.join(DATABASE_FILE);
        let report = if options.apply {
            database::apply_compaction(&config.project_key, &database, options)?
        } else {
            database::preview_compaction(&config.project_key, &database, options)?
        };
        println!("{}", pretty_json(&report)?);
        Ok(())
    }

    pub fn compact_legacy_lineage_proposals(
        &self,
        install_root: Option<&Path>,
        apply: bool,
        request_id: Option<&str>,
        approved_manifest_hash: Option<&str>,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        let install_root =
            setup::canonical_directory_boundary(&setup::resolve_install_root(install_root)?)?;
        let project_boundary = setup::canonical_directory_boundary(&self.root)?;
        if install_root.starts_with(&project_boundary) {
            return Err(AppError::Governance(
                "lineage 删除前备份必须位于项目工作树之外的机器级数据目录".to_owned(),
            ));
        }
        let backup_root = install_root.join("state/backups/lineage-compaction");
        let report = if apply {
            require_human_confirmation(human_confirmed, "compact legacy lineage proposals")?;
            let request_id = request_id.ok_or_else(|| {
                AppError::Governance(
                    "--apply 必须同时提供非空 --request-id 以保证幂等审计".to_owned(),
                )
            })?;
            let approved_manifest_hash = approved_manifest_hash.ok_or_else(|| {
                AppError::Governance(
                    "--apply 必须提供 dry-run 输出的 --approved-manifest-hash；计划变化时拒绝删除"
                        .to_owned(),
                )
            })?;
            self.store.apply_legacy_lineage_compaction(
                &self.config.project_key,
                request_id,
                approved_manifest_hash,
                &backup_root,
            )?
        } else {
            if request_id.is_some() || approved_manifest_hash.is_some() || human_confirmed {
                return Err(AppError::Governance(
                    "dry-run 不接受 --request-id、--approved-manifest-hash 或 --human-confirmed；只有 --apply 会写数据库".to_owned(),
                ));
            }
            self.store
                .preview_legacy_lineage_compaction(&self.config.project_key, &backup_root)?
        };
        println!("{}", pretty_json(&report)?);
        Ok(())
    }

    pub fn lineage_groups(&self, limit: u32) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "groups": self.store.list_lineage_groups(&self.config.project_key, limit)?,
            }))?
        );
        Ok(())
    }

    pub fn lineage_group(&self, group_id: &str) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "detail": self.store.lineage_group(&self.config.project_key, group_id)?,
            }))?
        );
        Ok(())
    }

    pub fn materialize_lineage_group_pair(
        &self,
        group_id: &str,
        from_symbol_id: &str,
        to_symbol_id: &str,
        request_id: &str,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        require_human_confirmation(human_confirmed, "materialize lineage group pair")?;
        println!(
            "{}",
            pretty_json(&self.store.materialize_lineage_group_pair(
                &self.config.project_key,
                group_id,
                from_symbol_id,
                to_symbol_id,
                request_id,
            )?)?
        );
        Ok(())
    }

    pub fn confirm_lineage(
        &self,
        candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
        supersede_candidate_id: Option<&str>,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        require_human_confirmation(human_confirmed, "confirm lineage")?;
        println!(
            "{}",
            pretty_json(&self.store.confirm_lineage(
                &self.config.project_key,
                candidate_id,
                request_id,
                actor_ref,
                reason,
                supersede_candidate_id,
            )?)?
        );
        Ok(())
    }

    pub fn reject_lineage(
        &self,
        candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        require_human_confirmation(human_confirmed, "reject lineage")?;
        println!(
            "{}",
            pretty_json(&self.store.reject_lineage(
                &self.config.project_key,
                candidate_id,
                request_id,
                actor_ref,
                reason,
            )?)?
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind_rule_symbol(
        &self,
        rule_id: &str,
        provider_profile_id: &str,
        provider_contract_id: &str,
        language_id: &str,
        snapshot: &str,
        symbol: &str,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        require_human_confirmation(human_confirmed, "bind rule symbol")?;
        let resolution = self.store.resolve_semantic_scope(
            &self.config.project_key,
            provider_profile_id,
            provider_contract_id,
            language_id,
            snapshot,
            symbol,
        )?;
        if resolution.kind == SemanticResolutionKind::Unresolved {
            return Err(AppError::Governance(format!(
                "拒绝绑定不可解析的 semantic 锚点：{}",
                resolution.reason.as_deref().unwrap_or("unknown")
            )));
        }
        let scope = RuleSymbolScope {
            provider_profile_id: provider_profile_id.to_owned(),
            provider_contract_id: provider_contract_id.to_owned(),
            language_id: language_id.trim().to_ascii_lowercase(),
            anchor_snapshot_fingerprint: snapshot.to_owned(),
            anchor_symbol_id: symbol.to_owned(),
            resolution_policy: SymbolResolutionPolicy::ConfirmedLineageOnly,
        };
        let mut config = self.config.clone();
        let rule = config
            .rules
            .iter_mut()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| AppError::Governance(format!("找不到 rule={rule_id:?}")))?;
        if rule.symbol_scopes.contains(&scope) {
            return Err(AppError::Governance("symbol scope 已存在".to_owned()));
        }
        rule.symbol_scopes.push(scope);
        rule.symbol_scopes.sort();
        self.write_config(&config)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "rule_id": rule_id,
                "resolution": resolution,
                "updated": true,
                "hard_gate_status": "reindex_required_after_final_config_and_head",
            }))?
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn unbind_rule_symbol(
        &self,
        rule_id: &str,
        provider_profile_id: &str,
        provider_contract_id: &str,
        language_id: &str,
        snapshot: &str,
        symbol: &str,
        human_confirmed: bool,
    ) -> Result<(), AppError> {
        require_human_confirmation(human_confirmed, "unbind rule symbol")?;
        let mut config = self.config.clone();
        let rule = config
            .rules
            .iter_mut()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| AppError::Governance(format!("找不到 rule={rule_id:?}")))?;
        let before = rule.symbol_scopes.len();
        rule.symbol_scopes.retain(|scope| {
            !(scope.provider_profile_id == provider_profile_id
                && scope.provider_contract_id == provider_contract_id
                && scope.language_id.eq_ignore_ascii_case(language_id)
                && scope.anchor_snapshot_fingerprint == snapshot
                && scope.anchor_symbol_id == symbol)
        });
        if rule.symbol_scopes.len() == before {
            return Err(AppError::Governance(
                "找不到精确匹配的 symbol scope".to_owned(),
            ));
        }
        self.write_config(&config)?;
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "rule_id": rule_id,
                "updated": true,
            }))?
        );
        Ok(())
    }

    pub fn rule_symbol_scopes(&self, rule_filter: Option<&str>) -> Result<(), AppError> {
        let mut scopes = Vec::new();
        for rule in &self.config.rules {
            if rule_filter.is_some_and(|filter| filter != rule.id) {
                continue;
            }
            for scope in &rule.symbol_scopes {
                let resolution = self.store.resolve_semantic_scope(
                    &self.config.project_key,
                    &scope.provider_profile_id,
                    &scope.provider_contract_id,
                    &scope.language_id,
                    &scope.anchor_snapshot_fingerprint,
                    &scope.anchor_symbol_id,
                )?;
                scopes.push(serde_json::json!({
                    "rule_id": rule.id,
                    "scope": scope,
                    "resolution": resolution,
                }));
            }
        }
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "project_key": self.config.project_key,
                "symbol_scopes": scopes,
            }))?
        );
        Ok(())
    }

    fn write_config(&self, config: &BrainConfig) -> Result<(), AppError> {
        config.validate()?;
        let path = self.root.join(BRAIN_DIRECTORY).join(CONFIG_FILE);
        let before = fs::read(&path)?;
        let before_hash = format!("{:x}", Sha256::digest(&before));
        setup::atomic_replace(&path, pretty_json(&config)?.as_bytes(), Some(&before_hash))
    }

    pub fn audit(&self, limit: u32) -> Result<(), AppError> {
        println!(
            "{}",
            pretty_json(&serde_json::json!({
                "project_key": self.config.project_key,
                "adapter_events": self
                    .store
                    .recent_adapter_audit(&self.config.project_key, limit)?,
                "legacy_actions": self.store.recent_audit(limit)?,
            }))?
        );
        Ok(())
    }
}

fn require_human_confirmation(confirmed: bool, operation: &str) -> Result<(), AppError> {
    if confirmed {
        Ok(())
    } else {
        Err(AppError::Governance(format!(
            "{operation} 改变人工治理事实，必须由操作者显式提供 --human-confirmed"
        )))
    }
}

fn provider_stability_status(
    document_sets_equal: bool,
    semantic_snapshots_equal: bool,
    all_complete: bool,
) -> &'static str {
    if document_sets_equal && semantic_snapshots_equal && all_complete {
        "stable_complete"
    } else if !document_sets_equal || !semantic_snapshots_equal {
        "nondeterministic"
    } else {
        "stable_incomplete"
    }
}

fn initial_config(
    project_name: String,
    project_key: String,
    profiles: &[ProjectProfile],
) -> BrainConfig {
    let (language_profiles, semantic_providers) = profile_config(profiles);
    BrainConfig {
        schema_version: CURRENT_SCHEMA_VERSION,
        project_key,
        project_name,
        language_profiles,
        semantic_providers,
        finding_effect_mappings: Vec::new(),
        stop_reconcile: StopReconcileConfig {
            enabled: true,
            base: "HEAD".to_owned(),
            envelope: format!("{BRAIN_DIRECTORY}/envelope.json"),
        },
        rules: vec![
            Rule {
                id: "PB-CORE-001".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::Block,
                include_paths: vec![format!("{BRAIN_DIRECTORY}/{CONFIG_FILE}")],
                exclude_paths: Vec::new(),
                actions: vec![brain_core::ActionKind::Delete],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: Vec::new(),
                message: "禁止通过普通删除操作移除 Project Brain 的项目规则配置".to_owned(),
                rationale: "规则控制面必须通过显式的规则修订流程变更".to_owned(),
            },
            Rule {
                id: "PB-CORE-002".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Soft,
                effect: RuleEffect::InjectContext,
                include_paths: vec![BRAIN_DIRECTORY.to_owned()],
                exclude_paths: vec![format!("{BRAIN_DIRECTORY}/brain.db")],
                actions: vec![
                    brain_core::ActionKind::Create,
                    brain_core::ActionKind::Modify,
                ],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: Vec::new(),
                message:
                    "正在修改项目决策控制面；请保持 schema_version、authority 与 lifecycle 语义兼容"
                        .to_owned(),
                rationale: "控制面规则变化会影响后续所有 Agent 行为".to_owned(),
            },
            Rule {
                id: "PB-CORE-003".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::Block,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                actions: vec![brain_core::ActionKind::Delete],
                operations: vec!["Bash".to_owned()],
                operation_contains: vec![
                    ".project-brain/config.json".to_owned(),
                    ".project-brain\\config.json".to_owned(),
                ],
                symbol_scopes: Vec::new(),
                message: "禁止通过 shell 删除 Project Brain 的项目规则配置".to_owned(),
                rationale: "无结构化路径参数的 shell 删除需要单独匹配命令载荷".to_owned(),
            },
        ],
    }
}

fn profile_config(
    profiles: &[ProjectProfile],
) -> (Vec<ProjectLanguageProfile>, Vec<SemanticProviderProfile>) {
    let profiles = profiles
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut languages = Vec::new();
    let mut providers = Vec::new();

    for profile in profiles {
        match profile {
            ProjectProfile::Rust => {
                languages.push(ProjectLanguageProfile {
                    language: "rust".to_owned(),
                    roots: Vec::new(),
                });
                providers.push(SemanticProviderProfile {
                    id: "rust-main".to_owned(),
                    format: SemanticProviderFormat::Scip,
                    producer: "rust-analyzer".to_owned(),
                    contract_version: 1,
                    language_mappings: vec![SemanticLanguageMapping {
                        raw_language: Some("rust".to_owned()),
                        language: "rust".to_owned(),
                        allow_missing_language: false,
                    }],
                });
            }
            ProjectProfile::Dotnet => {
                languages.extend([
                    ProjectLanguageProfile {
                        language: "csharp".to_owned(),
                        roots: Vec::new(),
                    },
                    ProjectLanguageProfile {
                        language: "visual-basic".to_owned(),
                        roots: Vec::new(),
                    },
                ]);
                providers.push(SemanticProviderProfile {
                    id: "dotnet-main".to_owned(),
                    format: SemanticProviderFormat::Scip,
                    producer: "scip-dotnet".to_owned(),
                    contract_version: 1,
                    language_mappings: vec![
                        SemanticLanguageMapping {
                            raw_language: Some("C#".to_owned()),
                            language: "csharp".to_owned(),
                            allow_missing_language: false,
                        },
                        SemanticLanguageMapping {
                            raw_language: Some("Visual Basic".to_owned()),
                            language: "visual-basic".to_owned(),
                            allow_missing_language: false,
                        },
                    ],
                });
            }
            ProjectProfile::Python => {
                languages.push(ProjectLanguageProfile {
                    language: "python".to_owned(),
                    roots: Vec::new(),
                });
                providers.push(SemanticProviderProfile {
                    id: "python-main".to_owned(),
                    format: SemanticProviderFormat::Scip,
                    producer: "scip-python".to_owned(),
                    contract_version: 1,
                    language_mappings: vec![SemanticLanguageMapping {
                        raw_language: None,
                        language: "python".to_owned(),
                        allow_missing_language: true,
                    }],
                });
            }
        }
    }

    (languages, providers)
}

fn generate_project_key(root: &Path) -> Result<String, std::time::SystemTimeError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let mut digest = Sha256::new();
    digest.update(b"project-brain/project-key/v1\0");
    digest.update(root.as_os_str().to_string_lossy().as_bytes());
    digest.update(nonce.to_le_bytes());
    digest.update(std::process::id().to_le_bytes());
    let encoded = format!("{:x}", digest.finalize());
    Ok(format!("pb_{}", &encoded[..32]))
}

fn legacy_project_key(config: &BrainConfig) -> Result<String, serde_json::Error> {
    let mut digest = Sha256::new();
    digest.update(b"project-brain/legacy-project-key/v1\0");
    digest.update(serde_json::to_vec(config)?);
    let encoded = format!("{:x}", digest.finalize());
    Ok(format!("pb_{}", &encoded[..32]))
}

fn discover_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        candidate
            .join(BRAIN_DIRECTORY)
            .join(CONFIG_FILE)
            .is_file()
            .then(|| candidate.to_owned())
    })
}

fn read_stdin_json<T>() -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
{
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    Ok(serde_json::from_str(&input)?)
}

fn read_stdin_json_limited<T>(limit: u64) -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
{
    let mut input = String::new();
    io::stdin().take(limit + 1).read_to_string(&mut input)?;
    if input.len() as u64 > limit {
        return Err(AppError::Setup(format!(
            "Hook 输入超过最大允许字节数：{limit}"
        )));
    }
    Ok(serde_json::from_str(&input)?)
}

fn pretty_json<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut output = serde_json::to_string_pretty(value)?;
    output.push('\n');
    Ok(output)
}

pub fn decision_reason(decision: &brain_core::Decision) -> String {
    if decision.evidence.is_empty() {
        return decision.summary.clone();
    }
    decision
        .evidence
        .iter()
        .map(|evidence| format!("{}: {}", evidence.rule_id, evidence.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        App, ProjectProfile, discover_root, initial_config, legacy_project_key, pretty_json,
        profile_config,
    };

    #[test]
    fn missing_marker_does_not_guess_a_project_root() {
        assert_eq!(
            discover_root(Path::new("Z:/definitely/not/a/project")),
            None
        );
    }

    #[test]
    fn init_creates_a_reopenable_project_without_tracking_the_database() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-init-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();

        App::init(Some(root.clone()), &[]).unwrap();

        assert!(root.join(".project-brain/config.json").is_file());
        assert!(root.join(".project-brain/envelope.json").is_file());
        assert!(root.join(".project-brain/brain.db").is_file());
        let envelope: crate::reconcile::ChangeEnvelope =
            serde_json::from_slice(&fs::read(root.join(".project-brain/envelope.json")).unwrap())
                .unwrap();
        assert_eq!(envelope.allowed_paths, ["."]);
        assert_eq!(envelope.forbidden_paths, [".git"]);
        let ignore = fs::read_to_string(root.join(".project-brain/.gitignore")).unwrap();
        assert!(ignore.lines().any(|line| line == "brain.db*"));
        assert!(ignore.lines().any(|line| line == ".brain.db*"));
        let app = App::open(Some(root.clone())).unwrap();
        assert!(app.config.project_key.starts_with("pb_"));
        drop(app);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_a_legacy_config_persists_a_project_key() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-config-migration-test-{}-{nonce}",
            std::process::id()
        ));
        let brain_dir = root.join(".project-brain");
        fs::create_dir_all(&brain_dir).unwrap();
        let legacy = initial_config("legacy".to_owned(), String::new(), &[]);
        fs::write(brain_dir.join("config.json"), pretty_json(&legacy).unwrap()).unwrap();

        let app = App::open(Some(root.clone())).unwrap();
        let generated = app.config.project_key.clone();
        drop(app);
        let persisted: brain_core::BrainConfig =
            serde_json::from_slice(&fs::read(brain_dir.join("config.json")).unwrap()).unwrap();
        assert_eq!(persisted.project_key, generated);
        assert!(generated.starts_with("pb_"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_project_key_depends_on_project_config_not_checkout_path() {
        let first = initial_config("same".to_owned(), String::new(), &[]);
        let second = initial_config("other".to_owned(), String::new(), &[]);
        assert_eq!(
            legacy_project_key(&first).unwrap(),
            legacy_project_key(&first).unwrap()
        );
        assert_ne!(
            legacy_project_key(&first).unwrap(),
            legacy_project_key(&second).unwrap()
        );
    }

    #[test]
    fn explicit_profiles_are_composable_deduplicated_and_never_inferred() {
        let empty = initial_config("empty".to_owned(), "pb_empty".to_owned(), &[]);
        assert!(empty.language_profiles.is_empty());
        assert!(empty.semantic_providers.is_empty());

        let (languages, providers) = profile_config(&[
            ProjectProfile::Python,
            ProjectProfile::Rust,
            ProjectProfile::Dotnet,
            ProjectProfile::Rust,
        ]);
        assert_eq!(
            languages
                .iter()
                .map(|profile| profile.language.as_str())
                .collect::<Vec<_>>(),
            ["rust", "csharp", "visual-basic", "python"]
        );
        assert_eq!(
            providers
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
            ["rust-main", "dotnet-main", "python-main"]
        );

        let configured = initial_config(
            "polyglot".to_owned(),
            "pb_polyglot".to_owned(),
            &[
                ProjectProfile::Rust,
                ProjectProfile::Dotnet,
                ProjectProfile::Python,
            ],
        );
        configured.validate().unwrap();
    }
}
