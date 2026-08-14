use std::{collections::BTreeMap, path::Path};

use brain_core::{
    AdapterCapabilities, AdapterKind, BrainConfig, GateDecision, HookOutcomePayload,
    InternalHookOutcome, StopDecision,
};
use brain_store::BrainStore;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    app::HookEvent,
    codex::{self, CodexHookInput},
    provider::ProviderTrustStatus,
};

const PI_ADAPTER_VERSION: u16 = 1;

/// PI Extension 将正式 runtime event 规范化到此字段子集。
///
/// 该桥接输入复用已验证的工具归一化逻辑，但保留独立 adapter identity、事件幂等域和输出协议。
pub type PiHookInput = CodexHookInput;

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ExtensionHookOutput(pub(crate) Value);

pub type PiHookOutput = ExtensionHookOutput;

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::pi()
}

#[cfg(test)]
pub fn handle(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &PiHookInput,
) -> PiHookOutput {
    let provider_trust = BTreeMap::new();
    handle_with_provider_trust(root, config, store, &provider_trust, event, input)
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &PiHookInput,
) -> PiHookOutput {
    handle_adapter_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::Pi,
        PI_ADAPTER_VERSION,
        "pi",
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_adapter_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &CodexHookInput,
    adapter_kind: AdapterKind,
    adapter_version: u16,
    identity_namespace: &'static str,
    continuation_supported: bool,
) -> ExtensionHookOutput {
    match codex::process_vendor_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        adapter_kind,
        adapter_version,
        identity_namespace,
    ) {
        Ok(outcome) => map_outcome(&outcome, continuation_supported),
        Err(error) => adapter_failure_output(
            event,
            &error.to_string(),
            continuation_supported,
            input.stop_hook_active(),
        ),
    }
}

pub fn failure_output(event: HookEvent, input: &PiHookInput, error: &str) -> PiHookOutput {
    adapter_failure_output(event, error, true, input.stop_hook_active())
}

pub(crate) fn adapter_failure_output(
    event: HookEvent,
    error: &str,
    continuation_supported: bool,
    stop_hook_active: bool,
) -> ExtensionHookOutput {
    let reason = format!("Project Brain 治理或审计失败：{error}");
    match event {
        HookEvent::PreToolUse => ExtensionHookOutput(json!({
            "schema_version": 1,
            "event": "tool_about_to_run",
            "block": true,
            "reason": reason,
            "context": []
        })),
        HookEvent::Stop => ExtensionHookOutput(json!({
            "schema_version": 1,
            "event": "task_stopping",
            "feedback": [reason],
            "continuation": {
                "requested": continuation_supported && !stop_hook_active,
                "supported": continuation_supported,
                "reason": if continuation_supported && !stop_hook_active {
                    Some(reason.as_str())
                } else {
                    None
                }
            }
        })),
        HookEvent::SessionStart | HookEvent::UserPromptSubmit | HookEvent::PostToolUse => {
            ExtensionHookOutput(json!({
                "schema_version": 1,
                "event": event_name(event),
                "degraded": true,
                "feedback": [reason]
            }))
        }
    }
}

fn map_outcome(outcome: &InternalHookOutcome, continuation_supported: bool) -> ExtensionHookOutput {
    let value = match &outcome.payload {
        HookOutcomePayload::SessionOpened { inject } => json!({
            "schema_version": 1,
            "event": "session_opened",
            "context": context_text(inject)
        }),
        HookOutcomePayload::IntentDeclared { gate, inject } => {
            let context = context_text(inject);
            gate_output("intent_declared", gate, &context)
        }
        HookOutcomePayload::ToolAboutToRun { gate, inject } => {
            let context = context_text(inject);
            gate_output("tool_about_to_run", gate, &context)
        }
        HookOutcomePayload::ToolFinished { feedback } => json!({
            "schema_version": 1,
            "event": "tool_finished",
            "feedback": feedback.iter().map(|item| item.text.as_str()).collect::<Vec<_>>()
        }),
        HookOutcomePayload::TaskStopping { stop, feedback } => {
            let (requested, reason) = match stop {
                StopDecision::AllowStop => (false, None),
                StopDecision::ContinueWork { reason } => (true, Some(reason.as_str())),
            };
            json!({
                "schema_version": 1,
                "event": "task_stopping",
                "feedback": feedback.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(),
                "continuation": {
                    "requested": requested,
                    "supported": continuation_supported,
                    "reason": reason
                }
            })
        }
    };
    ExtensionHookOutput(value)
}

fn gate_output(event: &str, gate: &GateDecision, context: &[&str]) -> Value {
    match gate {
        GateDecision::NoVeto => json!({
            "schema_version": 1,
            "event": event,
            "block": false,
            "context": context
        }),
        GateDecision::Deny { reason } => json!({
            "schema_version": 1,
            "event": event,
            "block": true,
            "reason": reason,
            "context": context
        }),
    }
}

fn context_text(items: &[brain_core::ContextItem]) -> Vec<&str> {
    items.iter().map(|item| item.text.as_str()).collect()
}

const fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "session_opened",
        HookEvent::UserPromptSubmit => "intent_declared",
        HookEvent::PreToolUse => "tool_about_to_run",
        HookEvent::PostToolUse => "tool_finished",
        HookEvent::Stop => "task_stopping",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use brain_core::{
        ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, CapabilitySupport,
        MemoryStatus, Rule, RuleEffect, RuleStrength, StopReconcileConfig,
    };
    use brain_store::BrainStore;
    use serde_json::json;

    use super::{PiHookInput, capabilities, handle};
    use crate::app::HookEvent;

    fn config() -> BrainConfig {
        BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_a".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: Vec::new(),
            semantic_providers: Vec::new(),
            finding_effect_mappings: Vec::new(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![Rule {
                id: "PROTECT".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::Block,
                include_paths: vec![".project-brain/config.json".to_owned()],
                exclude_paths: Vec::new(),
                actions: vec![ActionKind::Modify],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: Vec::new(),
                message: "protected".to_owned(),
                rationale: String::new(),
            }],
        }
    }

    #[test]
    fn pi_pre_tool_denial_uses_an_independent_adapter_domain() {
        let store = BrainStore::open_in_memory().unwrap();
        let input: PiHookInput = serde_json::from_value(json!({
            "session_id": "session",
            "cwd": "C:/repo",
            "tool_name": "edit",
            "tool_use_id": "tool",
            "tool_input": {
                "path": ".project-brain/config.json",
                "oldText": "old",
                "newText": "new"
            }
        }))
        .unwrap();

        let output = handle(
            Path::new("C:/repo"),
            &config(),
            &store,
            HookEvent::PreToolUse,
            &input,
        );

        assert_eq!(output.0["block"], true);
        let records = store.recent_adapter_audit("project_a", 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].adapter_kind, "pi");
        assert_eq!(
            capabilities().continue_after_stop,
            CapabilitySupport::Emulated
        );
    }
}
