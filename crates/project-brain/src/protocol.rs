use std::{collections::BTreeMap, path::Path};

use brain_core::{
    ActionDescriptor, BrainConfig, CURRENT_SCHEMA_VERSION, ContextItem, Decision, DecisionKind,
    FeedbackItem, FeedbackSeverity, GateDecision, HOOK_PROTOCOL_VERSION, HookEventPayload,
    HookOutcomePayload, InternalHookEvent, InternalHookOutcome, RuleEngine, StopDecision,
    ToolAction,
};

use crate::{app::decision_reason, error::AppError, reconcile};

pub fn process(
    root: &Path,
    config: &BrainConfig,
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
            let decision = evaluate_action(config, event, &tool.action, &tool.tool_name)?;
            HookOutcomePayload::ToolAboutToRun {
                gate: gate_from_decision(&decision),
                inject: context_from_decision(&decision),
            }
        }
        HookEventPayload::ToolFinished(tool) => {
            let decision = evaluate_action(config, event, &tool.action, &tool.tool_name)?;
            HookOutcomePayload::ToolFinished {
                feedback: feedback_from_decision(&decision),
            }
        }
        HookEventPayload::TaskStopping(stopping) => HookOutcomePayload::TaskStopping {
            stop: stop_decision(root, config, stopping.vendor_loop_active),
            feedback: Vec::new(),
        },
    };
    Ok(InternalHookOutcome {
        protocol_version: HOOK_PROTOCOL_VERSION,
        event_id: event.event_id.clone(),
        payload,
    })
}

fn evaluate_action(
    config: &BrainConfig,
    event: &InternalHookEvent,
    action: &ToolAction,
    tool_name: &str,
) -> Result<Decision, AppError> {
    Ok(RuleEngine::new(config)?.evaluate(&ActionDescriptor {
        schema_version: CURRENT_SCHEMA_VERSION,
        event_id: event.event_id.clone(),
        session_id: event.session_key.clone(),
        cwd: event.cwd.clone(),
        action: action.kind,
        operation: tool_name.to_owned(),
        target_files: action.target_files.clone(),
        command: action.command.clone(),
        metadata: BTreeMap::new(),
    })?)
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

fn stop_decision(root: &Path, config: &BrainConfig, vendor_loop_active: bool) -> StopDecision {
    if vendor_loop_active || !config.stop_reconcile.enabled {
        return StopDecision::AllowStop;
    }
    let report = match reconcile::evaluate_from_path(
        root,
        &config.stop_reconcile.base,
        Path::new(&config.stop_reconcile.envelope),
    ) {
        Ok(report) => report,
        Err(error) => {
            return StopDecision::ContinueWork {
                reason: format!("Project Brain Stop 对账失败：{error}"),
            };
        }
    };
    match report.decision {
        reconcile::ReconcileDecision::Allow => StopDecision::AllowStop,
        reconcile::ReconcileDecision::Block | reconcile::ReconcileDecision::Escalate => {
            let details = if report.forbidden_files.is_empty() {
                &report.unexpected_files
            } else {
                &report.forbidden_files
            };
            StopDecision::ContinueWork {
                reason: format!(
                    "Project Brain Stop 对账未通过：{}。涉及：{}",
                    report.summary,
                    details.join(", ")
                ),
            }
        }
    }
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
