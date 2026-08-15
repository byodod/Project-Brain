use std::{collections::BTreeMap, path::Path};

use brain_core::{AdapterCapabilities, AdapterKind, BrainConfig};
use brain_store::BrainStore;

use crate::{
    app::HookEvent,
    codex::CodexHookInput,
    pi::{self, ExtensionHookOutput},
    provider::ProviderTrustStatus,
};

const DSH_ADAPTER_VERSION: u16 = 2;

pub type DshHookInput = CodexHookInput;
pub type DshHookOutput = ExtensionHookOutput;

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::dsh()
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &DshHookInput,
) -> DshHookOutput {
    pi::handle_adapter_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::Dsh,
        DSH_ADAPTER_VERSION,
        "dsh",
        true,
    )
}

pub fn failure_output(event: HookEvent, input: &DshHookInput, error: &str) -> DshHookOutput {
    pi::adapter_failure_output(event, error, true, input.stop_hook_active())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use brain_core::{
        ActionKind, AdapterCapabilities, Authority, BrainConfig, CURRENT_SCHEMA_VERSION,
        CapabilitySupport, MemoryStatus, Rule, RuleEffect, RuleStrength, StopReconcileConfig,
    };
    use brain_store::BrainStore;
    use serde_json::json;

    use super::{DshHookInput, handle_with_provider_trust};
    use crate::app::HookEvent;

    #[test]
    fn dsh_claims_verified_tool_gate_and_stop_continuation() {
        let capabilities = AdapterCapabilities::dsh();
        assert_eq!(capabilities.deny_tool, CapabilitySupport::Supported);
        assert_eq!(
            capabilities.continue_after_stop,
            CapabilitySupport::Supported
        );
        assert_eq!(capabilities.pre_model_context, CapabilitySupport::Supported);
        assert_eq!(capabilities.native_replan, CapabilitySupport::Emulated);
    }

    #[test]
    fn dsh_replan_is_delivered_on_next_pre_step_before_retry() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_a".to_owned(),
            project_name: "test".to_owned(),
            language_profiles: Vec::new(),
            semantic_providers: Vec::new(),
            finding_effect_mappings: Vec::new(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![Rule {
                id: "REVIEW".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::RequireReview,
                include_paths: vec!["README.md".to_owned()],
                exclude_paths: Vec::new(),
                actions: vec![ActionKind::Create, ActionKind::Modify],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: Vec::new(),
                message: "先重新确认公开安装合同".to_owned(),
                rationale: String::new(),
            }],
        };
        let store = BrainStore::open_in_memory().unwrap();
        let input = |tool_id: &str| -> DshHookInput {
            serde_json::from_value(json!({
                "session_id": "session",
                "cwd": "C:/repo",
                "turn_id": "turn",
                "tool_name": "Write",
                "tool_use_id": tool_id,
                "tool_input": {"file_path":"README.md", "content":"updated"}
            }))
            .unwrap()
        };
        let first = handle_with_provider_trust(
            Path::new("C:/repo"),
            &config,
            &store,
            &BTreeMap::new(),
            HookEvent::PreToolUse,
            &input("tool-1"),
        );
        assert_eq!(first.0["block"], true);
        assert_eq!(first.0["replan"], true);

        let step: DshHookInput = serde_json::from_value(json!({
            "session_id":"session", "turn_id":"turn", "step_id":"step-2"
        }))
        .unwrap();
        let delivery = handle_with_provider_trust(
            Path::new("C:/repo"),
            &config,
            &store,
            &BTreeMap::new(),
            HookEvent::PreStep,
            &step,
        );
        assert!(
            delivery.0["context"]
                .as_array()
                .unwrap()
                .iter()
                .any(|text| { text.as_str().is_some_and(|text| text.contains("replan")) })
        );

        let retry = handle_with_provider_trust(
            Path::new("C:/repo"),
            &config,
            &store,
            &BTreeMap::new(),
            HookEvent::PreToolUse,
            &input("tool-2"),
        );
        assert_eq!(retry.0["block"], false);
    }
}
