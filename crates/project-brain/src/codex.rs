use std::{collections::BTreeMap, path::Path};

use brain_core::{
    ActionDescriptor, ActionKind, BrainConfig, CURRENT_SCHEMA_VERSION, DecisionKind, RuleEngine,
    normalize_project_path,
};
use brain_store::BrainStore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    app::{HookEvent, decision_reason, should_deny},
    error::AppError,
};

#[derive(Debug, Deserialize)]
pub struct CodexHookInput {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    hook_event_name: String,
    #[serde(default)]
    turn_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    tool_input: Value,
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct CodexHookOutput(Value);

pub fn handle(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &CodexHookInput,
) -> Result<CodexHookOutput, AppError> {
    match event {
        HookEvent::SessionStart => Ok(session_start(config)),
        HookEvent::PreToolUse | HookEvent::PostToolUse => {
            evaluate_tool(root, config, store, event, input)
        }
        HookEvent::Stop => Ok(CodexHookOutput(json!({ "continue": true }))),
    }
}

fn session_start(config: &BrainConfig) -> CodexHookOutput {
    let active_rules = config
        .rules
        .iter()
        .filter(|rule| rule.status == brain_core::MemoryStatus::Active)
        .map(|rule| format!("- {}: {}", rule.id, rule.message))
        .collect::<Vec<_>>()
        .join("\n");
    let context = format!(
        "Project Brain 已加载项目 {}。当前有效规则：\n{}",
        config.project_name, active_rules
    );
    CodexHookOutput(json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context
        }
    }))
}

fn evaluate_tool(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &CodexHookInput,
) -> Result<CodexHookOutput, AppError> {
    let action = to_action(root, input);
    let decision = RuleEngine::new(config)?.evaluate(&action)?;
    let event_name = match event {
        HookEvent::PreToolUse => "pre_tool_use",
        HookEvent::PostToolUse => "post_tool_use",
        _ => unreachable!("evaluate_tool 仅处理工具 Hook"),
    };
    store.record(event_name, &action, &decision)?;
    let reason = decision_reason(&decision);

    let output = match event {
        HookEvent::PreToolUse if should_deny(&decision) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        }),
        HookEvent::PreToolUse if decision.decision == DecisionKind::AllowWithContext => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": reason
            }
        }),
        HookEvent::PostToolUse if should_deny(&decision) => json!({
            "decision": "block",
            "reason": reason,
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": "该操作已经执行；Project Brain 正在阻止其结果被视为已完成。"
            }
        }),
        HookEvent::PostToolUse if decision.decision == DecisionKind::AllowWithContext => json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": reason
            }
        }),
        HookEvent::PreToolUse | HookEvent::PostToolUse => json!({}),
        _ => unreachable!("evaluate_tool 仅处理工具 Hook"),
    };

    Ok(CodexHookOutput(output))
}

fn to_action(root: &Path, input: &CodexHookInput) -> ActionDescriptor {
    let command = input
        .tool_input
        .get("command")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let action = classify_action(&input.tool_name, command.as_deref());
    let mut target_files = extract_target_files(&input.tool_input);
    target_files.sort();
    target_files.dedup();
    let target_files = target_files
        .into_iter()
        .map(|target| make_project_relative(root, &target))
        .collect();

    let metadata = BTreeMap::from([
        ("turn_id".to_owned(), Value::String(input.turn_id.clone())),
        (
            "hook_event_name".to_owned(),
            Value::String(input.hook_event_name.clone()),
        ),
    ]);

    ActionDescriptor {
        schema_version: CURRENT_SCHEMA_VERSION,
        event_id: input.tool_use_id.clone(),
        session_id: input.session_id.clone(),
        cwd: input.cwd.clone(),
        action,
        operation: input.tool_name.clone(),
        target_files,
        command,
        metadata,
    }
}

fn classify_action(tool_name: &str, command: Option<&str>) -> ActionKind {
    let tool = tool_name.to_ascii_lowercase();
    if matches!(tool.as_str(), "apply_patch" | "edit" | "write") {
        let command = command.unwrap_or_default();
        if command.contains("*** Delete File:") {
            return ActionKind::Delete;
        }
        if command.contains("*** Add File:") && !command.contains("*** Update File:") {
            return ActionKind::Create;
        }
        return ActionKind::Modify;
    }
    if tool == "bash" {
        let command = command.unwrap_or_default().to_ascii_lowercase();
        if command.contains("git ") || command.starts_with("git") {
            return ActionKind::GitOperation;
        }
        if command.contains("remove-item")
            || command.contains("rm ")
            || command.contains("unlink ")
            || command.contains("del ")
            || command.contains("erase ")
        {
            return ActionKind::Delete;
        }
        return ActionKind::Execute;
    }
    if tool.contains("read") || tool.contains("search") || tool.contains("find") {
        return ActionKind::Read;
    }
    ActionKind::Unknown
}

fn extract_target_files(tool_input: &Value) -> Vec<String> {
    let mut targets = Vec::new();
    if let Some(object) = tool_input.as_object() {
        extract_named_paths(object, &mut targets);
        if let Some(command) = object.get("command").and_then(Value::as_str) {
            extract_patch_paths(command, &mut targets);
        }
    }
    targets
}

fn extract_named_paths(object: &Map<String, Value>, targets: &mut Vec<String>) {
    for key in ["path", "file", "file_path", "target_file"] {
        if let Some(path) = object.get(key).and_then(Value::as_str) {
            targets.push(path.to_owned());
        }
    }
    if let Some(paths) = object.get("paths").and_then(Value::as_array) {
        targets.extend(
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned),
        );
    }
}

fn extract_patch_paths(command: &str, targets: &mut Vec<String>) {
    for line in command.lines() {
        for marker in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(path) = line.trim().strip_prefix(marker) {
                targets.push(path.trim().to_owned());
            }
        }
    }
}

fn make_project_relative(root: &Path, path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute()
        && let Ok(relative) = candidate.strip_prefix(root)
    {
        return normalize_project_path(&relative.to_string_lossy());
    }
    normalize_project_path(path)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use brain_core::{
        Authority, BrainConfig, CURRENT_SCHEMA_VERSION, MemoryStatus, Rule, RuleEffect,
        RuleStrength,
    };
    use brain_store::BrainStore;
    use serde_json::json;

    use super::{CodexHookInput, classify_action, extract_target_files, handle};
    use crate::app::HookEvent;
    use brain_core::ActionKind;

    #[test]
    fn extracts_all_apply_patch_file_markers() {
        let input = json!({
            "command": "*** Begin Patch\n*** Update File: src/main.rs\n*** Add File: src/new.rs\n*** End Patch"
        });
        assert_eq!(
            extract_target_files(&input),
            vec!["src/main.rs".to_owned(), "src/new.rs".to_owned()]
        );
    }

    #[test]
    fn classifies_destructive_shell_commands() {
        assert_eq!(
            classify_action("Bash", Some("Remove-Item file.txt")),
            ActionKind::Delete
        );
        assert_eq!(
            classify_action("Bash", Some("git status")),
            ActionKind::GitOperation
        );
    }

    #[test]
    fn codex_pre_tool_hook_denies_deleting_a_protected_file() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_name: "test".to_owned(),
            rules: vec![Rule {
                id: "PROTECT".to_owned(),
                status: MemoryStatus::Active,
                authority: Authority::RepositoryRule,
                strength: RuleStrength::Hard,
                effect: RuleEffect::Block,
                include_paths: vec![".project-brain/config.json".to_owned()],
                exclude_paths: Vec::new(),
                actions: vec![ActionKind::Delete],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                message: "protected".to_owned(),
                rationale: String::new(),
            }],
        };
        let store = BrainStore::open_in_memory().unwrap();
        let input = CodexHookInput {
            session_id: "session".to_owned(),
            cwd: "C:/repo".to_owned(),
            hook_event_name: "PreToolUse".to_owned(),
            turn_id: "turn".to_owned(),
            tool_name: "apply_patch".to_owned(),
            tool_use_id: "tool".to_owned(),
            tool_input: json!({
                "command": "*** Begin Patch\n*** Delete File: .project-brain/config.json\n*** End Patch"
            }),
        };

        let output = handle(
            Path::new("C:/repo"),
            &config,
            &store,
            HookEvent::PreToolUse,
            &input,
        )
        .unwrap();

        assert_eq!(output.0["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(store.audit_count().unwrap(), 1);
    }
}
