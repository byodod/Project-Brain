use std::{
    fs,
    path::{Component, Path},
};

use serde::Serialize;

use crate::{error::AppError, git, provider};

#[derive(Debug, Serialize)]
pub(crate) struct StagedSourceManifest {
    entries: Vec<StagedSourceEntry>,
}

#[derive(Debug, Serialize)]
struct StagedSourceEntry {
    relative_path: String,
    size: u64,
    sha256: String,
}

pub(crate) fn stage_project(
    root: &Path,
    destination: &Path,
) -> Result<StagedSourceManifest, AppError> {
    fs::create_dir_all(destination)?;
    let mut entries = Vec::new();
    for relative_path in git::repository_files(root)? {
        if excluded_source_path(&relative_path) {
            continue;
        }
        let relative = Path::new(&relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AppError::Provider(format!(
                "staging 源路径无效：{relative_path:?}"
            )));
        }
        let source = root.join(relative);
        validate_no_link_components(root, relative)?;
        let metadata = fs::symlink_metadata(&source)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(AppError::Provider(format!(
                "staging 源不是普通文件：{relative_path:?}"
            )));
        }
        let canonical = source.canonicalize()?;
        if !canonical.starts_with(root) {
            return Err(AppError::Provider(format!(
                "staging 源解析后越出项目：{relative_path:?}"
            )));
        }
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&canonical, &target)?;
        let target_metadata = fs::symlink_metadata(&target)?;
        if !target_metadata.is_file() || target_metadata.file_type().is_symlink() {
            return Err(AppError::Provider("staging 目标不是普通文件".to_owned()));
        }
        entries.push(StagedSourceEntry {
            relative_path,
            size: target_metadata.len(),
            sha256: provider::hash_file(&target)?,
        });
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(StagedSourceManifest { entries })
}

pub(crate) fn verify_staged_source(
    manifest: &StagedSourceManifest,
    destination: &Path,
    allowed_extra_paths: &[&str],
) -> Result<(), AppError> {
    let mut expected = manifest
        .entries
        .iter()
        .map(|entry| entry.relative_path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for path in allowed_extra_paths {
        if Path::new(path).is_absolute()
            || Path::new(path)
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || !expected.insert(path)
        {
            return Err(AppError::Provider(
                "staged Source 允许的额外路径无效或重复".to_owned(),
            ));
        }
    }
    for entry in &manifest.entries {
        let path = destination.join(Path::new(&entry.relative_path));
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != entry.size
            || provider::hash_file(&path)? != entry.sha256
        {
            return Err(AppError::Provider(format!(
                "staged Source 在执行期间发生变化：{:?}",
                entry.relative_path
            )));
        }
    }
    let mut directories = vec![destination.to_owned()];
    while let Some(directory) = directories.pop() {
        for child in fs::read_dir(directory)? {
            let path = child?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_link_or_reparse(&metadata) {
                return Err(AppError::Provider(
                    "staged project 中出现 link/reparse".to_owned(),
                ));
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(AppError::Provider(
                    "staged project 中出现非普通文件".to_owned(),
                ));
            }
            let relative = path
                .strip_prefix(destination)
                .map_err(|_| AppError::Provider("staged 文件越出项目".to_owned()))?
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| AppError::Provider("staged 文件路径不是 UTF-8".to_owned()))?
                .join("/");
            if !excluded_source_path(&relative) && !expected.contains(relative.as_str()) {
                return Err(AppError::Provider(format!(
                    "staged Source 出现未声明的新文件：{relative:?}"
                )));
            }
        }
    }
    Ok(())
}

fn excluded_source_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        matches!(
            segment,
            ".git" | ".project-brain" | "bin" | "obj" | "artifacts" | "target"
        )
    })
}

fn validate_no_link_components(root: &Path, relative: &Path) -> Result<(), AppError> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(AppError::Provider("staging 路径组件无效".to_owned()));
        };
        current.push(segment);
        let metadata = fs::symlink_metadata(&current)?;
        if is_link_or_reparse(&metadata) {
            return Err(AppError::Provider(format!(
                "staging 路径包含 link/reparse component：{}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}
