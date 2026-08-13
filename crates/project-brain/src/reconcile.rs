use std::{
    fs,
    path::{Path, PathBuf},
};

use brain_core::{CURRENT_SCHEMA_VERSION, path_has_prefix};
use serde::{Deserialize, Serialize};

use crate::{error::AppError, git};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeEnvelope {
    pub schema_version: u32,
    pub intent: String,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
}

impl ChangeEnvelope {
    pub fn example() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            intent: "描述当前任务允许产生的项目变更".to_owned(),
            allowed_paths: vec![".".to_owned()],
            forbidden_paths: vec![".git".to_owned()],
        }
    }
}

pub fn evaluate_from_path(
    root: &Path,
    base: &str,
    envelope: &Path,
) -> Result<ReconcileReport, AppError> {
    let envelope_path = resolve_envelope_path(root, envelope)?;
    let envelope = serde_json::from_slice(&fs::read(envelope_path)?)?;
    evaluate(root, base, &envelope)
}

fn resolve_envelope_path(root: &Path, envelope: &Path) -> Result<PathBuf, AppError> {
    let root = root.canonicalize()?;
    let candidate = if envelope.is_absolute() {
        envelope.to_owned()
    } else {
        root.join(envelope)
    };
    let candidate = candidate.canonicalize()?;
    if !candidate.starts_with(&root) {
        return Err(AppError::EnvelopeOutsideRoot(candidate));
    }
    Ok(candidate)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileDecision {
    Allow,
    Block,
    Escalate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcileReport {
    pub schema_version: u32,
    pub decision: ReconcileDecision,
    pub summary: String,
    pub base: String,
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpected_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_files: Vec<String>,
}

pub fn evaluate(
    root: &Path,
    base: &str,
    envelope: &ChangeEnvelope,
) -> Result<ReconcileReport, AppError> {
    if envelope.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(brain_core::CoreError::UnsupportedSchemaVersion {
            actual: envelope.schema_version,
            expected: CURRENT_SCHEMA_VERSION,
        }
        .into());
    }

    let changed_files = git::changed_files(root, base)?;
    let forbidden_files = changed_files
        .iter()
        .filter(|path| {
            envelope
                .forbidden_paths
                .iter()
                .any(|prefix| path_has_prefix(path, prefix))
        })
        .cloned()
        .collect::<Vec<_>>();
    let unexpected_files = if envelope.allowed_paths.is_empty() {
        Vec::new()
    } else {
        changed_files
            .iter()
            .filter(|path| {
                !envelope
                    .allowed_paths
                    .iter()
                    .any(|prefix| path_has_prefix(path, prefix))
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    let (decision, summary) = if !forbidden_files.is_empty() {
        (
            ReconcileDecision::Block,
            "变更触及 Change Envelope 明确禁止的范围".to_owned(),
        )
    } else if !unexpected_files.is_empty() {
        (
            ReconcileDecision::Escalate,
            "变更超出声明范围，需要审查或修订 Change Envelope".to_owned(),
        )
    } else {
        (
            ReconcileDecision::Allow,
            "实际变更位于声明范围内".to_owned(),
        )
    };

    Ok(ReconcileReport {
        schema_version: CURRENT_SCHEMA_VERSION,
        decision,
        summary,
        base: base.to_owned(),
        changed_files,
        unexpected_files,
        forbidden_files,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::resolve_envelope_path;
    use crate::error::AppError;

    #[test]
    fn rejects_an_envelope_outside_the_project_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().join(format!(
            "project-brain-envelope-test-{}-{nonce}",
            std::process::id()
        ));
        let root = sandbox.join("repo");
        let outside = sandbox.join("outside.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, "{}").unwrap();

        for envelope in [&outside, Path::new("../outside.json")] {
            let result = resolve_envelope_path(&root, envelope);
            assert!(matches!(result, Err(AppError::EnvelopeOutsideRoot(_))));
        }

        fs::remove_dir_all(sandbox).unwrap();
    }
}
