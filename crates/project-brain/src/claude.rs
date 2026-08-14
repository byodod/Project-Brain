use std::{collections::BTreeMap, path::Path};

use brain_core::{AdapterCapabilities, AdapterKind, BrainConfig};
use brain_store::BrainStore;

use crate::{
    app::HookEvent,
    codex::{self, CodexHookInput, CodexHookOutput},
    error::AppError,
    provider::ProviderTrustStatus,
};

#[cfg(test)]
use crate::provider;

const CLAUDE_CODE_ADAPTER_VERSION: u16 = 1;

/// Claude Code 当前五个同步 lifecycle hook 与 Codex 使用相同的字段子集。
///
/// 保留独立类型别名和 adapter identity，避免把两个 vendor 的幂等域或审计记录混在一起。
pub type ClaudeHookInput = CodexHookInput;
pub type ClaudeHookOutput = CodexHookOutput;

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::claude_code()
}

#[cfg(test)]
pub fn handle(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &ClaudeHookInput,
) -> Result<ClaudeHookOutput, AppError> {
    let provider_trust =
        provider::trust_status(None, root, &config.project_key, &config.semantic_providers);
    handle_with_provider_trust(root, config, store, &provider_trust, event, input)
}

pub fn handle_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &ClaudeHookInput,
) -> Result<ClaudeHookOutput, AppError> {
    codex::handle_vendor_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::ClaudeCode,
        CLAUDE_CODE_ADAPTER_VERSION,
        "claude_code",
    )
}

pub fn failure_output(
    event: HookEvent,
    input: &ClaudeHookInput,
    error: &str,
) -> Option<ClaudeHookOutput> {
    codex::failure_output(event, input, error)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use brain_core::{
        ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, MemoryStatus, Rule, RuleEffect,
        RuleStrength, StopReconcileConfig,
    };
    use brain_store::BrainStore;
    use serde_json::json;

    use super::{ClaudeHookInput, capabilities, handle};
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
    fn claude_pre_tool_denial_uses_a_separate_adapter_audit_domain() {
        let store = BrainStore::open_in_memory().unwrap();
        let input: ClaudeHookInput = serde_json::from_value(json!({
            "session_id": "session",
            "cwd": "C:/repo",
            "tool_name": "Write",
            "tool_use_id": "tool",
            "tool_input": {
                "file_path": ".project-brain/config.json",
                "content": ""
            }
        }))
        .unwrap();

        let output = handle(
            Path::new("C:/repo"),
            &config(),
            &store,
            HookEvent::PreToolUse,
            &input,
        )
        .unwrap();

        assert_eq!(output.0["hookSpecificOutput"]["permissionDecision"], "deny");
        let records = store.recent_adapter_audit("project_a", 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].adapter_kind, "claude_code");
        assert_eq!(
            capabilities(),
            brain_core::AdapterCapabilities::claude_code()
        );
    }
}
