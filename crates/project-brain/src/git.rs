use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    process::{Command, Output},
};

use brain_analyzer::LineRange;
use brain_core::normalize_project_path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffHunk {
    pub old: Option<LineRange>,
    pub new: Option<LineRange>,
    pub old_start: usize,
    pub new_start: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeSourceState {
    pub schema_version: u32,
    pub fingerprint: String,
    pub entries: Vec<WorktreeSourceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorktreeSourceEntry {
    pub path: String,
    pub kind: WorktreeSourceEntryKind,
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeSourceEntryKind {
    File,
    Symlink,
    Missing,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceDeltaV1 {
    pub schema_version: u32,
    pub before_fingerprint: String,
    pub after_fingerprint: String,
    pub created_paths: Vec<String>,
    pub modified_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
}

impl SourceDeltaV1 {
    pub fn between(before: &WorktreeSourceState, after: &WorktreeSourceState) -> Self {
        let before_entries = before
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let after_entries = after
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let created_paths = after_entries
            .keys()
            .filter(|path| !before_entries.contains_key(**path))
            .map(|path| (*path).to_owned())
            .collect();
        let deleted_paths = before_entries
            .keys()
            .filter(|path| !after_entries.contains_key(**path))
            .map(|path| (*path).to_owned())
            .collect();
        let modified_paths = before_entries
            .iter()
            .filter_map(|(path, before_entry)| {
                after_entries
                    .get(path)
                    .filter(|after_entry| *after_entry != before_entry)
                    .map(|_| (*path).to_owned())
            })
            .collect();
        Self {
            schema_version: 1,
            before_fingerprint: before.fingerprint.clone(),
            after_fingerprint: after.fingerprint.clone(),
            created_paths,
            modified_paths,
            deleted_paths,
        }
    }

    pub fn changed_paths(&self) -> Vec<String> {
        let mut paths = self.created_paths.clone();
        paths.extend(self.modified_paths.iter().cloned());
        paths.extend(self.deleted_paths.iter().cloned());
        paths.sort();
        paths.dedup();
        paths
    }
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

    let mut files = parse_null_paths(&diff.stdout)?;
    files.extend(untracked_files(root)?);
    Ok(files.into_iter().collect())
}

pub fn untracked_files(root: &Path) -> Result<BTreeSet<String>, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()?;
    ensure_success(&output)?;
    parse_null_paths(&output.stdout)
}

pub fn repository_files(root: &Path) -> Result<Vec<String>, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()?;
    ensure_success(&output)?;
    Ok(parse_null_paths(&output.stdout)?.into_iter().collect())
}

/// 对 Git 已跟踪及未忽略的未跟踪文件建立内容指纹，用于拒绝索引期间发生的源码变化。
pub fn worktree_fingerprint(root: &Path) -> Result<String, AppError> {
    Ok(worktree_source_state(root)?.fingerprint)
}

/// 对 Git 可见工作树建立可重放的内容状态；fingerprint 与 v1
/// `worktree_fingerprint` 字节合同完全一致。
pub fn worktree_source_state(root: &Path) -> Result<WorktreeSourceState, AppError> {
    let root = root.canonicalize()?;
    let mut digest = Sha256::new();
    let mut entries = Vec::new();
    digest.update(b"project-brain/worktree-fingerprint/v1\0");
    for path in repository_files(&root)? {
        let relative = Path::new(&path);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(AppError::RepositoryPathOutsideRoot(relative.to_owned()));
        }
        digest.update(path.as_bytes());
        digest.update([0]);
        let candidate = root.join(relative);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                digest.update(b"missing\0");
                entries.push(WorktreeSourceEntry {
                    path,
                    kind: WorktreeSourceEntryKind::Missing,
                    content_sha256: None,
                });
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            digest.update(b"symlink\0");
            let target = fs::read_link(candidate)?;
            let bytes = target.as_os_str().as_encoded_bytes();
            digest.update(bytes);
            entries.push(WorktreeSourceEntry {
                path,
                kind: WorktreeSourceEntryKind::Symlink,
                content_sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            });
        } else if metadata.is_file() {
            digest.update(b"file\0");
            let mut file = fs::File::open(candidate)?;
            let mut file_digest = Sha256::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
                file_digest.update(&buffer[..read]);
            }
            entries.push(WorktreeSourceEntry {
                path,
                kind: WorktreeSourceEntryKind::File,
                content_sha256: Some(format!("{:x}", file_digest.finalize())),
            });
        } else {
            digest.update(b"other\0");
            entries.push(WorktreeSourceEntry {
                path,
                kind: WorktreeSourceEntryKind::Other,
                content_sha256: None,
            });
        }
        digest.update([0]);
    }
    Ok(WorktreeSourceState {
        schema_version: 1,
        fingerprint: format!("{:x}", digest.finalize()),
        entries,
    })
}

pub fn worktree_is_clean(root: &Path) -> Result<bool, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()?;
    ensure_success(&output)?;
    Ok(output.stdout.is_empty())
}

pub fn head_revision(root: &Path) -> Result<String, AppError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
        .output()?;
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map(|revision| revision.trim().to_owned())
            .map_err(|_| AppError::Git("HEAD revision 不是 UTF-8".to_owned()));
    }

    let symbolic = Command::new("git")
        .current_dir(root)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()?;
    if !symbolic.status.success() {
        return Err(AppError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let reference = String::from_utf8(symbolic.stdout)
        .map_err(|_| AppError::Git("HEAD symbolic ref 不是 UTF-8".to_owned()))?;
    let reference = reference.trim();
    let existing = Command::new("git")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", reference])
        .output()?;
    if existing.status.code() == Some(1) {
        return Ok(format!("unborn:{reference}"));
    }
    Err(AppError::Git(
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    ))
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
        return String::from_utf8(output.stdout)
            .map(Some)
            .map_err(|_| AppError::NonUtf8Source(path.to_owned()));
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
    let (old_start, old) = parse_diff_range(ranges.next()?, '-').ok()?;
    let (new_start, new) = parse_diff_range(ranges.next()?, '+').ok()?;
    Some(DiffHunk {
        old,
        new,
        old_start,
        new_start,
    })
}

fn parse_diff_range(value: &str, marker: char) -> Result<(usize, Option<LineRange>), ()> {
    let value = value.strip_prefix(marker).ok_or(())?;
    let (start, count) = value
        .split_once(',')
        .map_or((value, "1"), |(start, count)| (start, count));
    let start = start.parse::<usize>().map_err(|_| ())?;
    let count = count.parse::<usize>().map_err(|_| ())?;
    if count == 0 {
        return Ok((start, None));
    }
    Ok((start, Some(LineRange::new(start, start + count - 1))))
}

fn ensure_success(output: &Output) -> Result<(), AppError> {
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::Git(
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    ))
}

fn parse_null_paths(bytes: &[u8]) -> Result<BTreeSet<String>, AppError> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec())
                .map(|path| normalize_project_path(&path))
                .map_err(|_| AppError::NonUtf8GitPath)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        DiffHunk, head_revision, parse_hunk_header, parse_null_paths, worktree_fingerprint,
    };
    use brain_analyzer::LineRange;

    fn test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("project-brain-git-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn parses_and_sorts_null_delimited_git_paths() {
        let paths = parse_null_paths(b"src/z.rs\0src/a.rs\0").unwrap();
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
                old_start: 10,
                new_start: 12,
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -7 +6,0 @@"),
            Some(DiffHunk {
                old: Some(LineRange::new(7, 7)),
                new: None,
                old_start: 7,
                new_start: 6,
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -10,0 +11,2 @@"),
            Some(DiffHunk {
                old: None,
                new: Some(LineRange::new(11, 12)),
                old_start: 10,
                new_start: 11,
            })
        );
    }

    #[test]
    fn rejects_non_utf8_paths_instead_of_aliasing_them() {
        assert!(parse_null_paths(b"src/\xff.rs\0").is_err());
    }

    #[test]
    fn represents_an_unborn_head_without_requiring_a_commit() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        let init = Command::new("git")
            .current_dir(&root)
            .arg("init")
            .output()
            .unwrap();
        assert!(init.status.success());

        let revision = head_revision(&root).unwrap();
        assert!(revision.starts_with("unborn:refs/heads/"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worktree_fingerprint_tracks_repository_content_but_not_ignored_files() {
        let root = test_root();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .current_dir(&root)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
        fs::write(root.join("source.rs"), "fn one() {}\n").unwrap();
        let first = worktree_fingerprint(&root).unwrap();
        fs::write(root.join("ignored.log"), "diagnostic\n").unwrap();
        assert_eq!(first, worktree_fingerprint(&root).unwrap());
        fs::write(root.join("source.rs"), "fn two() {}\n").unwrap();
        assert_ne!(first, worktree_fingerprint(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
