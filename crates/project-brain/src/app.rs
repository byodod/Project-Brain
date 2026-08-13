use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use brain_core::{
    ActionDescriptor, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, MemoryStatus,
    ProjectLanguageProfile, Rule, RuleEffect, RuleEngine, RuleStrength, RuleSymbolScope,
    SemanticLanguageMapping, SemanticProviderFormat, SemanticProviderProfile, StopReconcileConfig,
    SymbolResolutionPolicy, normalize_project_path,
};
use brain_store::{BrainStore, SemanticResolutionKind};
use clap::ValueEnum;
use sha2::{Digest, Sha256};

use crate::{
    analyze,
    codex::{self, CodexHookInput},
    error::AppError,
    git, index, provider, reconcile, scip_index, setup,
};

const BRAIN_DIRECTORY: &str = ".project-brain";
const CONFIG_FILE: &str = "config.json";
const DATABASE_FILE: &str = "brain.db";
const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AgentKind {
    Codex,
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
        codex_home: Option<&Path>,
        agent: AgentKind,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                println!(
                    "{}",
                    pretty_json(&setup::install_codex_hooks(install_root, codex_home)?)?
                );
            }
        }
        Ok(())
    }

    pub fn uninstall_hooks(
        install_root: Option<&Path>,
        codex_home: Option<&Path>,
        agent: AgentKind,
        force: bool,
    ) -> Result<(), AppError> {
        match agent {
            AgentKind::Codex => {
                println!(
                    "{}",
                    pretty_json(&setup::uninstall_codex_hooks(
                        install_root,
                        codex_home,
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
        codex_home: Option<&Path>,
    ) -> Result<(), AppError> {
        let mut report = setup::doctor(
            install_root,
            codex_home,
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
        println!("{}", pretty_json(&report)?);
        if report.is_ready() {
            Ok(())
        } else {
            Err(AppError::DoctorDegraded(report.issues.join("；")))
        }
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
        }
    }

    pub fn capabilities(agent: AgentKind) -> Result<(), AppError> {
        let capabilities = match agent {
            AgentKind::Codex => codex::capabilities(),
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
        fs::write(
            brain_dir.join(".gitignore"),
            "brain.db\nbrain.db-shm\nbrain.db-wal\n",
        )?;
        fs::write(
            brain_dir.join("envelope.json"),
            pretty_json(&reconcile::ChangeEnvelope::example())?,
        )?;
        BrainStore::open(&brain_dir.join(DATABASE_FILE))?;

        println!("Project Brain 已初始化：{}", brain_dir.display());
        Ok(())
    }

    pub fn open(explicit_root: Option<PathBuf>) -> Result<Self, AppError> {
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
        let store = BrainStore::open(&brain_dir.join(DATABASE_FILE))?;
        Ok(Self {
            root,
            config,
            store,
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
        }
    }

    pub fn reconcile(&self, base: &str, envelope: &Path) -> Result<(), AppError> {
        let report = reconcile::evaluate_from_path(&self.root, base, envelope)?;
        println!("{}", pretty_json(&report)?);
        Ok(())
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

        let status = if document_sets_equal && semantic_snapshots_equal && all_complete {
            "stable_complete"
        } else if !document_sets_equal || !semantic_snapshots_equal {
            "nondeterministic"
        } else {
            "stable_incomplete"
        };
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
        assert!(ignore.lines().any(|line| line == "brain.db"));
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
