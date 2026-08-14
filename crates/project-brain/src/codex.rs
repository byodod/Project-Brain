use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use brain_core::{
    AdapterCapabilities, AdapterIdentity, AdapterKind, BrainConfig, ContextItem,
    EventIdentityQuality, FeedbackItem, GateDecision, HOOK_PROTOCOL_VERSION, HookEventPayload,
    HookOutcomePayload, IdempotencyMetadata, IntentDeclared, IntentOrigin, InternalHookEvent,
    InternalHookOutcome, SessionOpenReason, SessionOpened, StopDecision, TaskStopping,
    ToolAboutToRun, ToolAction, ToolFinished, ToolStatus, normalize_project_path,
};
use brain_store::{AdapterRecordResult, BrainStore};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::provider;
use crate::provider::ProviderTrustStatus;
use crate::{app::HookEvent, error::AppError, protocol};

const CODEX_ADAPTER_VERSION: u16 = 1;
static DELIVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Deserialize)]
pub struct CodexHookInput {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    turn_id: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    tool_response: Value,
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    stop_hook_active: bool,
}

impl CodexHookInput {
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn stop_hook_active(&self) -> bool {
        self.stop_hook_active
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct CodexHookOutput(pub(crate) Value);

pub const fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities::codex()
}

#[cfg(test)]
pub fn handle(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    event: HookEvent,
    input: &CodexHookInput,
) -> Result<CodexHookOutput, AppError> {
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
    input: &CodexHookInput,
) -> Result<CodexHookOutput, AppError> {
    handle_vendor_with_provider_trust(
        root,
        config,
        store,
        provider_trust,
        event,
        input,
        AdapterKind::Codex,
        CODEX_ADAPTER_VERSION,
        "codex",
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_vendor_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &CodexHookInput,
    adapter_kind: AdapterKind,
    adapter_version: u16,
    identity_namespace: &'static str,
) -> Result<CodexHookOutput, AppError> {
    match process_vendor_with_provider_trust(
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
        Ok(outcome) => Ok(map_outcome(&outcome)),
        Err(error) => {
            if let Some(output) = failure_output(event, input, &error.to_string()) {
                return Ok(output);
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_vendor_with_provider_trust(
    root: &Path,
    config: &BrainConfig,
    store: &BrainStore,
    provider_trust: &BTreeMap<String, ProviderTrustStatus>,
    event: HookEvent,
    input: &CodexHookInput,
    adapter_kind: AdapterKind,
    adapter_version: u16,
    identity_namespace: &'static str,
) -> Result<InternalHookOutcome, AppError> {
    let started = Instant::now();
    let internal_event = to_internal_event(
        root,
        config,
        event,
        input,
        adapter_kind,
        adapter_version,
        identity_namespace,
    )?;
    let outcome = match protocol::process(root, config, store, provider_trust, &internal_event) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = store.record_adapter_failure(
                &internal_event,
                elapsed_millis(started),
                &error.to_string(),
            );
            return Err(error);
        }
    };
    let outcome =
        match store.record_adapter_event(&internal_event, &outcome, elapsed_millis(started)) {
            Ok(result) => match result {
                AdapterRecordResult::Inserted(_) => outcome,
                AdapterRecordResult::Duplicate(first_outcome) => first_outcome,
            },
            Err(error) => {
                return Err(error.into());
            }
        };
    Ok(outcome)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn to_internal_event(
    root: &Path,
    config: &BrainConfig,
    event: HookEvent,
    input: &CodexHookInput,
    adapter_kind: AdapterKind,
    adapter_version: u16,
    identity_namespace: &'static str,
) -> Result<InternalHookEvent, AppError> {
    let cwd = if input.cwd.trim().is_empty() {
        root.to_string_lossy().into_owned()
    } else {
        input.cwd.clone()
    };
    let session_key = if input.session_id.trim().is_empty() {
        hash_id(
            &format!("{identity_namespace}_session"),
            &[&config.project_key, &cwd],
        )
    } else {
        input.session_id.clone()
    };
    let turn_key = (!input.turn_id.trim().is_empty()).then(|| input.turn_id.clone());
    let payload = match event {
        HookEvent::SessionStart => HookEventPayload::SessionOpened(SessionOpened {
            reason: session_reason(&input.source),
            previous_session_key: None,
        }),
        HookEvent::UserPromptSubmit => HookEventPayload::IntentDeclared(IntentDeclared {
            text: input.prompt.clone(),
            origin: IntentOrigin::Interactive,
        }),
        HookEvent::PreToolUse => {
            let (operation_id, tool_name, action) = normalized_tool(
                root,
                &config.project_key,
                &session_key,
                input,
                identity_namespace,
            );
            HookEventPayload::ToolAboutToRun(ToolAboutToRun {
                operation_id,
                tool_name,
                action,
            })
        }
        HookEvent::PostToolUse => {
            let (operation_id, tool_name, action) = normalized_tool(
                root,
                &config.project_key,
                &session_key,
                input,
                identity_namespace,
            );
            HookEventPayload::ToolFinished(ToolFinished {
                operation_id,
                tool_name,
                action,
                status: tool_status(&input.tool_response),
                duration_ms: None,
            })
        }
        HookEvent::Stop => HookEventPayload::TaskStopping(TaskStopping {
            last_assistant_message: input.last_assistant_message.clone(),
            vendor_loop_active: input.stop_hook_active,
        }),
    };
    let (event_id, identity_quality) = event_identity(
        event,
        input,
        &config.project_key,
        &session_key,
        identity_namespace,
    )?;
    let event = InternalHookEvent {
        protocol_version: HOOK_PROTOCOL_VERSION,
        project_key: config.project_key.clone(),
        event_id,
        idempotency: IdempotencyMetadata { identity_quality },
        adapter: AdapterIdentity {
            kind: adapter_kind,
            adapter_version,
        },
        session_key,
        cwd,
        turn_key,
        payload,
    };
    event.validate()?;
    Ok(event)
}

fn event_identity(
    event: HookEvent,
    input: &CodexHookInput,
    project_key: &str,
    session_key: &str,
    identity_namespace: &'static str,
) -> Result<(String, EventIdentityQuality), AppError> {
    let event_name = match event {
        HookEvent::SessionStart => "session_opened",
        HookEvent::UserPromptSubmit => "intent_declared",
        HookEvent::PreToolUse => "tool_about_to_run",
        HookEvent::PostToolUse => "tool_finished",
        HookEvent::Stop => "task_stopping",
    };
    let stable_vendor_key = match event {
        HookEvent::PreToolUse | HookEvent::PostToolUse if !input.tool_use_id.trim().is_empty() => {
            Some((
                input.tool_use_id.as_str(),
                EventIdentityQuality::VendorStable,
            ))
        }
        HookEvent::UserPromptSubmit if !input.turn_id.trim().is_empty() => {
            Some((input.turn_id.as_str(), EventIdentityQuality::DerivedStable))
        }
        HookEvent::SessionStart
        | HookEvent::UserPromptSubmit
        | HookEvent::PreToolUse
        | HookEvent::PostToolUse
        | HookEvent::Stop => None,
    };
    if let Some((stable_key, quality)) = stable_vendor_key {
        return Ok((
            hash_id_bytes(
                &format!("{identity_namespace}_event"),
                &[
                    project_key.as_bytes(),
                    session_key.as_bytes(),
                    event_name.as_bytes(),
                    stable_key.as_bytes(),
                ],
            ),
            quality,
        ));
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = DELIVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok((
        hash_id_bytes(
            &format!("{identity_namespace}_event"),
            &[
                project_key.as_bytes(),
                session_key.as_bytes(),
                event_name.as_bytes(),
                &nonce.to_le_bytes(),
                &sequence.to_le_bytes(),
                &std::process::id().to_le_bytes(),
            ],
        ),
        EventIdentityQuality::PerDelivery,
    ))
}

fn normalized_tool(
    root: &Path,
    project_key: &str,
    session_key: &str,
    input: &CodexHookInput,
    identity_namespace: &'static str,
) -> (String, String, ToolAction) {
    let command = input
        .tool_input
        .get("command")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let kind = classify_action(
        root,
        &input.tool_name,
        &input.tool_input,
        command.as_deref(),
    );
    let mut target_files = extract_target_files(&input.tool_input);
    if let Some(command) = command.as_deref() {
        target_files.extend(extract_destructive_command_paths(command));
    }
    target_files.sort();
    target_files.dedup();
    let target_files = target_files
        .into_iter()
        .map(|target| make_project_relative(root, &target))
        .collect::<Vec<_>>();
    let action = ToolAction {
        kind,
        target_files,
        command,
        deterministic_impacts: deterministic_impacts(root, input),
    };
    let operation_id = if input.tool_use_id.trim().is_empty() {
        let action_json = serde_json::to_vec(&action).unwrap_or_default();
        hash_id_bytes(
            &format!("{identity_namespace}_operation"),
            &[
                input.session_id.as_bytes(),
                project_key.as_bytes(),
                session_key.as_bytes(),
                input.turn_id.as_bytes(),
                input.tool_name.as_bytes(),
                &action_json,
            ],
        )
    } else {
        hash_id(
            &format!("{identity_namespace}_operation"),
            &[project_key, session_key, &input.tool_use_id],
        )
    };
    (operation_id, input.tool_name.clone(), action)
}

fn session_reason(source: &str) -> SessionOpenReason {
    match source.to_ascii_lowercase().as_str() {
        "startup" => SessionOpenReason::Startup,
        "resume" => SessionOpenReason::Resume,
        "clear" => SessionOpenReason::Clear,
        "compact" => SessionOpenReason::Compact,
        _ => SessionOpenReason::Unknown,
    }
}

fn tool_status(response: &Value) -> ToolStatus {
    let Some(object) = response.as_object() else {
        return ToolStatus::Unknown;
    };
    if let Some(success) = object.get("success").and_then(Value::as_bool) {
        return if success {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Failed
        };
    }
    if let Some(exit_code) = object.get("exit_code").and_then(Value::as_i64) {
        return if exit_code == 0 {
            ToolStatus::Succeeded
        } else {
            ToolStatus::Failed
        };
    }
    if object.get("error").is_some_and(|error| !error.is_null()) {
        return ToolStatus::Failed;
    }
    match object
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("success" | "succeeded" | "completed" | "ok") => ToolStatus::Succeeded,
        Some("error" | "failed" | "cancelled" | "timed_out") => ToolStatus::Failed,
        _ => ToolStatus::Unknown,
    }
}

fn map_outcome(outcome: &InternalHookOutcome) -> CodexHookOutput {
    let output = match &outcome.payload {
        HookOutcomePayload::SessionOpened { inject } => context_output("SessionStart", inject),
        HookOutcomePayload::IntentDeclared { gate, inject } => match gate {
            GateDecision::Deny { reason } => json!({
                "decision": "block",
                "reason": reason
            }),
            GateDecision::NoVeto if inject.is_empty() => json!({}),
            GateDecision::NoVeto => context_output("UserPromptSubmit", inject),
        },
        HookOutcomePayload::ToolAboutToRun { gate, inject } => match gate {
            GateDecision::Deny { reason } => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason
                }
            }),
            GateDecision::NoVeto if inject.is_empty() => json!({}),
            GateDecision::NoVeto => context_output("PreToolUse", inject),
        },
        HookOutcomePayload::ToolFinished { feedback } if feedback.is_empty() => json!({}),
        HookOutcomePayload::ToolFinished { feedback } => json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": join_feedback(feedback)
            }
        }),
        HookOutcomePayload::TaskStopping { stop, feedback: _ } => match stop {
            StopDecision::AllowStop => json!({ "continue": true }),
            StopDecision::ContinueWork { reason } => json!({
                "decision": "block",
                "reason": reason
            }),
        },
    };
    CodexHookOutput(output)
}

pub fn failure_output(
    event: HookEvent,
    input: &CodexHookInput,
    error: &str,
) -> Option<CodexHookOutput> {
    let reason = format!("Project Brain 治理或审计失败，拒绝默认放行：{error}");
    match event {
        HookEvent::PreToolUse => Some(CodexHookOutput(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        }))),
        HookEvent::Stop if input.stop_hook_active => {
            Some(CodexHookOutput(json!({ "continue": true })))
        }
        HookEvent::Stop => Some(CodexHookOutput(json!({
            "decision": "block",
            "reason": reason
        }))),
        HookEvent::SessionStart | HookEvent::UserPromptSubmit | HookEvent::PostToolUse => None,
    }
}

fn context_output(hook_event_name: &str, items: &[ContextItem]) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": hook_event_name,
            "additionalContext": items
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        }
    })
}

fn join_feedback(items: &[FeedbackItem]) -> String {
    items
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn hash_id(label: &str, parts: &[&str]) -> String {
    hash_id_bytes(
        label,
        &parts.iter().map(|part| part.as_bytes()).collect::<Vec<_>>(),
    )
}

fn hash_id_bytes(label: &str, parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"project-brain/internal-hook/v1\0");
    digest.update(label.as_bytes());
    for part in parts {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(part);
    }
    format!("{label}_{:x}", digest.finalize())
}

fn classify_action(
    root: &Path,
    tool_name: &str,
    tool_input: &Value,
    command: Option<&str>,
) -> brain_core::ActionKind {
    use brain_core::ActionKind;

    let tool = tool_name.to_ascii_lowercase();
    if tool == "write" {
        let target = extract_target_files(tool_input).into_iter().next();
        return if target.is_some_and(|path| root.join(make_project_relative(root, &path)).exists())
        {
            ActionKind::Modify
        } else {
            ActionKind::Create
        };
    }
    if matches!(tool.as_str(), "apply_patch" | "edit") {
        let command = command.unwrap_or_default();
        if command.contains("*** Delete File:") {
            return ActionKind::Delete;
        }
        if command.contains("*** Add File:") && !command.contains("*** Update File:") {
            return ActionKind::Create;
        }
        return ActionKind::Modify;
    }
    if tool == "str_replace_editor" {
        return match command.unwrap_or_default().to_ascii_lowercase().as_str() {
            "view" => ActionKind::Read,
            "create" => ActionKind::Create,
            "str_replace" | "insert" => ActionKind::Modify,
            _ => ActionKind::Unknown,
        };
    }
    if matches!(tool.as_str(), "bash" | "pwsh" | "shell_command") {
        let command = command.unwrap_or_default().to_ascii_lowercase();
        if !extract_destructive_command_paths(&command).is_empty() {
            return ActionKind::Delete;
        }
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

fn extract_destructive_command_paths(command: &str) -> Vec<String> {
    let tokens = shell_like_tokens(command);
    let mut paths = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].to_ascii_lowercase();
        let (is_delete, operands_start) = if matches!(
            token.as_str(),
            "rm" | "unlink" | "del" | "erase" | "remove-item"
        ) {
            (true, index + 1)
        } else if token == "git"
            && tokens
                .get(index + 1)
                .is_some_and(|next| next.eq_ignore_ascii_case("rm"))
        {
            (true, index + 2)
        } else {
            (false, index + 1)
        };
        if !is_delete {
            index += 1;
            continue;
        }
        index = operands_start;
        while index < tokens.len() && tokens[index] != ";" {
            let candidate = &tokens[index];
            if !candidate.starts_with('-') && literal_command_path(candidate) {
                paths.push(candidate.to_owned());
            }
            index += 1;
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn literal_command_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['*', '?', '$', '%', '`', '\n', '\r'])
        && !matches!(value.to_ascii_lowercase().as_str(), "--" | "true" | "false")
}

fn shell_like_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in command.chars() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else {
                current.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else if matches!(character, ';' | '|' | '&') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if tokens.last().is_none_or(|last| last != ";") {
                tokens.push(";".to_owned());
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
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

fn deterministic_impacts(root: &Path, input: &CodexHookInput) -> Vec<brain_core::ToolImpact> {
    let tool = input.tool_name.to_ascii_lowercase();
    let editor_command = input
        .tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if tool == "write" || (tool == "str_replace_editor" && editor_command == "create") {
        return extract_target_files(&input.tool_input)
            .into_iter()
            .map(|path| brain_core::ToolImpact {
                path: make_project_relative(root, &path),
                whole_file: true,
                ranges: Vec::new(),
            })
            .collect();
    }
    if tool == "apply_patch" {
        let mut impacts = Vec::new();
        if let Some(command) = input.tool_input.get("command").and_then(Value::as_str) {
            for line in command.lines() {
                for marker in ["*** Add File:", "*** Delete File:"] {
                    if let Some(path) = line.trim().strip_prefix(marker) {
                        impacts.push(brain_core::ToolImpact {
                            path: make_project_relative(root, path.trim()),
                            whole_file: true,
                            ranges: Vec::new(),
                        });
                    }
                }
            }
        }
        impacts.sort_by(|left, right| left.path.cmp(&right.path));
        impacts.dedup_by(|left, right| left.path == right.path);
        return impacts;
    }
    if tool == "str_replace_editor" && editor_command == "insert" {
        let path = input.tool_input.get("path").and_then(Value::as_str);
        let line = input
            .tool_input
            .get("insert_line")
            .and_then(Value::as_u64)
            .and_then(|line| usize::try_from(line).ok());
        if let (Some(path), Some(line)) = (path, line) {
            return vec![brain_core::ToolImpact {
                path: make_project_relative(root, path),
                whole_file: false,
                ranges: vec![brain_core::ToolLineRange {
                    start_line: line.saturating_add(1),
                    end_line: line.saturating_add(1),
                }],
            }];
        }
    }
    if tool == "edit" || (tool == "str_replace_editor" && editor_command == "str_replace") {
        let path = input
            .tool_input
            .get("file_path")
            .or_else(|| input.tool_input.get("path"))
            .and_then(Value::as_str);
        let old = input
            .tool_input
            .get("old_string")
            .or_else(|| input.tool_input.get("old_str"))
            .and_then(Value::as_str);
        if let (Some(path), Some(old)) = (path, old)
            && !old.is_empty()
        {
            let relative = make_project_relative(root, path);
            let absolute = root.join(&relative);
            if let Ok(source) = std::fs::read_to_string(absolute) {
                let matches = source.match_indices(old).collect::<Vec<_>>();
                if matches.len() == 1 {
                    let offset = matches[0].0;
                    let start_line = source[..offset]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1;
                    let end_line = start_line + old.bytes().filter(|byte| *byte == b'\n').count();
                    return vec![brain_core::ToolImpact {
                        path: relative,
                        whole_file: false,
                        ranges: vec![brain_core::ToolLineRange {
                            start_line,
                            end_line,
                        }],
                    }];
                }
            }
        }
    }
    Vec::new()
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
    use std::{
        fs,
        path::Path,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_core::{
        ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, HookEventPayload,
        InternalHookEvent, MemoryStatus, Rule, RuleEffect, RuleStrength, StopReconcileConfig,
        ToolStatus,
    };
    use brain_evidence::{
        EvidenceAuthority, EvidenceCoverage, EvidenceFreshness, EvidencePlane, EvidenceProvider,
        EvidenceSnapshot,
    };
    use brain_store::BrainStore;
    use serde_json::json;

    use super::{
        CodexHookInput, classify_action, deterministic_impacts, extract_destructive_command_paths,
        extract_target_files, failure_output, handle,
    };
    use crate::app::HookEvent;

    fn config(project_key: &str) -> BrainConfig {
        BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: project_key.to_owned(),
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
                actions: vec![ActionKind::Delete],
                operations: Vec::new(),
                operation_contains: Vec::new(),
                symbol_scopes: Vec::new(),
                message: "protected".to_owned(),
                rationale: String::new(),
            }],
        }
    }

    fn delete_input() -> CodexHookInput {
        CodexHookInput {
            session_id: "session".to_owned(),
            cwd: "C:/repo".to_owned(),
            turn_id: "turn".to_owned(),
            tool_name: "apply_patch".to_owned(),
            tool_use_id: "tool".to_owned(),
            tool_input: json!({
                "command": "*** Begin Patch\n*** Delete File: .project-brain/config.json\n*** End Patch"
            }),
            ..CodexHookInput::default()
        }
    }

    fn engine_snapshot(project_key: &str) -> EvidenceSnapshot {
        engine_snapshot_for_source(project_key, "sha256_source-test")
    }

    fn engine_snapshot_for_source(project_key: &str, source_fingerprint: &str) -> EvidenceSnapshot {
        EvidenceSnapshot::new(
            project_key,
            EvidencePlane::Engine,
            EvidenceProvider {
                id: "engine-resolver".to_owned(),
                version: "4.6+sha256.test".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            source_fingerprint,
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn source_snapshot(project_key: &str) -> EvidenceSnapshot {
        EvidenceSnapshot::new(
            project_key,
            EvidencePlane::Source,
            EvidenceProvider {
                id: "git-source".to_owned(),
                version: "1.0+sha256.test".to_owned(),
                contract_version: 1,
                authority: EvidenceAuthority::Deterministic,
            },
            "sha256_source-test",
            EvidenceCoverage::Complete,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

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
        let root = Path::new("C:/repo");
        assert_eq!(
            classify_action(
                root,
                "shell_command",
                &json!({"command": "Remove-Item file.txt"}),
                Some("Remove-Item file.txt"),
            ),
            ActionKind::Delete
        );
        assert_eq!(
            classify_action(
                root,
                "shell_command",
                &json!({"command": "git status"}),
                Some("git status"),
            ),
            ActionKind::GitOperation
        );
        assert_eq!(
            extract_destructive_command_paths(
                "git status && git rm '.project-brain/config.json'; Remove-Item -Force other.txt"
            ),
            vec![
                ".project-brain/config.json".to_owned(),
                "other.txt".to_owned()
            ]
        );
        assert_eq!(
            classify_action(
                root,
                "str_replace_editor",
                &json!({"command": "str_replace", "path": "src/lib.rs"}),
                Some("str_replace"),
            ),
            ActionKind::Modify
        );
        assert_eq!(
            classify_action(
                root,
                "pwsh",
                &json!({"command": "Remove-Item file.txt"}),
                Some("Remove-Item file.txt"),
            ),
            ActionKind::Delete
        );
    }

    #[test]
    fn codex_pre_tool_hook_denies_without_approving_vendor_permissions() {
        let store = BrainStore::open_in_memory().unwrap();
        let output = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &delete_input(),
        )
        .unwrap();

        assert_eq!(output.0["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn codex_no_veto_emits_no_vendor_permission_approval() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut input = delete_input();
        input.tool_use_id = "allowed-tool".to_owned();
        input.tool_input = json!({ "path": "README.md" });
        let output = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &input,
        )
        .unwrap();

        assert_eq!(output.0, json!({}));
    }

    #[test]
    fn codex_no_veto_with_context_does_not_approve_vendor_permissions() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut config = config("project_a");
        config.rules[0].effect = RuleEffect::InjectContext;
        let output = handle(
            Path::new("C:/repo"),
            &config,
            &store,
            HookEvent::PreToolUse,
            &delete_input(),
        )
        .unwrap();

        assert!(
            output.0["hookSpecificOutput"]
                .get("additionalContext")
                .is_some()
        );
        assert!(
            output.0["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
    }

    #[test]
    fn codex_post_tool_violation_is_feedback_not_a_retroactive_block() {
        let store = BrainStore::open_in_memory().unwrap();
        let output = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &delete_input(),
        )
        .unwrap();

        assert!(output.0.get("decision").is_none());
        assert_eq!(
            output.0["hookSpecificOutput"]["hookEventName"],
            "PostToolUse"
        );
    }

    #[test]
    fn codex_post_tool_records_failed_response_status() {
        let store = BrainStore::open_in_memory().unwrap();
        let mut input = delete_input();
        input.tool_response = json!({ "exit_code": 1 });
        handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &input,
        )
        .unwrap();
        let records = store.recent_adapter_audit("project_a", 10).unwrap();
        let event: InternalHookEvent = serde_json::from_str(&records[0].event_json).unwrap();
        let HookEventPayload::ToolFinished(finished) = event.payload else {
            panic!("期望 ToolFinished")
        };
        assert_eq!(finished.status, ToolStatus::Failed);
    }

    #[test]
    fn successful_post_tool_mutation_marks_source_and_downstream_evidence_stale() {
        let store = BrainStore::open_in_memory().unwrap();
        let engine = engine_snapshot("project_a");
        store
            .apply_evidence_snapshot_for_current_source(&engine, &engine.source_fingerprint)
            .unwrap();
        let source = source_snapshot("project_a");
        store
            .apply_evidence_snapshot_for_current_source(&source, &source.source_fingerprint)
            .unwrap();
        let input = CodexHookInput {
            session_id: "session".to_owned(),
            cwd: "C:/repo".to_owned(),
            turn_id: "turn".to_owned(),
            tool_name: "Write".to_owned(),
            tool_use_id: "write-tool".to_owned(),
            tool_input: json!({ "path": "src/main.rs" }),
            tool_response: json!({ "success": true }),
            ..CodexHookInput::default()
        };

        let output = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &input,
        )
        .unwrap();
        assert!(
            output.0["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("已标记为 stale")
        );
        let heads = store.list_evidence_heads("project_a").unwrap();
        assert_eq!(heads.len(), 2);
        assert!(
            heads
                .iter()
                .all(|head| head.freshness == EvidenceFreshness::Stale)
        );

        let session = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::SessionStart,
            &CodexHookInput {
                session_id: "next-session".to_owned(),
                source: "startup".to_owned(),
                ..CodexHookInput::default()
            },
        )
        .unwrap();
        assert!(
            session.0["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("freshness=stale")
        );
    }

    #[test]
    fn opaque_post_tool_preserves_matching_evidence_and_stales_source_drift() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-codex-opaque-source-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        assert!(
            Command::new("git")
                .current_dir(&root)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let source_fingerprint = crate::git::worktree_fingerprint(&root).unwrap();
        let store = BrainStore::open_in_memory().unwrap();
        let engine = engine_snapshot_for_source("project_a", &source_fingerprint);
        store
            .apply_evidence_snapshot_for_current_source(&engine, &source_fingerprint)
            .unwrap();

        let read_only_shell = CodexHookInput {
            session_id: "session".to_owned(),
            cwd: root.to_string_lossy().into_owned(),
            turn_id: "turn-1".to_owned(),
            tool_name: "shell_command".to_owned(),
            tool_use_id: "shell-read".to_owned(),
            tool_input: json!({ "command": "git status" }),
            tool_response: json!({ "exit_code": 0 }),
            ..CodexHookInput::default()
        };
        handle(
            &root,
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &read_only_shell,
        )
        .unwrap();
        assert_eq!(
            store.list_evidence_heads("project_a").unwrap()[0].freshness,
            EvidenceFreshness::Fresh
        );

        fs::write(
            root.join("src/main.rs"),
            "fn main() { println!(\"changed\"); }\n",
        )
        .unwrap();
        let mut mutating_shell = read_only_shell;
        mutating_shell.turn_id = "turn-2".to_owned();
        mutating_shell.tool_use_id = "shell-write".to_owned();
        mutating_shell.tool_input = json!({ "command": "opaque generator invocation" });
        let output = handle(
            &root,
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &mutating_shell,
        )
        .unwrap();
        assert!(
            output.0["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("已标记为 stale")
        );
        assert_eq!(
            store.list_evidence_heads("project_a").unwrap()[0].freshness,
            EvidenceFreshness::Stale
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opaque_post_tool_with_unverifiable_source_removes_hard_authority() {
        let store = BrainStore::open_in_memory().unwrap();
        let engine = engine_snapshot("project_a");
        store
            .apply_evidence_snapshot_for_current_source(&engine, &engine.source_fingerprint)
            .unwrap();
        let input = CodexHookInput {
            session_id: "session".to_owned(),
            cwd: "Z:/missing-project-brain-repository".to_owned(),
            turn_id: "turn".to_owned(),
            tool_name: "shell_command".to_owned(),
            tool_use_id: "opaque-tool".to_owned(),
            tool_input: json!({ "command": "opaque command" }),
            tool_response: json!({ "exit_code": 1 }),
            ..CodexHookInput::default()
        };
        let output = handle(
            Path::new("Z:/missing-project-brain-repository"),
            &config("project_a"),
            &store,
            HookEvent::PostToolUse,
            &input,
        )
        .unwrap();
        assert!(
            output.0["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("标记为 unknown")
        );
        assert_eq!(
            store.list_evidence_heads("project_a").unwrap()[0].freshness,
            EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn session_and_intent_outputs_follow_their_codex_contracts() {
        let store = BrainStore::open_in_memory().unwrap();
        let session = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::SessionStart,
            &CodexHookInput {
                session_id: "session".to_owned(),
                source: "startup".to_owned(),
                ..CodexHookInput::default()
            },
        )
        .unwrap();
        assert_eq!(
            session.0["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );
        assert!(
            session.0["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("project_key=project_a")
        );

        let intent = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::UserPromptSubmit,
            &CodexHookInput {
                session_id: "session".to_owned(),
                turn_id: "turn".to_owned(),
                prompt: "修改 README".to_owned(),
                ..CodexHookInput::default()
            },
        )
        .unwrap();
        assert_eq!(intent.0, json!({}));
        assert!(
            store
                .recent_adapter_audit("project_a", 10)
                .unwrap()
                .iter()
                .any(|record| record.event_kind == "intent_declared")
        );
    }

    #[test]
    fn critical_hook_errors_have_explicit_fail_closed_outputs() {
        let input = CodexHookInput::default();
        let pre = failure_output(HookEvent::PreToolUse, &input, "database unavailable").unwrap();
        assert_eq!(pre.0["hookSpecificOutput"]["permissionDecision"], "deny");
        let stop = failure_output(HookEvent::Stop, &input, "database unavailable").unwrap();
        assert_eq!(stop.0["decision"], "block");
        assert!(failure_output(HookEvent::PostToolUse, &input, "failure").is_none());
    }

    #[test]
    fn duplicate_delivery_reuses_the_first_outcome_and_audit_row() {
        let store = BrainStore::open_in_memory().unwrap();
        let input = delete_input();
        let first = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &input,
        )
        .unwrap();
        let second = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &input,
        )
        .unwrap();
        assert_eq!(first.0, second.0);
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn vendor_tool_identity_rejects_a_changed_retry_payload() {
        let store = BrainStore::open_in_memory().unwrap();
        let first = delete_input();
        let mut retry = delete_input();
        retry.tool_input = json!({ "path": "README.md" });

        let denied = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &first,
        )
        .unwrap();
        let replayed = handle(
            Path::new("C:/repo"),
            &config("project_a"),
            &store,
            HookEvent::PreToolUse,
            &retry,
        )
        .unwrap();
        assert_ne!(denied.0, replayed.0);
        assert_eq!(
            denied.0["hookSpecificOutput"]["permissionDecisionReason"],
            "PROTECT: protected"
        );
        assert!(
            replayed.0["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("event_id 已用于不同事件"))
        );
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn identical_vendor_ids_are_isolated_by_project_key() {
        let store = BrainStore::open_in_memory().unwrap();
        let input = delete_input();
        for project_key in ["project_a", "project_b"] {
            handle(
                Path::new("C:/repo"),
                &config(project_key),
                &store,
                HookEvent::PreToolUse,
                &input,
            )
            .unwrap();
        }
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            1
        );
        assert_eq!(
            store.recent_adapter_audit("project_b", 10).unwrap().len(),
            1
        );
        let operation_ids = ["project_a", "project_b"].map(|project_key| {
            let record = store
                .recent_adapter_audit(project_key, 1)
                .unwrap()
                .pop()
                .unwrap();
            let event: InternalHookEvent = serde_json::from_str(&record.event_json).unwrap();
            let HookEventPayload::ToolAboutToRun(tool) = event.payload else {
                panic!("期望 ToolAboutToRun")
            };
            tool.operation_id
        });
        assert_ne!(operation_ids[0], operation_ids[1]);
    }

    #[test]
    fn active_stop_hook_does_not_start_a_reconcile_loop() {
        let store = BrainStore::open_in_memory().unwrap();
        let input = CodexHookInput {
            stop_hook_active: true,
            ..CodexHookInput::default()
        };
        let output = handle(
            Path::new("Z:/not/a/repository"),
            &config("project_a"),
            &store,
            HookEvent::Stop,
            &input,
        )
        .unwrap();
        assert_eq!(output.0, json!({ "continue": true }));
    }

    #[test]
    fn stop_without_a_vendor_delivery_id_is_not_cached_across_invocations() {
        let store = BrainStore::open_in_memory().unwrap();
        let input = CodexHookInput {
            turn_id: "turn".to_owned(),
            stop_hook_active: true,
            ..CodexHookInput::default()
        };
        for _ in 0..2 {
            handle(
                Path::new("Z:/not/a/repository"),
                &config("project_a"),
                &store,
                HookEvent::Stop,
                &input,
            )
            .unwrap();
        }
        assert_eq!(
            store.recent_adapter_audit("project_a", 10).unwrap().len(),
            2
        );
    }

    #[test]
    fn structured_write_and_delete_have_whole_file_impacts_but_patch_update_does_not() {
        let root = std::env::temp_dir().join("project-brain-structured-impact-root");
        let write_path = root.join("src/lib.rs");
        let write = CodexHookInput {
            tool_name: "Write".to_owned(),
            tool_input: json!({"file_path": write_path, "content": "new"}),
            ..CodexHookInput::default()
        };
        let write_impacts = deterministic_impacts(&root, &write);
        assert_eq!(write_impacts.len(), 1);
        assert_eq!(write_impacts[0].path, "src/lib.rs");
        assert!(write_impacts[0].whole_file);

        let delete = CodexHookInput {
            tool_name: "apply_patch".to_owned(),
            tool_input: json!({"command": "*** Begin Patch\n*** Delete File: src/lib.rs\n*** End Patch"}),
            ..CodexHookInput::default()
        };
        assert!(deterministic_impacts(&root, &delete)[0].whole_file);

        let update = CodexHookInput {
            tool_name: "apply_patch".to_owned(),
            tool_input: json!({"command": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"}),
            ..CodexHookInput::default()
        };
        assert!(deterministic_impacts(&root, &update).is_empty());
    }

    #[test]
    fn edit_impact_requires_a_unique_old_string() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("project-brain-codex-edit-{nonce}"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "one\ntarget\nthree\n").unwrap();
        let unique = CodexHookInput {
            tool_name: "Edit".to_owned(),
            tool_input: json!({"file_path": root.join("src/lib.rs"), "old_string": "target", "new_string": "changed"}),
            ..CodexHookInput::default()
        };
        let impacts = deterministic_impacts(&root, &unique);
        assert_eq!(impacts[0].ranges[0].start_line, 2);
        assert_eq!(impacts[0].ranges[0].end_line, 2);

        fs::write(root.join("src/lib.rs"), "target\ntarget\n").unwrap();
        assert!(deterministic_impacts(&root, &unique).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
