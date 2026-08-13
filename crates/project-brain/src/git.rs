use std::{
    collections::BTreeSet,
    path::Path,
    process::{Command, Output},
};

use brain_analyzer::LineRange;
use brain_core::normalize_project_path;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffHunk {
    pub old: Option<LineRange>,
    pub new: Option<LineRange>,
}

pub fn changed_files(root: &Path, base: &str) -> Result<Vec<String>, AppError> {
    validate_base(base)?;
    let diff = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "-z",
            base,
            "--",
        ])
        .output()?;
    ensure_success(&diff)?;

    let mut files = parse_null_paths(&diff.stdout);
    files.extend(untracked_files(root)?);
    Ok(files.into_iter().collect())
}

pub fn untracked_files(root: &Path) -> Result<BTreeSet<String>, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()?;
    ensure_success(&output)?;
    Ok(parse_null_paths(&output.stdout))
}

pub fn diff_hunks(root: &Path, base: &str, path: &str) -> Result<Vec<DiffHunk>, AppError> {
    validate_base(base)?;
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=0",
            "--no-color",
            base,
            "--",
            path,
        ])
        .output()?;
    ensure_success(&output)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_hunk_header)
        .collect())
}

pub fn file_at_revision(root: &Path, base: &str, path: &str) -> Result<Option<String>, AppError> {
    validate_base(base)?;
    let spec = format!("{base}:./{}", normalize_project_path(path));
    let output = Command::new("git")
        .current_dir(root)
        .args(["show", "--no-ext-diff", "--format=", &spec])
        .output()?;
    if output.status.success() {
        return Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("does not exist in") || error.contains("exists on disk, but not in") {
        return Ok(None);
    }
    Err(AppError::Git(error.trim().to_owned()))
}

fn validate_base(base: &str) -> Result<(), AppError> {
    if base.is_empty() || base.starts_with('-') || base.chars().any(char::is_whitespace) {
        return Err(AppError::Git(format!("非法的 Git base：{base:?}")));
    }
    Ok(())
}

fn parse_hunk_header(line: &str) -> Option<DiffHunk> {
    let body = line.strip_prefix("@@ ")?;
    let end = body.find(" @@")?;
    let mut ranges = body[..end].split_whitespace();
    let old = parse_diff_range(ranges.next()?, '-').ok()?;
    let new = parse_diff_range(ranges.next()?, '+').ok()?;
    Some(DiffHunk { old, new })
}

fn parse_diff_range(value: &str, marker: char) -> Result<Option<LineRange>, ()> {
    let value = value.strip_prefix(marker).ok_or(())?;
    let (start, count) = value
        .split_once(',')
        .map_or((value, "1"), |(start, count)| (start, count));
    let start = start.parse::<usize>().map_err(|_| ())?;
    let count = count.parse::<usize>().map_err(|_| ())?;
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(LineRange::new(start, start + count - 1)))
}

fn ensure_success(output: &Output) -> Result<(), AppError> {
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
    use super::{DiffHunk, parse_hunk_header, parse_null_paths};
    use brain_analyzer::LineRange;

    #[test]
    fn parses_and_sorts_null_delimited_git_paths() {
        let paths = parse_null_paths(b"src/z.rs\0src/a.rs\0");
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            vec!["src/a.rs".to_owned(), "src/z.rs".to_owned()]
        );
    }

    #[test]
    fn parses_replacement_and_deletion_hunks() {
        assert_eq!(
            parse_hunk_header("@@ -10,2 +12,3 @@ fn example"),
            Some(DiffHunk {
                old: Some(LineRange::new(10, 11)),
                new: Some(LineRange::new(12, 14)),
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -7 +6,0 @@"),
            Some(DiffHunk {
                old: Some(LineRange::new(7, 7)),
                new: None,
            })
        );
    }
}
