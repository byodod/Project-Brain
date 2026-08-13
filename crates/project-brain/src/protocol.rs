use std::{collections::BTreeMap, path::Path};

use brain_core::{
    ActionDescriptor, BrainConfig, CURRENT_SCHEMA_VERSION, ContextItem, Decision, DecisionKind,
    Evidence, EvidenceGrade, FeedbackItem, FeedbackSeverity, GateDecision, HOOK_PROTOCOL_VERSION,
    HookEventPayload, HookOutcomePayload, InternalHookEvent, InternalHookOutcome, MemoryStatus,
    Rule, RuleEffect, RuleEngine, StopDecision, ToolAction, path_has_prefix,
};
use brain_store::{BrainStore, SemanticResolutionKind, SemanticSourceTrust};

use crate::provider::ProviderTrustStatus;
use crate::{app::decision_reason, error::AppError, reconcile};

pub fn process(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: &InternalHookEvent,
) -> Result<InternalHookOutcome, AppError> {
    event.validate()?;
    let payload = match &event.payload {
        HookEventPayload::SessionOpened(_) => HookOutcomePayload::SessionOpened {
            inject: vec![ContextItem {
                text: session_context(config),
            }],
        },
        HookEventPayload::IntentDeclared(_) => HookOutcomePayload::IntentDeclared {
            gate: GateDecision::NoVeto,
            inject: Vec::new(),
        },
        HookEventPayload::ToolAboutToRun(tool) => {
            let decision = evaluate_action(
                root,
                config,
                store,
                provider_trust,
                event,
                &tool.action,
                &tool.tool_name,
            )?;
            HookOutcomePayload::ToolAboutToRun {
                gate: gate_from_decision(&decision),
                inject: context_from_decision(&decision),
            }
        }
        HookEventPayload::ToolFinished(tool) => {
            let decision = evaluate_action(
                root,
                config,
                store,
                provider_trust,
                event,
                &tool.action,
                &tool.tool_name,
            )?;
            HookOutcomePayload::ToolFinished {
                feedback: feedback_from_decision(&decision),
            }
        }
        HookEventPayload::TaskStopping(stopping) => {
            let (stop, feedback) = stop_decision(
                root,
                config,
                store,
                provider_trust,
                stopping.vendor_loop_active,
            );
            HookOutcomePayload::TaskStopping { stop, feedback }
        }
    };
    Ok(InternalHookOutcome {
        protocol_version: HOOK_PROTOCOL_VERSION,
        event_id: event.event_id.clone(),
        payload,
    })
}

fn evaluate_action(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: &InternalHookEvent,
    action: &ToolAction,
    tool_name: &str,
) -> Result<Decision, AppError> {
    let descriptor = ActionDescriptor {
        schema_version: CURRENT_SCHEMA_VERSION,
        event_id: event.event_id.clone(),
        session_id: event.session_key.clone(),
        cwd: event.cwd.clone(),
        action: action.kind,
        operation: tool_name.to_owned(),
        target_files: action.target_files.clone(),
        command: action.command.clone(),
        metadata: BTreeMap::new(),
    };
    let path_decision = RuleEngine::new(config)?.evaluate(&descriptor)?;
    let semantic = evaluate_symbol_rules(root, config, store, provider_trust, &descriptor, action);
    Ok(merge_decisions(path_decision, semantic))
}

#[allow(
    clippy::too_many_lines,
    reason = "符号 gate 显式保留 resolver、机器 trust、freshness 与 deterministic impact 的降级矩阵"
)]
fn evaluate_symbol_rules(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    descriptor: &ActionDescriptor,
    action: &ToolAction,
) -> Decision {
    let source_state = crate::git::worktree_fingerprint(root)
        .and_then(|fingerprint| crate::git::head_revision(root).map(|head| (fingerprint, head)))
        .ok();
    let mut evidence = Vec::new();
    let mut context = Vec::new();
    let mut hard_effects = Vec::new();
    for rule in config
        .rules
        .iter()
        .filter(|rule| rule.status == MemoryStatus::Active && !rule.symbol_scopes.is_empty())
        .filter(|rule| matches_non_symbol_constraints(rule, descriptor))
    {
        for scope in &rule.symbol_scopes {
            let resolution = match store.resolve_semantic_scope(
                &config.project_key,
                &scope.provider_profile_id,
                &scope.provider_contract_id,
                &scope.language_id,
                &scope.anchor_snapshot_fingerprint,
                &scope.anchor_symbol_id,
            ) {
                Ok(resolution) => resolution,
                Err(error) => {
                    let warning = format!(
                        "Project Brain advisory：规则 {} 的 semantic resolver 不可用（{}）；基础设施失败按 fail-open 处理，本次不执行符号硬阻断。",
                        rule.id, error
                    );
                    context.push(warning.clone());
                    evidence.push(Evidence {
                        rule_id: rule.id.clone(),
                        effect: RuleEffect::InjectContext,
                        message: warning,
                        rationale: rule.rationale.clone(),
                        grade: Some(EvidenceGrade::Unavailable),
                        symbol_id: None,
                        snapshot_fingerprint: None,
                    });
                    continue;
                }
            };
            let Some(symbol) = resolution.resolved_symbol.as_ref() else {
                continue;
            };
            if !descriptor
                .target_files
                .iter()
                .any(|path| path == &symbol.path)
            {
                continue;
            }
            let fresh = resolution.source.as_ref().is_some_and(|source| {
                source.trust == SemanticSourceTrust::TrustedProvider
                    && provider_trust
                        .get(&scope.provider_profile_id)
                        .is_some_and(|status| {
                            status.ready
                                && status.registration_id == source.provider_registration_id
                                && status.executable_sha256 == source.executable_sha256
                        })
                    && source_state.as_ref().is_some_and(|(fingerprint, head)| {
                        source.worktree_fingerprint == *fingerprint && source.head_revision == *head
                    })
            });
            let deterministic = action.deterministic_impacts.iter().any(|impact| {
                impact.path == symbol.path
                    && (impact.whole_file
                        || impact.ranges.iter().any(|range| {
                            range.start_line <= symbol.end_line
                                && symbol.start_line <= range.end_line
                        }))
            });
            let grade = match resolution.kind {
                SemanticResolutionKind::DirectSemantic => EvidenceGrade::SemanticDirect,
                SemanticResolutionKind::ConfirmedLineage => EvidenceGrade::SemanticConfirmedLineage,
                SemanticResolutionKind::Unresolved => EvidenceGrade::Unavailable,
            };
            if fresh && deterministic {
                evidence.push(Evidence {
                    rule_id: rule.id.clone(),
                    effect: rule.effect,
                    message: rule.message.clone(),
                    rationale: rule.rationale.clone(),
                    grade: Some(grade),
                    symbol_id: Some(symbol.id.clone()),
                    snapshot_fingerprint: resolution.latest_snapshot_fingerprint.clone(),
                });
                hard_effects.push(rule.effect);
                if rule.effect != RuleEffect::Block {
                    context.push(rule.message.clone());
                }
            } else {
                let reason = if resolution
                    .source
                    .as_ref()
                    .is_none_or(|source| source.trust != SemanticSourceTrust::TrustedProvider)
                {
                    "semantic snapshot 缺少 trusted Provider 证明"
                } else if !fresh {
                    "semantic snapshot 已过期"
                } else {
                    "工具影响范围无法确定"
                };
                let warning = format!(
                    "Project Brain advisory：规则 {} 可能涉及符号 {}，但{}；本次不执行硬阻断。{}",
                    rule.id, symbol.display_name, reason, rule.message
                );
                context.push(warning.clone());
                evidence.push(Evidence {
                    rule_id: rule.id.clone(),
                    effect: RuleEffect::InjectContext,
                    message: warning,
                    rationale: rule.rationale.clone(),
                    grade: Some(EvidenceGrade::Unavailable),
                    symbol_id: Some(symbol.id.clone()),
                    snapshot_fingerprint: resolution.latest_snapshot_fingerprint.clone(),
                });
            }
        }
    }
    let decision = if hard_effects.contains(&RuleEffect::Block) {
        DecisionKind::Block
    } else if hard_effects.contains(&RuleEffect::Escalate) {
        DecisionKind::Escalate
    } else if !context.is_empty() {
        DecisionKind::AllowWithContext
    } else {
        DecisionKind::Allow
    };
    Decision {
        schema_version: CURRENT_SCHEMA_VERSION,
        decision,
        summary: "semantic symbol scope 已按证据等级评估".to_owned(),
        context,
        evidence,
    }
}

fn matches_non_symbol_constraints(rule: &Rule, action: &ActionDescriptor) -> bool {
    if !rule.actions.is_empty() && !rule.actions.contains(&action.action) {
        return false;
    }
    if !rule.operations.is_empty()
        && !rule
            .operations
            .iter()
            .any(|operation| operation.eq_ignore_ascii_case(&action.operation))
    {
        return false;
    }
    let searchable = format!(
        "{} {}",
        action.operation,
        action.command.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if !rule.operation_contains.is_empty()
        && !rule
            .operation_contains
            .iter()
            .any(|needle| searchable.contains(&needle.to_ascii_lowercase()))
    {
        return false;
    }
    let include = rule.include_paths.is_empty()
        || action.target_files.iter().any(|target| {
            rule.include_paths
                .iter()
                .any(|prefix| path_has_prefix(target, prefix))
        });
    include
        && !action.target_files.iter().any(|target| {
            rule.exclude_paths
                .iter()
                .any(|prefix| path_has_prefix(target, prefix))
        })
}

fn merge_decisions(mut left: Decision, right: Decision) -> Decision {
    left.context.extend(right.context);
    left.evidence.extend(right.evidence);
    left.decision = [left.decision, right.decision]
        .into_iter()
        .max_by_key(|decision| match decision {
            DecisionKind::Allow => 0,
            DecisionKind::AllowWithContext => 1,
            DecisionKind::Escalate => 2,
            DecisionKind::Block => 3,
        })
        .unwrap_or(DecisionKind::Allow);
    match left.decision {
        DecisionKind::Allow => "未命中需要改变行为的规则",
        DecisionKind::AllowWithContext => "允许执行，并注入相关项目约束",
        DecisionKind::Escalate => "需要显式决策后再继续",
        DecisionKind::Block => "命中具备确定性证据的硬规则，拒绝执行",
    }
    .clone_into(&mut left.summary);
    left
}

fn gate_from_decision(decision: &Decision) -> GateDecision {
    if matches!(
        decision.decision,
        DecisionKind::Block | DecisionKind::Escalate
    ) {
        GateDecision::Deny {
            reason: decision_reason(decision),
        }
    } else {
        GateDecision::NoVeto
    }
}

fn context_from_decision(decision: &Decision) -> Vec<ContextItem> {
    if decision.decision == DecisionKind::AllowWithContext {
        vec![ContextItem {
            text: decision_reason(decision),
        }]
    } else {
        Vec::new()
    }
}

fn feedback_from_decision(decision: &Decision) -> Vec<FeedbackItem> {
    match decision.decision {
        DecisionKind::Allow => Vec::new(),
        DecisionKind::AllowWithContext => vec![FeedbackItem {
            severity: FeedbackSeverity::Info,
            text: decision_reason(decision),
        }],
        DecisionKind::Block | DecisionKind::Escalate => vec![FeedbackItem {
            severity: FeedbackSeverity::Error,
            text: format!(
                "操作已经执行，无法撤销其副作用；Project Brain 检测到：{}",
                decision_reason(decision)
            ),
        }],
    }
}

fn stop_decision(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    vendor_loop_active: bool,
) -> (StopDecision, Vec<FeedbackItem>) {
    if vendor_loop_active {
        return (StopDecision::AllowStop, Vec::new());
    }
    if !config.stop_reconcile.enabled {
        let (violations, feedback) = evaluate_symbol_stop(root, config, store, provider_trust);
        return if violations.is_empty() {
            (StopDecision::AllowStop, feedback)
        } else {
            (
                StopDecision::ContinueWork {
                    reason: format!(
                        "Project Brain Stop 符号对账未通过：{}",
                        violations.join("；")
                    ),
                },
                feedback,
            )
        };
    }
    let report = match reconcile::evaluate_from_path(
        root,
        &config.stop_reconcile.base,
        Path::new(&config.stop_reconcile.envelope),
    ) {
        Ok(report) => report,
        Err(error) => {
            return (
                StopDecision::ContinueWork {
                    reason: format!("Project Brain Stop 对账失败：{error}"),
                },
                Vec::new(),
            );
        }
    };
    match report.decision {
        reconcile::ReconcileDecision::Allow => {
            let (violations, feedback) = evaluate_symbol_stop(root, config, store, provider_trust);
            if violations.is_empty() {
                (StopDecision::AllowStop, feedback)
            } else {
                (
                    StopDecision::ContinueWork {
                        reason: format!(
                            "Project Brain Stop 符号对账未通过：{}",
                            violations.join("；")
                        ),
                    },
                    feedback,
                )
            }
        }
        reconcile::ReconcileDecision::Block | reconcile::ReconcileDecision::Escalate => {
            let details = if report.forbidden_files.is_empty() {
                &report.unexpected_files
            } else {
                &report.forbidden_files
            };
            (
                StopDecision::ContinueWork {
                    reason: format!(
                        "Project Brain Stop 对账未通过：{}。涉及：{}",
                        report.summary,
                        details.join(", ")
                    ),
                },
                Vec::new(),
            )
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Stop gate 线性呈现基线资格、Provider trust、Git hunk 和 fail-open 反馈"
)]
fn evaluate_symbol_stop(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
) -> (Vec<String>, Vec<FeedbackItem>) {
    let head = crate::git::head_revision(root).ok();
    let mut violations = Vec::new();
    let mut feedback = Vec::new();
    for rule in config.rules.iter().filter(|rule| {
        rule.status == MemoryStatus::Active
            && !rule.symbol_scopes.is_empty()
            && matches!(rule.effect, RuleEffect::Block | RuleEffect::Escalate)
    }) {
        for scope in &rule.symbol_scopes {
            let resolution = match store.resolve_semantic_scope(
                &config.project_key,
                &scope.provider_profile_id,
                &scope.provider_contract_id,
                &scope.language_id,
                &scope.anchor_snapshot_fingerprint,
                &scope.anchor_symbol_id,
            ) {
                Ok(resolution) => resolution,
                Err(error) => {
                    feedback.push(FeedbackItem {
                        severity: FeedbackSeverity::Warning,
                        text: format!(
                            "规则 {} 的 semantic resolver 不可用（{}）；Stop 按 fail-open 处理该符号。",
                            rule.id, error
                        ),
                    });
                    continue;
                }
            };
            let Some(symbol) = resolution.resolved_symbol.as_ref() else {
                feedback.push(FeedbackItem {
                    severity: FeedbackSeverity::Warning,
                    text: format!(
                        "规则 {} 的符号锚点当前 unresolved；Stop 不以推断结果阻断。",
                        rule.id
                    ),
                });
                continue;
            };
            let baseline_eligible = config.stop_reconcile.base == "HEAD"
                && resolution.source.as_ref().is_some_and(|source| {
                    source.trust == SemanticSourceTrust::TrustedProvider
                        && provider_trust
                            .get(&scope.provider_profile_id)
                            .is_some_and(|status| {
                                status.ready
                                    && status.registration_id == source.provider_registration_id
                                    && status.executable_sha256 == source.executable_sha256
                            })
                        && source.worktree_clean
                        && head
                            .as_ref()
                            .is_some_and(|head| source.head_revision == *head)
                });
            if !baseline_eligible {
                feedback.push(FeedbackItem {
                    severity: FeedbackSeverity::Warning,
                    text: format!(
                        "规则 {} 的 semantic snapshot 不是当前干净 HEAD 基线；Stop 按 advisory 处理符号 {}。",
                        rule.id, symbol.display_name
                    ),
                });
                continue;
            }
            if !rule.include_paths.is_empty()
                && !rule
                    .include_paths
                    .iter()
                    .any(|prefix| path_has_prefix(&symbol.path, prefix))
            {
                continue;
            }
            if rule
                .exclude_paths
                .iter()
                .any(|prefix| path_has_prefix(&symbol.path, prefix))
            {
                continue;
            }
            let hunks = match crate::git::diff_hunks(root, "HEAD", &symbol.path) {
                Ok(hunks) => hunks,
                Err(error) => {
                    feedback.push(FeedbackItem {
                        severity: FeedbackSeverity::Warning,
                        text: format!(
                            "规则 {} 无法读取 {} 的 Git hunk（{}）；Stop 按 fail-open 处理。",
                            rule.id, symbol.path, error
                        ),
                    });
                    continue;
                }
            };
            let intersects = hunks.iter().any(|hunk| {
                hunk.old.is_some_and(|range| {
                    range.start_line <= symbol.end_line && symbol.start_line <= range.end_line
                }) || (hunk.old.is_none()
                    && hunk.old_start.saturating_add(1) >= symbol.start_line
                    && hunk.old_start <= symbol.end_line)
            });
            if intersects {
                violations.push(format!(
                    "{} 命中 {}:{}-{}（semantic_baseline_diff）：{}",
                    rule.id, symbol.path, symbol.start_line, symbol.end_line, rule.message
                ));
            }
        }
    }
    (violations, feedback)
}

fn session_context(config: &BrainConfig) -> String {
    let active_rules = config
        .rules
        .iter()
        .filter(|rule| rule.status == brain_core::MemoryStatus::Active)
        .map(|rule| format!("- {}: {}", rule.id, rule.message))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Project Brain 已加载项目 {}（project_key={}）。当前有效规则：\n{}",
        config.project_name, config.project_key, active_rules
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_core::{
        ActionDescriptor, ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, DecisionKind,
        EvidenceGrade, MemoryStatus, ProjectLanguageProfile, Rule, RuleEffect, RuleStrength,
        RuleSymbolScope, SemanticLanguageMapping, SemanticProviderFormat, SemanticProviderProfile,
        StopReconcileConfig, SymbolResolutionPolicy, ToolAction, ToolImpact,
    };
    use brain_store::{BrainStore, SemanticSnapshotSource};
    use brain_symbols::{
        IdentityQuality, LineageSymbolObservation, ProviderDescriptor, SourceFileState,
        SourceLanguage, SymbolNode, SymbolNodeInput, SymbolSnapshot,
    };

    use super::{evaluate_symbol_rules, evaluate_symbol_stop};
    use crate::git;
    use crate::provider::ProviderTrustStatus;

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "project-brain-protocol-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "集成夹具在一个位置建立真实 Git、semantic snapshot、trusted attestation 与 hard rule"
    )]
    fn fixture() -> (PathBuf, BrainConfig, BrainStore, String, String) {
        let root = test_root("semantic-gate");
        fs::create_dir_all(root.join("src")).unwrap();
        let source = "fn protected() {\n    println!(\"safe\");\n}\n";
        fs::write(root.join("src/lib.rs"), source).unwrap();
        run_git(&root, &["init"]);
        run_git(&root, &["add", "src/lib.rs"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Project Brain Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "baseline",
            ],
        );
        let project_key = "project_protocol_test";
        let provider = ProviderDescriptor {
            id: "scip-test-contract".to_owned(),
            version: "contract-1".to_owned(),
            identity_quality: IdentityQuality::Semantic,
        };
        let symbol = SymbolNode::from_provider_key(
            project_key,
            &provider,
            SymbolNodeInput {
                language: SourceLanguage::rust(),
                kind: "function",
                provider_key: "test package protected().",
                display_name: "protected",
                path: "src/lib.rs",
                start_line: 1,
                end_line: 3,
                content: source.as_bytes(),
            },
        );
        let head = git::head_revision(&root).unwrap();
        let snapshot = SymbolSnapshot::for_worktree(
            project_key,
            provider.clone(),
            &head,
            vec![SourceFileState::from_source(
                "src/lib.rs",
                SourceLanguage::rust(),
                source.as_bytes(),
                false,
            )],
            vec![symbol.clone()],
            Vec::new(),
        );
        let observation = LineageSymbolObservation {
            project_key: project_key.to_owned(),
            provider_profile_id: "test-main".to_owned(),
            provider_contract_id: provider.id.clone(),
            language: SourceLanguage::rust(),
            snapshot_revision: snapshot.source_revision.clone(),
            symbol_id: symbol.id.clone(),
            provider_symbol: Some("test package protected().".to_owned()),
            is_local: false,
            kind: symbol.kind.clone(),
            display_name: symbol.display_name.clone(),
            path: symbol.path.clone(),
            normalized_definition_fingerprint: format!("sha256_{}", "a".repeat(64)),
        };
        let store = BrainStore::open_in_memory().unwrap();
        store
            .apply_semantic_snapshot(
                &snapshot,
                "test-main",
                &[observation],
                &[],
                &SemanticSnapshotSource::trusted_provider(
                    git::worktree_fingerprint(&root).unwrap(),
                    head,
                    true,
                    "registration-test".to_owned(),
                    "b".repeat(64),
                    "c".repeat(64),
                ),
            )
            .unwrap();
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: project_key.to_owned(),
            project_name: "protocol test".to_owned(),
            language_profiles: vec![ProjectLanguageProfile {
                language: "rust".to_owned(),
                roots: Vec::new(),
            }],
            semantic_providers: vec![SemanticProviderProfile {
                id: "test-main".to_owned(),
                format: SemanticProviderFormat::Scip,
                producer: "test-producer".to_owned(),
                contract_version: 1,
                language_mappings: vec![SemanticLanguageMapping {
                    raw_language: Some("rust".to_owned()),
                    language: "rust".to_owned(),
                    allow_missing_language: false,
                }],
            }],
            rules: vec![Rule {
                id: "R-SYMBOL".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::Block,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                actions: vec![ActionKind::Modify],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: vec![RuleSymbolScope {
                    provider_profile_id: "test-main".to_owned(),
                    provider_contract_id: provider.id,
                    language_id: "rust".to_owned(),
                    anchor_snapshot_fingerprint: snapshot.source_revision.clone(),
                    anchor_symbol_id: symbol.id.clone(),
                    resolution_policy: SymbolResolutionPolicy::ConfirmedLineageOnly,
                }],
                message: "禁止修改 protected".to_owned(),
                rationale: "测试 hard semantic gate".to_owned(),
            }],
            stop_reconcile: StopReconcileConfig {
                enabled: false,
                base: "HEAD".to_owned(),
                envelope: ".project-brain/envelope.json".to_owned(),
            },
        };
        config.validate().unwrap();
        (root, config, store, snapshot.source_revision, symbol.id)
    }

    fn descriptor() -> ActionDescriptor {
        ActionDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            event_id: "event".to_owned(),
            session_id: "session".to_owned(),
            cwd: String::new(),
            action: ActionKind::Modify,
            operation: "Write".to_owned(),
            target_files: vec!["src/lib.rs".to_owned()],
            command: None,
            metadata: BTreeMap::new(),
        }
    }

    fn provider_trust() -> BTreeMap<String, ProviderTrustStatus> {
        [(
            "test-main".to_owned(),
            ProviderTrustStatus {
                profile_id: "test-main".to_owned(),
                ready: true,
                registration_id: Some("registration-test".to_owned()),
                registration_revision: Some(1),
                executable_sha256: Some("b".repeat(64)),
                issue: None,
            },
        )]
        .into_iter()
        .collect()
    }

    #[test]
    fn fresh_deterministic_whole_file_impact_can_block() {
        let (root, config, store, _, _) = fixture();
        let action = ToolAction {
            kind: ActionKind::Modify,
            target_files: vec!["src/lib.rs".to_owned()],
            command: None,
            deterministic_impacts: vec![ToolImpact {
                path: "src/lib.rs".to_owned(),
                whole_file: true,
                ranges: Vec::new(),
            }],
        };
        let decision = evaluate_symbol_rules(
            &root,
            &config,
            &store,
            &provider_trust(),
            &descriptor(),
            &action,
        );
        assert_eq!(decision.decision, DecisionKind::Block);
        assert_eq!(
            decision.evidence[0].grade,
            Some(EvidenceGrade::SemanticDirect)
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_snapshot_or_unknown_impact_is_advisory_not_a_violation() {
        let (root, config, store, _, _) = fixture();
        fs::write(
            root.join("src/lib.rs"),
            "fn protected() {\n    println!(\"changed\");\n}\n",
        )
        .unwrap();
        let action = ToolAction {
            kind: ActionKind::Modify,
            target_files: vec!["src/lib.rs".to_owned()],
            command: None,
            deterministic_impacts: Vec::new(),
        };
        let decision = evaluate_symbol_rules(
            &root,
            &config,
            &store,
            &provider_trust(),
            &descriptor(),
            &action,
        );
        assert_eq!(decision.decision, DecisionKind::AllowWithContext);
        assert_eq!(decision.evidence[0].grade, Some(EvidenceGrade::Unavailable));
        assert_eq!(decision.evidence[0].effect, RuleEffect::InjectContext);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_drifted_current_provider_binding_is_fail_open() {
        let (root, config, store, _, _) = fixture();
        let action = ToolAction {
            kind: ActionKind::Modify,
            target_files: vec!["src/lib.rs".to_owned()],
            command: None,
            deterministic_impacts: vec![ToolImpact {
                path: "src/lib.rs".to_owned(),
                whole_file: true,
                ranges: Vec::new(),
            }],
        };
        let decision = evaluate_symbol_rules(
            &root,
            &config,
            &store,
            &BTreeMap::new(),
            &descriptor(),
            &action,
        );
        assert_eq!(decision.decision, DecisionKind::AllowWithContext);
        assert_eq!(decision.evidence[0].grade, Some(EvidenceGrade::Unavailable));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stop_uses_clean_head_baseline_and_actual_git_hunks() {
        let (root, config, store, _, _) = fixture();
        fs::write(
            root.join("src/lib.rs"),
            "fn protected() {\n    let inserted = true;\n    println!(\"safe\");\n}\n",
        )
        .unwrap();
        let (violations, feedback) =
            evaluate_symbol_stop(&root, &config, &store, &provider_trust());
        assert!(feedback.is_empty());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("semantic_baseline_diff"));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
