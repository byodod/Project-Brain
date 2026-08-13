use std::{
    collections::BTreeSet,
    path::Path,
    process::{Command, Output},
};

use brain_core::{CURRENT_SCHEMA_VERSION, normalize_project_path, path_has_prefix};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

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
            allowed_paths: vec!["crates".to_owned(), "README.md".to_owned()],
            forbidden_paths: vec![".git".to_owned()],
        }
    }
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

    let changed_files = git_changed_files(root, base)?;
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

fn git_changed_files(root: &Path, base: &str) -> Result<Vec<String>, AppError> {
    let diff = Command::new("git")
        .current_dir(root)
        .args(["diff", "--name-only", "-z", base, "--"])
        .output()?;
    ensure_git_success(&diff)?;

    let untracked = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()?;
    ensure_git_success(&untracked)?;

    let mut files = parse_null_paths(&diff.stdout);
    files.extend(parse_null_paths(&untracked.stdout));
    Ok(files.into_iter().collect())
}

fn ensure_git_success(output: &Output) -> Result<(), AppError> {
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::Git(
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    ))
}

fn parse_null_paths(bytes: &[u8]) -> BTreeSet<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| normalize_project_path(&String::from_utf8_lossy(path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_null_paths;

    #[test]
    fn parses_and_sorts_null_delimited_git_paths() {
        let paths = parse_null_paths(b"src/z.rs\0src/a.rs\0");
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            vec!["src/a.rs".to_owned(), "src/z.rs".to_owned()]
        );
    }
}
