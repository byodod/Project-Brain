use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use brain_core::{
    ActionDescriptor, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, MemoryStatus,
    ProjectLanguageProfile, Rule, RuleEffect, RuleEngine, RuleStrength, SemanticLanguageMapping,
    SemanticProviderFormat, SemanticProviderProfile, StopReconcileConfig, normalize_project_path,
};
use brain_store::BrainStore;
use clap::ValueEnum;
use sha2::{Digest, Sha256};

use crate::{
    analyze,
    codex::{self, CodexHookInput},
    error::AppError,
    index, reconcile, scip_index,
};

const BRAIN_DIRECTORY: &str = ".project-brain";
const CONFIG_FILE: &str = "config.json";
const DATABASE_FILE: &str = "brain.db";

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
                let output = codex::handle(&app.root, &app.config, &app.store, event, &input)?;
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

    pub fn confirm_lineage(
        &self,
        candidate_id: &str,
        request_id: &str,
        actor_ref: Option<&str>,
        reason: Option<&str>,
        supersede_candidate_id: Option<&str>,
    ) -> Result<(), AppError> {
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
    ) -> Result<(), AppError> {
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
