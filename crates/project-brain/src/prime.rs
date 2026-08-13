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

const PRIME_AGENT_ADAPTER_VERSION: u16 = 1;

/// Prime Agent Extension 将正式 runtime event 规范化到此字段子集。
///
/// 该桥接输入复用已验证的工具归一化逻辑，但保留独立 adapter identity、事件幂等域和输出协议。
pub type PrimeHookInput = CodexHookInput;

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct PrimeHookOutput(pub(crate) Value);

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::prime_agent()
}

#[cfg(test)]
pub fn handle(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &PrimeHookInput,
) -> PrimeHookOutput {
    let provider_trust = BTreeMap::new();
    handle_with_provider_trust(root, config, store, &provider_trust, event, input)
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &PrimeHookInput,
) -> PrimeHookOutput {
    match codex::process_vendor_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::PrimeAgent,
        PRIME_AGENT_ADAPTER_VERSION,
        "prime_agent",
    ) {
        Ok(outcome) => map_outcome(&outcome),
        Err(error) => failure_output(event, &error.to_string()),
    }
}

pub fn failure_output(event: HookEvent, error: &str) -> PrimeHookOutput {
    let reason = format!("Project Brain 治理或审计失败：{error}");
    match event {
        HookEvent::PreToolUse => PrimeHookOutput(json!({
            "schema_version": 1,
            "event": "tool_about_to_run",
            "block": true,
            "reason": reason,
            "context": []
        })),
        HookEvent::Stop => PrimeHookOutput(json!({
            "schema_version": 1,
            "event": "task_stopping",
            "feedback": [reason],
            "continuation": {
                "requested": false,
                "supported": false
            }
        })),
        HookEvent::SessionStart | HookEvent::UserPromptSubmit | HookEvent::PostToolUse => {
            PrimeHookOutput(json!({
                "schema_version": 1,
                "event": event_name(event),
                "degraded": true,
                "feedback": [reason]
            }))
        }
    }
}

fn map_outcome(outcome: &InternalHookOutcome) -> PrimeHookOutput {
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
                    "supported": false,
                    "reason": reason
                }
            })
        }
    };
    PrimeHookOutput(value)
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

    use super::{PrimeHookInput, capabilities, handle};
    use crate::app::HookEvent;

    fn config() -> BrainConfig {
        BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_a".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: Vec::new(),
            semantic_providers: Vec::new(),
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
    fn prime_pre_tool_denial_uses_an_independent_adapter_domain() {
        let store = BrainStore::open_in_memory().unwrap();
        let input: PrimeHookInput = serde_json::from_value(json!({
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
        assert_eq!(records[0].adapter_kind, "prime_agent");
        assert_eq!(
            capabilities().continue_after_stop,
            CapabilitySupport::Unsupported
        );
    }
}
