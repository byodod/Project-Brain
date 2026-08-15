use std::{collections::BTreeMap, path::Path};

use brain_core::{
    ActionDescriptor, ActionKind, BrainConfig, CURRENT_SCHEMA_VERSION, ContextItem, Decision,
    DecisionKind, Evidence, EvidenceGrade, FeedbackItem, FeedbackSeverity, GateDecision,
    HOOK_PROTOCOL_VERSION, HookEventPayload, HookOutcomePayload, InternalHookEvent,
    InternalHookOutcome, MemoryStatus, Rule, RuleEffect, RuleEngine, StopDecision, ToolAction,
    normalize_project_path, path_has_prefix,
};
use brain_evidence::{EvidenceFreshness, EvidencePlane};
use brain_store::{
    BrainStore, ControlSessionState, EvidenceHeadIdentity, EvidenceHeadTransition,
    EvidenceImpactPlan, SemanticResolutionKind, SemanticSourceTrust,
};
use sha2::{Digest, Sha256};

use crate::evidence::{CurrentSourceVerification, effective_evidence_freshness_v2};
use crate::provider::ProviderTrustStatus;
use crate::{app::decision_reason, error::AppError, git, reconcile};

const SOURCE_BOUND_EVIDENCE_PLANES: [EvidencePlane; 6] = [
    EvidencePlane::Source,
    EvidencePlane::Semantic,
    EvidencePlane::Engine,
    EvidencePlane::Build,
    EvidencePlane::Test,
    EvidencePlane::Runtime,
];

#[allow(
    clippy::too_many_lines,
    reason = "Hook v2 的事件状态机在单一入口保持事件到结果的可审计映射"
)]
pub fn process(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: &InternalHookEvent,
) -> Result<InternalHookOutcome, AppError> {
    event.validate()?;
    let payload = match &event.payload {
        HookEventPayload::SessionOpened(opened) => {
            let _ = store.open_control_session(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                opened.parent_session_key.as_deref(),
                session_origin_name(opened.origin),
                opened.delegation_depth,
            )?;
            let state = store.sync_control_context(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                &project_context_digest(config)?,
            )?;
            let current_source = CurrentSourceVerification::inspect(root);
            let mut inject = control_context_items(config, &state);
            inject.extend(agent_claim_context(store, &config.project_key)?);
            inject.extend(evidence_context(
                root,
                store,
                &config.project_key,
                &current_source,
                true,
            )?);
            mark_context_delivery(store, event, &inject)?;
            HookOutcomePayload::SessionOpened { inject }
        }
        HookEventPayload::IntentDeclared(intent) => {
            let goal_digest = digest_parts(&[b"project-brain/raw-goal/v1", intent.text.as_bytes()]);
            let _ = store.declare_control_goal(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                &intent.text,
                &goal_digest,
            )?;
            let state = store.sync_control_context(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                &project_context_digest(config)?,
            )?;
            let current_source = CurrentSourceVerification::inspect(root);
            let mut inject = control_context_items(config, &state);
            inject.extend(agent_claim_context(store, &config.project_key)?);
            inject.extend(evidence_context(
                root,
                store,
                &config.project_key,
                &current_source,
                false,
            )?);
            mark_context_delivery(store, event, &inject)?;
            HookOutcomePayload::IntentDeclared {
                gate: GateDecision::NoVeto,
                inject,
            }
        }
        HookEventPayload::ContextRequested(_) => {
            let state = store.sync_control_context(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                &project_context_digest(config)?,
            )?;
            if !control_delivery_needed(&state) {
                return Ok(InternalHookOutcome {
                    protocol_version: HOOK_PROTOCOL_VERSION,
                    event_id: event.event_id.clone(),
                    payload: HookOutcomePayload::ContextRequested { inject: Vec::new() },
                });
            }
            let current_source = CurrentSourceVerification::inspect(root);
            let mut inject = control_context_items(config, &state);
            inject.extend(agent_claim_context(store, &config.project_key)?);
            inject.extend(evidence_context(
                root,
                store,
                &config.project_key,
                &current_source,
                false,
            )?);
            mark_context_delivery(store, event, &inject)?;
            HookOutcomePayload::ContextRequested { inject }
        }
        HookEventPayload::ToolAboutToRun(tool) => {
            let state = store.sync_control_context(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
                &project_context_digest(config)?,
            )?;
            if state.outstanding_kind.as_deref() == Some("repair_required")
                && !repair_inspection_action(&tool.action)
                && !action_addresses_repair(root, &tool.action, state.outstanding_json.as_deref())
            {
                let inject = control_context_items(config, &state);
                return Ok(InternalHookOutcome {
                    protocol_version: HOOK_PROTOCOL_VERSION,
                    event_id: event.event_id.clone(),
                    payload: HookOutcomePayload::ToolAboutToRun {
                        gate: GateDecision::Replan {
                            reason: "存在尚未由实际 diff 证明修复的偏差；当前写入与修复范围无关，必须先纠偏".to_owned(),
                        },
                        inject,
                    },
                });
            }
            let decision = evaluate_action(
                root,
                config,
                store,
                provider_trust,
                event,
                &tool.action,
                &tool.tool_name,
            )?;
            let mut inject = context_from_decision(&decision);
            let current_source = CurrentSourceVerification::inspect(root);
            inject.extend(evidence_context(
                root,
                store,
                &config.project_key,
                &current_source,
                false,
            )?);
            let proposal_digest = action_proposal_digest(&tool.action)?;
            let gate = if decision.decision == DecisionKind::RequireReview {
                if delivered_replan_matches(&state, &proposal_digest) {
                    store.clear_control_hold(
                        &config.project_key,
                        event.adapter.kind,
                        &event.session_key,
                    )?;
                    GateDecision::NoVeto
                } else {
                    let hold = serde_json::json!({
                        "schema_version": 1,
                        "proposal_digest": proposal_digest,
                        "target_files": tool.action.target_files,
                        "decision": decision,
                        "instruction": "撤回本次写入选择；在下一模型步骤吸收约束后重新提出变更"
                    });
                    store.set_control_hold(
                        &config.project_key,
                        event.adapter.kind,
                        &event.session_key,
                        "replan",
                        &serde_json::to_string(&hold)?,
                    )?;
                    let state = store
                        .control_session(
                            &config.project_key,
                            event.adapter.kind,
                            &event.session_key,
                        )?
                        .ok_or_else(|| AppError::Provider("控制会话丢失".to_owned()))?;
                    inject = control_context_items(config, &state);
                    if event.adapter.kind != brain_core::AdapterKind::Dsh {
                        mark_context_delivery(store, event, &inject)?;
                    }
                    GateDecision::Replan {
                        reason: decision_reason(&decision),
                    }
                }
            } else {
                gate_from_decision(&decision)
            };
            if matches!(gate, GateDecision::NoVeto) && pre_action_may_mutate_source(&tool.action) {
                store.record_control_change_proposal(
                    &config.project_key,
                    event.adapter.kind,
                    &event.session_key,
                    &tool.operation_id,
                    &proposal_digest,
                    &serde_json::to_string(&tool.action)?,
                )?;
                if let Ok(source_state) = git::worktree_source_state(root) {
                    let source_state_json = serde_json::to_string(&source_state)?;
                    store.record_source_operation_baseline(
                        &config.project_key,
                        event.adapter.kind,
                        &event.session_key,
                        &tool.operation_id,
                        &event.event_id,
                        &source_state.fingerprint,
                        &source_state_json,
                    )?;
                }
            }
            HookOutcomePayload::ToolAboutToRun { gate, inject }
        }
        HookEventPayload::ToolFinished(tool) => {
            tool_finished_payload(root, config, store, provider_trust, event, tool)?
        }
        HookEventPayload::TaskStopping(stopping) => {
            let current_source = CurrentSourceVerification::inspect(root);
            let control = store.control_session(
                &config.project_key,
                event.adapter.kind,
                &event.session_key,
            )?;
            let (stop, mut feedback) = if !stopping.vendor_loop_active
                && let Some(state) = control
                && let Some(kind) = state.outstanding_kind
            {
                (
                    StopDecision::ContinueWork {
                        reason: format!(
                            "Project Brain active-control 尚未闭环：{kind}。必须先处理已交付的纠偏状态并用实际 diff/验证证明完成。"
                        ),
                    },
                    vec![FeedbackItem {
                        severity: FeedbackSeverity::Error,
                        text: state.outstanding_json.unwrap_or_default(),
                    }],
                )
            } else {
                stop_decision(
                    root,
                    config,
                    store,
                    provider_trust,
                    &current_source,
                    stopping.vendor_loop_active,
                )
            };
            feedback.extend(evidence_feedback(
                root,
                store,
                &config.project_key,
                &current_source,
            )?);
            HookOutcomePayload::TaskStopping { stop, feedback }
        }
    };
    Ok(InternalHookOutcome {
        protocol_version: HOOK_PROTOCOL_VERSION,
        event_id: event.event_id.clone(),
        payload,
    })
}

fn tool_finished_payload(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: &InternalHookEvent,
    tool: &brain_core::ToolFinished,
) -> Result<HookOutcomePayload, AppError> {
    let decision = evaluate_action(
        root,
        config,
        store,
        provider_trust,
        event,
        &tool.action,
        &tool.tool_name,
    )?;
    let mut feedback = feedback_from_decision(&decision);
    reconcile_observed_change(root, config, store, event, tool, &mut feedback)?;
    if let Some(plan) = evidence_impact_plan(root, store, &config.project_key, event, tool)? {
        let result = store.apply_evidence_impact_plan(&config.project_key, &plan)?;
        if result.heads_marked > 0 {
            let stale_count = plan
                .transitions
                .iter()
                .filter(|transition| transition.freshness == EvidenceFreshness::Stale)
                .count();
            let unknown_count = plan
                .transitions
                .iter()
                .filter(|transition| transition.freshness == EvidenceFreshness::Unknown)
                .count();
            let transition_summary = match (stale_count, unknown_count) {
                (stale, 0) => format!("{stale} 个 head 已标记为 stale"),
                (0, unknown) => format!("{unknown} 个 head 已标记为 unknown"),
                (stale, unknown) => {
                    format!("{stale} 个 head 已标记为 stale，{unknown} 个 head 已标记为 unknown")
                }
            };
            feedback.push(FeedbackItem {
                severity: FeedbackSeverity::Warning,
                text: format!(
                    "Project Brain：按 Evidence Input Manifest 精确复核后，{transition_summary}；共 {} 个 head 被降权，{} 个未受影响 head 保持 fresh。非 fresh 证据没有硬阻断资格。",
                    result.heads_marked,
                    result.heads_preserved,
                ),
            });
        }
    }
    Ok(HookOutcomePayload::ToolFinished { feedback })
}

fn explicit_mutation(action: &ToolAction) -> bool {
    matches!(
        action.kind,
        ActionKind::Create | ActionKind::Modify | ActionKind::Delete
    )
}

fn opaque_action_may_mutate_source(action: &ToolAction) -> bool {
    matches!(
        action.kind,
        ActionKind::Execute | ActionKind::GitOperation | ActionKind::Unknown
    )
}

fn pre_action_may_mutate_source(action: &ToolAction) -> bool {
    explicit_mutation(action)
        || matches!(action.kind, ActionKind::Execute | ActionKind::GitOperation)
}

#[allow(
    clippy::too_many_lines,
    reason = "ObservedChange 必须线性呈现提案、基线、hold 与反馈的完整闭环"
)]
fn reconcile_observed_change(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: &InternalHookEvent,
    tool: &brain_core::ToolFinished,
    feedback: &mut Vec<FeedbackItem>,
) -> Result<(), AppError> {
    if tool.status == brain_core::ToolStatus::Failed || !pre_action_may_mutate_source(&tool.action)
    {
        return Ok(());
    }
    let state = store.ensure_control_session(
        &config.project_key,
        event.adapter.kind,
        &event.session_key,
    )?;
    let Some(stored_proposal) = store.control_change_proposal(
        &config.project_key,
        event.adapter.kind,
        &event.session_key,
        &tool.operation_id,
    )?
    else {
        let hold = serde_json::json!({
            "schema_version": 1,
            "operation_id": tool.operation_id,
            "tool_name": tool.tool_name,
            "reason": "缺少与本次 PostTool 对应的已放行 PreTool 变更提案",
            "declared_targets": mutation_paths(&tool.action),
            "result_digest": tool.result_digest,
        });
        store.set_control_hold(
            &config.project_key,
            event.adapter.kind,
            &event.session_key,
            "verify_required",
            &serde_json::to_string(&hold)?,
        )?;
        feedback.push(FeedbackItem {
            severity: FeedbackSeverity::Warning,
            text: "Project Brain 缺少对应的 PreTool 提案；已进入 verify_required。".to_owned(),
        });
        return Ok(());
    };
    let proposed_action = serde_json::from_str::<ToolAction>(&stored_proposal.action_json)
        .map_err(|error| AppError::Provider(format!("持久化变更提案 JSON 已损坏：{error}")))?;
    let Some(observed) = observed_change_paths(root, store, event, tool)? else {
        let hold = serde_json::json!({
            "schema_version": 1,
            "operation_id": tool.operation_id,
            "tool_name": tool.tool_name,
            "reason": "无法取得可靠的 PreTool Source baseline，实际影响范围需要验证",
            "declared_targets": mutation_paths(&proposed_action),
            "result_digest": tool.result_digest,
        });
        store.set_control_hold(
            &config.project_key,
            event.adapter.kind,
            &event.session_key,
            "verify_required",
            &serde_json::to_string(&hold)?,
        )?;
        feedback.push(FeedbackItem {
            severity: FeedbackSeverity::Warning,
            text: "Project Brain 无法可靠计算本次实际 diff；已进入 verify_required，停止前必须补充验证证据。".to_owned(),
        });
        return Ok(());
    };
    let expected = proposed_action
        .proposed_change
        .as_ref()
        .map_or_else(
            || mutation_paths(&proposed_action),
            |proposal| proposal.target_files.clone(),
        )
        .into_iter()
        .map(|path| make_project_relative(root, &path))
        .collect::<Vec<_>>();
    let observed = observed
        .into_iter()
        .map(|path| make_project_relative(root, &path))
        .collect::<Vec<_>>();
    let unexpected = observed
        .iter()
        .filter(|path| {
            !expected
                .iter()
                .any(|target| path_has_prefix(path, target) || path_has_prefix(target, path))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        let hold = serde_json::json!({
            "schema_version": 1,
            "operation_id": tool.operation_id,
            "tool_name": tool.tool_name,
            "proposal_digest": stored_proposal.proposal_digest,
            "expected_paths": expected,
            "observed_paths": observed,
            "unexpected_paths": unexpected,
            "result_digest": tool.result_digest,
            "instruction": "检查实际 diff，撤销或正式纳入超出提案范围的变更"
        });
        store.set_control_hold(
            &config.project_key,
            event.adapter.kind,
            &event.session_key,
            "repair_required",
            &serde_json::to_string(&hold)?,
        )?;
        feedback.push(FeedbackItem {
            severity: FeedbackSeverity::Error,
            text: format!(
                "Project Brain 检测到提案之外的实际变更：{}。已暂停无关写入，下一模型步骤必须先修复。",
                unexpected.join(", ")
            ),
        });
    } else if state.outstanding_kind.as_deref() == Some("repair_required")
        && action_addresses_repair(root, &proposed_action, state.outstanding_json.as_deref())
    {
        store.clear_control_hold(&config.project_key, event.adapter.kind, &event.session_key)?;
        feedback.push(FeedbackItem {
            severity: FeedbackSeverity::Info,
            text: "Project Brain 已用本次实际 diff 确认修复范围未再外溢，repair_required 已解除。"
                .to_owned(),
        });
    }
    Ok(())
}

fn observed_change_paths(
    root: &Path,
    store: &BrainStore,
    event: &InternalHookEvent,
    tool: &brain_core::ToolFinished,
) -> Result<Option<Vec<String>>, AppError> {
    let Some(baseline) = store.source_operation_baseline(
        &event.project_key,
        event.adapter.kind,
        &event.session_key,
        &tool.operation_id,
    )?
    else {
        return Ok(None);
    };
    let before = serde_json::from_str::<git::WorktreeSourceState>(&baseline.source_state_json)
        .map_err(|error| {
            AppError::Provider(format!("Source operation baseline JSON 已损坏：{error}"))
        })?;
    if before.fingerprint != baseline.source_fingerprint {
        return Err(AppError::Provider(
            "Source operation baseline fingerprint 与状态内容不一致".to_owned(),
        ));
    }
    let after = git::worktree_source_state(root)?;
    Ok(Some(
        git::SourceDeltaV1::between(&before, &after).changed_paths(),
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "精准影响计划必须完整处理基线、当前 Source、input manifest 与未知降级"
)]
fn evidence_impact_plan(
    root: &Path,
    store: &BrainStore,
    project_key: &str,
    event: &InternalHookEvent,
    tool: &brain_core::ToolFinished,
) -> Result<Option<EvidenceImpactPlan>, AppError> {
    let explicit_mutation = explicit_mutation(&tool.action);
    if !explicit_mutation && !opaque_action_may_mutate_source(&tool.action) {
        return Ok(None);
    }
    let heads = store
        .list_evidence_heads(project_key)?
        .into_iter()
        .filter(|head| {
            SOURCE_BOUND_EVIDENCE_PLANES.contains(&head.plane)
                && head.freshness == EvidenceFreshness::Fresh
        })
        .collect::<Vec<_>>();
    if heads.is_empty() {
        return Ok(None);
    }
    let baseline = store.source_operation_baseline(
        project_key,
        event.adapter.kind,
        &event.session_key,
        &tool.operation_id,
    )?;
    let (current_source, mut changed_paths, baseline_problem) = if let Some(baseline) = baseline {
        let before = serde_json::from_str::<git::WorktreeSourceState>(&baseline.source_state_json)
            .map_err(|error| {
                AppError::Provider(format!("Source operation baseline JSON 已损坏：{error}"))
            })?;
        if before.fingerprint != baseline.source_fingerprint {
            return Err(AppError::Provider(
                "Source operation baseline fingerprint 与状态内容不一致".to_owned(),
            ));
        }
        match git::worktree_source_state(root) {
            Ok(after) => {
                let delta = git::SourceDeltaV1::between(&before, &after);
                (
                    CurrentSourceVerification::Verified(after.fingerprint),
                    delta.changed_paths(),
                    None,
                )
            }
            Err(error) => (
                CurrentSourceVerification::Unavailable(error.to_string()),
                mutation_paths(&tool.action),
                Some("post_source_state_unavailable".to_owned()),
            ),
        }
    } else {
        let current = CurrentSourceVerification::inspect(root);
        let mut paths = mutation_paths(&tool.action);
        paths.extend(git::changed_files(root, "HEAD").unwrap_or_default());
        (
            current,
            paths,
            Some("pre_source_baseline_missing".to_owned()),
        )
    };
    changed_paths.sort();
    changed_paths.dedup();
    let mut transitions = Vec::new();
    let mut preserved = Vec::new();
    let identity = |head: &brain_store::EvidenceHeadRecord| EvidenceHeadIdentity {
        plane: head.plane,
        provider_id: head.provider_id.clone(),
        snapshot_fingerprint: head.snapshot_fingerprint.clone(),
    };
    for head in &heads {
        if explicit_mutation && head.input_manifest.is_none() {
            transitions.push(EvidenceHeadTransition {
                identity: identity(head),
                freshness: EvidenceFreshness::Stale,
                reason: format!(
                    "PostToolUse observed {:?} {:?}; legacy project-wide Evidence 被显式源码变更失效",
                    tool.status, tool.action.kind
                ),
            });
            continue;
        }
        let effective = effective_evidence_freshness_v2(
            root,
            head.freshness,
            &head.snapshot.source_fingerprint,
            head.input_manifest.as_ref(),
            &current_source,
        );
        match effective.freshness {
            EvidenceFreshness::Fresh => preserved.push(identity(head)),
            EvidenceFreshness::Stale | EvidenceFreshness::Unknown => {
                transitions.push(EvidenceHeadTransition {
                    identity: identity(head),
                    freshness: effective.freshness,
                    reason: effective.reason.unwrap_or_else(|| {
                        format!(
                            "PostToolUse observed {:?} {:?}; Evidence input validity changed",
                            tool.status, tool.action.kind
                        )
                    }),
                });
            }
        }
    }
    let final_source = CurrentSourceVerification::inspect(root);
    let mut unknown_reason = baseline_problem;
    if final_source != current_source {
        transitions = heads
            .iter()
            .map(|head| EvidenceHeadTransition {
                identity: identity(head),
                freshness: EvidenceFreshness::Unknown,
                reason: "Evidence impact 规划期间 whole Source 发生并发变化".to_owned(),
            })
            .collect();
        preserved.clear();
        unknown_reason = Some("source_changed_during_impact_planning".to_owned());
    } else if let CurrentSourceVerification::Unavailable(error) = &current_source {
        unknown_reason = Some(error.clone());
    }
    Ok(Some(EvidenceImpactPlan {
        event_id: event.event_id.clone(),
        hook_event_id: Some(event.event_id.clone()),
        operation_id: Some(tool.operation_id.clone()),
        observed_source_fingerprint: final_source.fingerprint().map(ToOwned::to_owned),
        transitions,
        preserved,
        changed_paths,
        unknown_reason,
    }))
}

fn mutation_paths(action: &ToolAction) -> Vec<String> {
    let mut paths = action.target_files.clone();
    paths.extend(
        action
            .deterministic_impacts
            .iter()
            .map(|impact| impact.path.clone()),
    );
    paths.sort();
    paths.dedup();
    paths
}

fn action_proposal_digest(action: &ToolAction) -> Result<String, AppError> {
    if let Some(proposal) = &action.proposed_change {
        return Ok(proposal.proposal_digest.clone());
    }
    let canonical = serde_json::to_vec(action)?;
    Ok(digest_parts(&[
        b"project-brain/action-proposal/v1",
        &canonical,
    ]))
}

fn delivered_replan_matches(state: &ControlSessionState, proposal_digest: &str) -> bool {
    if state.outstanding_kind.as_deref() != Some("replan") || !state.outstanding_delivered {
        return false;
    }
    state
        .outstanding_json
        .as_deref()
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .and_then(|payload| {
            payload
                .get("proposal_digest")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .is_some_and(|digest| digest == proposal_digest)
}

fn action_addresses_repair(root: &Path, action: &ToolAction, payload: Option<&str>) -> bool {
    if !explicit_mutation(action) {
        return false;
    }
    let repair_paths = payload
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .and_then(|payload| payload.get("unexpected_paths").cloned())
        .and_then(|paths| serde_json::from_value::<Vec<String>>(paths).ok())
        .unwrap_or_default();
    let repair_paths = repair_paths
        .iter()
        .map(|path| make_project_relative(root, path))
        .collect::<Vec<_>>();
    !repair_paths.is_empty()
        && mutation_paths(action).iter().any(|target| {
            let target = make_project_relative(root, target);
            repair_paths
                .iter()
                .any(|repair| path_has_prefix(&target, repair) || path_has_prefix(repair, &target))
        })
}

fn make_project_relative(root: &Path, path: &str) -> String {
    let candidate = normalized_root_path(path);
    let project_root = normalized_root_path(&root.to_string_lossy());
    let candidate_cmp = comparable_path(&candidate);
    let root_cmp = comparable_path(&project_root);
    if candidate_cmp == root_cmp {
        return String::new();
    }
    let prefix = format!("{root_cmp}/");
    if candidate_cmp.starts_with(&prefix) {
        return candidate[prefix.len()..].to_owned();
    }
    candidate
}

fn normalized_root_path(path: &str) -> String {
    let normalized = normalize_project_path(path);
    normalized
        .strip_prefix("/?/")
        .unwrap_or(&normalized)
        .to_owned()
}

fn comparable_path(path: &str) -> String {
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path.to_owned()
    }
}

fn repair_inspection_action(action: &ToolAction) -> bool {
    if action.kind == ActionKind::Read {
        return true;
    }
    if !matches!(action.kind, ActionKind::Execute | ActionKind::GitOperation) {
        return false;
    }
    let Some(command) = action.command.as_deref() else {
        return false;
    };
    let command = command.to_ascii_lowercase();
    let mutating_markers = [
        "remove-item",
        "set-content",
        "add-content",
        "out-file",
        "new-item",
        "copy-item",
        "move-item",
        "rename-item",
        "clear-content",
        "git add",
        "git commit",
        "git checkout",
        "git switch",
        "git reset",
        "git restore",
        "git clean",
        "git rm",
        " rm ",
        " del ",
        " erase ",
        "mkdir",
        "touch ",
        "apply_patch",
        ">",
    ];
    if mutating_markers
        .iter()
        .any(|marker| command.contains(marker))
    {
        return false;
    }
    command.split(';').all(|segment| {
        let segment = segment.trim();
        segment.is_empty()
            || [
                "git status",
                "git diff",
                "git log",
                "git show",
                "git branch",
                "get-childitem",
                "get-content",
                "write-host",
                "write-output",
                "select-string",
                "where.exe",
                "rg ",
                "rg\t",
                "node --version",
                "npm --version",
            ]
            .iter()
            .any(|prefix| segment.starts_with(prefix))
    })
}

fn evidence_context(
    root: &Path,
    store: &BrainStore,
    project_key: &str,
    current_source: &CurrentSourceVerification,
    include_fresh: bool,
) -> Result<Vec<ContextItem>, AppError> {
    Ok(store
        .list_evidence_head_summaries(project_key)?
        .into_iter()
        .filter_map(|head| {
            let effective = effective_evidence_freshness_v2(
                root,
                head.freshness,
                &head.source_fingerprint,
                head.input_manifest.as_ref(),
                current_source,
            );
            (include_fresh || effective.freshness != EvidenceFreshness::Fresh).then(|| {
                ContextItem {
                    text: evidence_message(
                        head.plane,
                        &head.provider_id,
                        head.freshness,
                        effective.freshness,
                        &head.snapshot_fingerprint,
                        combined_freshness_reason(
                            head.stale_reason.as_deref(),
                            effective.reason.as_deref(),
                        )
                        .as_deref(),
                    ),
                }
            })
        })
        .collect())
}

fn evidence_feedback(
    root: &Path,
    store: &BrainStore,
    project_key: &str,
    current_source: &CurrentSourceVerification,
) -> Result<Vec<FeedbackItem>, AppError> {
    Ok(store
        .list_evidence_head_summaries(project_key)?
        .into_iter()
        .filter_map(|head| {
            let effective = effective_evidence_freshness_v2(
                root,
                head.freshness,
                &head.source_fingerprint,
                head.input_manifest.as_ref(),
                current_source,
            );
            (effective.freshness != EvidenceFreshness::Fresh).then(|| FeedbackItem {
                severity: FeedbackSeverity::Warning,
                text: evidence_message(
                    head.plane,
                    &head.provider_id,
                    head.freshness,
                    effective.freshness,
                    &head.snapshot_fingerprint,
                    combined_freshness_reason(
                        head.stale_reason.as_deref(),
                        effective.reason.as_deref(),
                    )
                    .as_deref(),
                ),
            })
        })
        .collect())
}

fn combined_freshness_reason(recorded: Option<&str>, effective: Option<&str>) -> Option<String> {
    match (recorded, effective) {
        (Some(recorded), Some(effective)) if recorded != effective => {
            Some(format!("{recorded}；实时验证：{effective}"))
        }
        (Some(recorded), _) => Some(recorded.to_owned()),
        (None, Some(effective)) => Some(effective.to_owned()),
        (None, None) => None,
    }
}

fn evidence_message(
    plane: EvidencePlane,
    provider_id: &str,
    recorded_freshness: EvidenceFreshness,
    effective_freshness: EvidenceFreshness,
    snapshot_fingerprint: &str,
    stale_reason: Option<&str>,
) -> String {
    let reason = stale_reason
        .map(|value| format!("；原因：{value}"))
        .unwrap_or_default();
    format!(
        "Project Brain {} Evidence：provider={provider_id}，recorded_freshness={}，effective_freshness={}，snapshot={snapshot_fingerprint}{reason}。只有 effective_freshness=fresh + complete + deterministic + 当前 Source 指纹匹配的具体 error finding 才可能具备硬阻断资格；仍需仓库规则显式授权。",
        plane.as_str(),
        recorded_freshness.as_str(),
        effective_freshness.as_str()
    )
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
    } else if hard_effects.contains(&RuleEffect::RequireReview) {
        DecisionKind::RequireReview
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
            DecisionKind::RequireReview => 3,
            DecisionKind::Block => 4,
        })
        .unwrap_or(DecisionKind::Allow);
    match left.decision {
        DecisionKind::Allow => "未命中需要改变行为的规则",
        DecisionKind::AllowWithContext => "允许执行，并注入相关项目约束",
        DecisionKind::Escalate => "需要显式决策后再继续",
        DecisionKind::RequireReview => "执行前必须重新规划并显式吸收相关项目约束",
        DecisionKind::Block => "命中具备确定性证据的硬规则，拒绝执行",
    }
    .clone_into(&mut left.summary);
    left
}

fn gate_from_decision(decision: &Decision) -> GateDecision {
    match decision.decision {
        DecisionKind::Block | DecisionKind::Escalate => GateDecision::Deny {
            reason: decision_reason(decision),
        },
        DecisionKind::RequireReview => GateDecision::Replan {
            reason: decision_reason(decision),
        },
        DecisionKind::Allow | DecisionKind::AllowWithContext => GateDecision::NoVeto,
    }
}

fn context_from_decision(decision: &Decision) -> Vec<ContextItem> {
    if matches!(
        decision.decision,
        DecisionKind::AllowWithContext | DecisionKind::RequireReview
    ) {
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
        DecisionKind::RequireReview => vec![FeedbackItem {
            severity: FeedbackSeverity::Info,
            text: format!(
                "操作已在 Project Brain 重规划交付后执行：{}",
                decision_reason(decision)
            ),
        }],
        DecisionKind::Block | DecisionKind::Escalate => {
            vec![FeedbackItem {
                severity: FeedbackSeverity::Error,
                text: format!(
                    "操作已经执行，无法撤销其副作用；Project Brain 检测到：{}",
                    decision_reason(decision)
                ),
            }]
        }
    }
}

fn stop_decision(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    current_source: &CurrentSourceVerification,
    vendor_loop_active: bool,
) -> (StopDecision, Vec<FeedbackItem>) {
    if vendor_loop_active {
        return (StopDecision::AllowStop, Vec::new());
    }
    let (finding_violations, finding_feedback) =
        evaluate_finding_stop(root, config, store, current_source);
    if !config.stop_reconcile.enabled {
        let (mut violations, mut feedback) =
            evaluate_symbol_stop(root, config, store, provider_trust);
        violations.extend(finding_violations);
        feedback.extend(finding_feedback);
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
            let (mut violations, mut feedback) =
                evaluate_symbol_stop(root, config, store, provider_trust);
            violations.extend(finding_violations);
            feedback.extend(finding_feedback);
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

/// 只把精确命中、fresh、complete、deterministic 且声明为确定性违规的 finding
/// 提升为 Stop gate。存储不可用、缺失 head、合同漂移和 stale 都按 fail-open 反馈。
fn evaluate_finding_stop(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    current_source: &CurrentSourceVerification,
) -> (Vec<String>, Vec<FeedbackItem>) {
    let heads = match store.list_evidence_heads(&config.project_key) {
        Ok(heads) => heads,
        Err(error) => {
            return (
                Vec::new(),
                vec![FeedbackItem {
                    severity: FeedbackSeverity::Warning,
                    text: format!(
                        "Evidence ledger 不可用（{error}）；Finding effect 映射按 fail-open 处理。"
                    ),
                }],
            );
        }
    };
    let mut violations = Vec::new();
    let mut feedback = Vec::new();
    for mapping in config
        .finding_effect_mappings
        .iter()
        .filter(|mapping| mapping.status == MemoryStatus::Active)
    {
        let matching_identity = heads
            .iter()
            .find(|head| head.plane == mapping.plane && head.provider_id == mapping.provider_id);
        let Some(head) = matching_identity else {
            feedback.push(FeedbackItem {
                severity: FeedbackSeverity::Warning,
                text: format!(
                    "Finding effect 映射 {} 缺少 {} provider={} 的 Evidence head；按 advisory 处理。",
                    mapping.id,
                    mapping.plane.as_str(),
                    mapping.provider_id
                ),
            });
            continue;
        };
        if head.snapshot.provider.contract_version != mapping.provider_contract_version {
            feedback.push(FeedbackItem {
                severity: FeedbackSeverity::Warning,
                text: format!(
                    "Finding effect 映射 {} 要求 provider contract v{}，当前为 v{}；按 advisory 处理。",
                    mapping.id,
                    mapping.provider_contract_version,
                    head.snapshot.provider.contract_version
                ),
            });
            continue;
        }
        let effective = effective_evidence_freshness_v2(
            root,
            head.freshness,
            &head.snapshot.source_fingerprint,
            head.input_manifest.as_ref(),
            current_source,
        );
        for finding in head
            .snapshot
            .findings
            .iter()
            .filter(|finding| finding.code == mapping.finding_code)
        {
            match mapping.effect {
                RuleEffect::InjectContext => feedback.push(FeedbackItem {
                    severity: FeedbackSeverity::Info,
                    text: format!("{}：{}", mapping.message, finding.message),
                }),
                RuleEffect::Block | RuleEffect::RequireReview | RuleEffect::Escalate => {
                    if head
                        .snapshot
                        .finding_can_hard_block(finding, effective.freshness, true)
                    {
                        violations.push(format!(
                            "{} 命中 {}/{}/{}：{}",
                            mapping.id,
                            mapping.plane.as_str(),
                            mapping.provider_id,
                            finding.code,
                            mapping.message
                        ));
                    } else {
                        feedback.push(FeedbackItem {
                            severity: FeedbackSeverity::Warning,
                            text: format!(
                                "Finding effect 映射 {} 命中 {}，但证据不满足 effective fresh + complete + deterministic violation + 当前 Source 指纹匹配；按 advisory 处理。{}",
                                mapping.id,
                                finding.code,
                                effective
                                    .reason
                                    .as_deref()
                                    .map(|reason| format!(" 原因：{reason}"))
                                    .unwrap_or_default()
                            ),
                        });
                    }
                }
            }
        }
    }
    (violations, feedback)
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

fn project_context_digest(config: &BrainConfig) -> Result<String, AppError> {
    let canonical = serde_json::to_vec(&(
        &config.project_key,
        &config.project_name,
        config
            .rules
            .iter()
            .filter(|rule| rule.status == MemoryStatus::Active)
            .collect::<Vec<_>>(),
        &config.stop_reconcile,
    ))?;
    Ok(digest_parts(&[
        b"project-brain/project-context/v1",
        &canonical,
    ]))
}

fn control_delivery_needed(state: &ControlSessionState) -> bool {
    state.hydrated_epoch != state.lifecycle_epoch
        || (state.outstanding_kind.is_some() && !state.outstanding_delivered)
}

fn control_context_items(config: &BrainConfig, state: &ControlSessionState) -> Vec<ContextItem> {
    let mut sections = vec![format!(
        "[Project Brain active control]\n目标版本={}，项目上下文版本={}，生命周期={}。\n{}\n\n若需要由 Agent 自主建立或维护开发约束，请使用 `project-brain rules upsert-agent --rule AGENT-... --message ...`；该入口固定写入 agent_inference/soft/inject_context，不能自授阻断、复核或豁免权限，具体参数以 `--help` 为准。\n\n若发现值得保留的项目事实，可用 `project-brain claims submit` 追加结构化声明；声明不可删除，也不能授权规则、豁免约束或证明功能已实现。完成状态只接受实际 diff 与验证证据。",
        state.goal_revision,
        state.context_revision,
        state.lifecycle_epoch,
        session_context(config)
    )];
    if let Some(goal) = state.raw_goal.as_deref() {
        sections.push(format!(
            "当前原始用户目标（高权限事实，不得由 Agent 自行改写或宣称完成）：\n{goal}"
        ));
    }
    if let (Some(kind), Some(payload)) = (
        state.outstanding_kind.as_deref(),
        state.outstanding_json.as_deref(),
    ) {
        sections.push(format!(
            "当前必须处理的纠偏状态 kind={kind}。在继续无关写入或宣布完成前，必须按以下结构化事实重规划、修复并用实际 diff/验证证明已解决：\n{payload}"
        ));
    }
    vec![ContextItem {
        text: sections.join("\n\n"),
    }]
}

fn agent_claim_context(
    store: &BrainStore,
    project_key: &str,
) -> Result<Vec<ContextItem>, AppError> {
    let claims = store.list_agent_claims(project_key, 8)?;
    if claims.is_empty() {
        return Ok(Vec::new());
    }
    let text = claims
        .iter()
        .map(|claim| {
            format!(
                "- [{}] {} (claim_id={}, authority=agent_claim_only)",
                claim.kind, claim.content, claim.claim_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(vec![ContextItem {
        text: format!(
            "最近的 Agent 项目声明（仅用于方向提示，不得据此阻断、豁免或判断已实现）：\n{text}"
        ),
    }])
}

fn mark_context_delivery(
    store: &BrainStore,
    event: &InternalHookEvent,
    inject: &[ContextItem],
) -> Result<(), AppError> {
    if inject.is_empty() {
        return Ok(());
    }
    let text = inject
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    store.mark_control_delivered(
        &event.project_key,
        event.adapter.kind,
        &event.session_key,
        &digest_parts(&[b"project-brain/context-delivery/v1", text.as_bytes()]),
    )?;
    Ok(())
}

const fn session_origin_name(origin: brain_core::SessionOrigin) -> &'static str {
    match origin {
        brain_core::SessionOrigin::Interactive => "interactive",
        brain_core::SessionOrigin::Subagent => "subagent",
        brain_core::SessionOrigin::RuntimeContinuation => "runtime_continuation",
        brain_core::SessionOrigin::Unknown => "unknown",
    }
}

fn digest_parts(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    format!("sha256_{:x}", digest.finalize())
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
        EvidenceGrade, FindingEffectMapping, MemoryStatus, ProjectLanguageProfile, ProposedChange,
        Rule, RuleEffect, RuleStrength, RuleSymbolScope, SemanticLanguageMapping,
        SemanticProviderFormat, SemanticProviderProfile, StopReconcileConfig,
        SymbolResolutionPolicy, ToolAction, ToolImpact,
    };
    use brain_evidence::{
        EvidenceAuthority, EvidenceCoverage, EvidenceFinding, EvidencePlane, EvidenceProvider,
        EvidenceSnapshot, FindingAuthority, FindingSeverity,
    };
    use brain_store::{BrainStore, SemanticSnapshotSource};
    use brain_symbols::{
        IdentityQuality, LineageSymbolObservation, ProviderDescriptor, SourceFileState,
        SourceLanguage, SymbolNode, SymbolNodeInput, SymbolSnapshot,
    };

    use super::{
        action_addresses_repair, evaluate_finding_stop, evaluate_symbol_rules,
        evaluate_symbol_stop, make_project_relative, repair_inspection_action,
    };
    use crate::evidence::CurrentSourceVerification;
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
            finding_effect_mappings: Vec::new(),
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
                launcher_package_manifest_sha256: None,
                issue: None,
            },
        )]
        .into_iter()
        .collect()
    }

    fn finding_mapping() -> FindingEffectMapping {
        FindingEffectMapping {
            id: "TEST-ASSERT-001".to_owned(),
            status: MemoryStatus::Active,
            authority: Authority::RepositoryRule,
            strength: RuleStrength::Hard,
            effect: RuleEffect::Block,
            plane: EvidencePlane::Test,
            provider_id: "dotnet-test".to_owned(),
            provider_contract_version: 1,
            finding_code: "assertion_failed".to_owned(),
            message: "声明的测试断言失败，必须继续修复".to_owned(),
        }
    }

    fn finding_config(mappings: Vec<FindingEffectMapping>) -> BrainConfig {
        BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_finding_test".to_owned(),
            project_name: "finding test".to_owned(),
            language_profiles: Vec::new(),
            semantic_providers: Vec::new(),
            finding_effect_mappings: mappings,
            rules: Vec::new(),
            stop_reconcile: StopReconcileConfig::default(),
        }
    }

    fn test_failure_snapshot() -> EvidenceSnapshot {
        EvidenceSnapshot::new(
            "project_finding_test",
            EvidencePlane::Test,
            EvidenceProvider {
                id: "dotnet-test".to_owned(),
                version: "1.0+sha256.test".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            "sha256_source",
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![EvidenceFinding {
                code: "assertion_failed".to_owned(),
                severity: FindingSeverity::Error,
                authority: FindingAuthority::DeterministicViolation,
                message: "expected true but observed false".to_owned(),
                artifact_id: None,
                path: Some("tests/SaveTests.cs".to_owned()),
            }],
        )
        .unwrap()
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
            proposed_change: None,
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
            proposed_change: None,
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
            proposed_change: None,
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

    #[test]
    fn finding_requires_explicit_mapping_and_fresh_authoritative_evidence() {
        let store = BrainStore::open_in_memory().unwrap();
        let snapshot = test_failure_snapshot();
        store
            .apply_evidence_snapshot_for_current_source(&snapshot, &snapshot.source_fingerprint)
            .unwrap();
        let matching_source = CurrentSourceVerification::Verified("sha256_source".to_owned());

        let (unmapped, unmapped_feedback) = evaluate_finding_stop(
            Path::new("."),
            &finding_config(Vec::new()),
            &store,
            &matching_source,
        );
        assert!(unmapped.is_empty());
        assert!(unmapped_feedback.is_empty());

        let config = finding_config(vec![finding_mapping()]);
        config.validate().unwrap();
        let (mapped, feedback) =
            evaluate_finding_stop(Path::new("."), &config, &store, &matching_source);
        assert_eq!(mapped.len(), 1);
        assert!(feedback.is_empty());
        assert!(mapped[0].contains("TEST-ASSERT-001"));

        let mismatched_source =
            CurrentSourceVerification::Verified("sha256_different_source".to_owned());
        let (mismatched, mismatched_feedback) =
            evaluate_finding_stop(Path::new("."), &config, &store, &mismatched_source);
        assert!(mismatched.is_empty());
        assert!(
            mismatched_feedback
                .iter()
                .any(|item| item.text.contains("当前 Source 指纹匹配"))
        );

        let unavailable_source =
            CurrentSourceVerification::Unavailable("git unavailable".to_owned());
        let (unavailable, unavailable_feedback) =
            evaluate_finding_stop(Path::new("."), &config, &store, &unavailable_source);
        assert!(unavailable.is_empty());
        assert!(
            unavailable_feedback
                .iter()
                .any(|item| item.text.contains("无法验证"))
        );

        store
            .mark_evidence_planes_stale(
                &config.project_key,
                &[EvidencePlane::Test],
                "source-change",
                "test source changed",
                &["src/Save.cs".to_owned()],
            )
            .unwrap();
        let (stale, stale_feedback) =
            evaluate_finding_stop(Path::new("."), &config, &store, &matching_source);
        assert!(stale.is_empty());
        assert!(
            stale_feedback
                .iter()
                .any(|item| item.text.contains("advisory"))
        );
    }

    #[test]
    fn absolute_proposal_path_matches_relative_git_delta() {
        let root = Path::new(r"\\?\E:\Github\Test\project-brain-dsh-game-trial");
        assert_eq!(
            make_project_relative(
                root,
                "E:/Github/Test/project-brain-dsh-game-trial/package.json"
            ),
            "package.json"
        );
        let action = ToolAction {
            kind: ActionKind::Modify,
            target_files: vec![
                "E:/Github/Test/project-brain-dsh-game-trial/package.json".to_owned(),
            ],
            command: None,
            deterministic_impacts: Vec::new(),
            proposed_change: Some(ProposedChange {
                proposal_digest: "proposal".to_owned(),
                base_source_fingerprint: "source".to_owned(),
                target_files: vec![
                    "E:/Github/Test/project-brain-dsh-game-trial/package.json".to_owned(),
                ],
                proposed_content_digest: None,
            }),
        };
        assert!(action_addresses_repair(
            root,
            &action,
            Some(r#"{"unexpected_paths":["package.json"]}"#),
        ));
    }

    #[test]
    fn repair_hold_allows_inspection_but_not_shell_mutation() {
        let inspection = ToolAction {
            kind: ActionKind::GitOperation,
            target_files: Vec::new(),
            command: Some(
                "git status --short; Write-Output '---DIFF---'; git diff; Get-ChildItem -Recurse"
                    .to_owned(),
            ),
            deterministic_impacts: Vec::new(),
            proposed_change: None,
        };
        assert!(repair_inspection_action(&inspection));

        let mutation = ToolAction {
            kind: ActionKind::Execute,
            target_files: Vec::new(),
            command: Some("Get-Content package.json | Set-Content copy.json".to_owned()),
            deterministic_impacts: Vec::new(),
            proposed_change: None,
        };
        assert!(!repair_inspection_action(&mutation));
    }
}
