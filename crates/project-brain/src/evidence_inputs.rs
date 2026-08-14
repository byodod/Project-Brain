use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use brain_evidence::{
    DependencyCoverage, EvidenceInputManifestV1, InputDependencyContractV1, InputManifestEntry,
    InputPathState, InputPathUniverse, InputRole, InputSelectorV1, PathMatcherV1,
    content_fingerprint,
};
use sha2::{Digest, Sha256};

use crate::{error::AppError, git};

const MAX_FILESYSTEM_INPUTS: usize = 100_000;

pub(crate) fn profile_contract_hash(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    content_fingerprint(&bytes)
}

pub(crate) fn conservative_repository_contract(
    project_key: &str,
    profile_id: &str,
    provider_contract_id: &str,
    provider_contract_version: u32,
) -> Result<InputDependencyContractV1, AppError> {
    let profile_hash = profile_contract_hash(&[
        "project-brain/provider-profile/v1",
        project_key,
        profile_id,
        provider_contract_id,
        &provider_contract_version.to_string(),
        "repository-visible:**",
    ]);
    Ok(InputDependencyContractV1::new(
        project_key,
        profile_id,
        provider_contract_id,
        provider_contract_version,
        &profile_hash,
        vec![InputSelectorV1::Tree {
            root: String::new(),
            universe: InputPathUniverse::RepositoryVisible,
            matcher: PathMatcherV1::new(vec!["**".to_owned()], Vec::new())?,
            role: InputRole::Source,
        }],
        DependencyCoverage::Conservative,
    )?)
}

pub(crate) fn python_compile_contract(
    project_key: &str,
    profile_id: &str,
    source_root: &str,
    provider_contract_version: u32,
) -> Result<InputDependencyContractV1, AppError> {
    let normalized_root = if source_root == "." {
        String::new()
    } else {
        source_root.to_owned()
    };
    let profile_hash = profile_contract_hash(&[
        "project-brain/provider-profile/v1",
        project_key,
        profile_id,
        "python-compile",
        &provider_contract_version.to_string(),
        &normalized_root,
        "repository-visible:**/*.py",
    ]);
    Ok(InputDependencyContractV1::new(
        project_key,
        profile_id,
        "python-compile",
        provider_contract_version,
        &profile_hash,
        vec![InputSelectorV1::Tree {
            root: normalized_root,
            universe: InputPathUniverse::RepositoryVisible,
            matcher: PathMatcherV1::new(vec!["**/*.py".to_owned()], Vec::new())?,
            role: InputRole::Source,
        }],
        DependencyCoverage::Complete,
    )?)
}

/// 在整个解析前后验证同一个 Source fingerprint，避免把跨状态拼接的 input manifest
/// 用于 hard authority。
pub(crate) fn resolve_stable(
    root: &Path,
    contract: &InputDependencyContractV1,
) -> Result<EvidenceInputManifestV1, AppError> {
    contract.validate()?;
    let source_before = git::worktree_fingerprint(root)?;
    let entries = resolve_entries(root, contract)?;
    let source_after = git::worktree_fingerprint(root)?;
    if source_before != source_after {
        return Err(AppError::Provider(
            "Evidence input manifest 解析期间 Source fingerprint 发生变化".to_owned(),
        ));
    }
    Ok(EvidenceInputManifestV1::new(
        contract.clone(),
        &source_after,
        entries,
    )?)
}

pub(crate) fn resolve_conservative_for_source(
    root: &Path,
    project_key: &str,
    profile_id: &str,
    provider_contract_id: &str,
    provider_contract_version: u32,
    expected_source_fingerprint: &str,
) -> Result<EvidenceInputManifestV1, AppError> {
    let contract = conservative_repository_contract(
        project_key,
        profile_id,
        provider_contract_id,
        provider_contract_version,
    )?;
    let manifest = resolve_stable(root, &contract)?;
    if manifest.source_fingerprint_at_creation != expected_source_fingerprint {
        return Err(AppError::Provider(format!(
            "Evidence input manifest Source 与运行结果不一致：expected={expected_source_fingerprint}, actual={}",
            manifest.source_fingerprint_at_creation
        )));
    }
    Ok(manifest)
}

fn resolve_entries(
    root: &Path,
    contract: &InputDependencyContractV1,
) -> Result<Vec<InputManifestEntry>, AppError> {
    let root = root.canonicalize()?;
    let needs_repository = contract.selectors.iter().any(|selector| {
        matches!(
            selector,
            InputSelectorV1::Tree {
                universe: InputPathUniverse::RepositoryVisible,
                ..
            }
        )
    });
    let repository_files = if needs_repository {
        git::repository_files(&root)?
    } else {
        Vec::new()
    };
    let mut entries = BTreeMap::<String, InputManifestEntry>::new();
    for selector in &contract.selectors {
        match selector {
            InputSelectorV1::ExactPath {
                path,
                role,
                presence_sensitive,
            } => {
                let candidate = root.join(Path::new(path));
                match regular_file_entry(&root, &candidate, path, *role)? {
                    Some(entry) => insert_entry(&mut entries, entry)?,
                    None if *presence_sensitive => insert_entry(
                        &mut entries,
                        InputManifestEntry {
                            path: path.clone(),
                            state: InputPathState::Absent,
                            role: *role,
                            content_sha256: None,
                            size: None,
                        },
                    )?,
                    None => {}
                }
            }
            InputSelectorV1::Tree {
                root: tree_root,
                universe,
                matcher,
                role,
            } => {
                let candidates = match universe {
                    InputPathUniverse::RepositoryVisible => repository_files
                        .iter()
                        .filter(|path| selector.matches_project_path(path))
                        .cloned()
                        .collect::<Vec<_>>(),
                    InputPathUniverse::ProjectFilesystem => {
                        filesystem_tree_files(&root, tree_root, matcher, MAX_FILESYSTEM_INPUTS)?
                    }
                };
                for path in candidates {
                    let candidate = root.join(Path::new(&path));
                    let entry =
                        regular_file_entry(&root, &candidate, &path, *role)?.ok_or_else(|| {
                            AppError::Provider(format!(
                                "Evidence input selector 命中的文件在解析时消失：{path:?}"
                            ))
                        })?;
                    insert_entry(&mut entries, entry)?;
                }
            }
        }
    }
    Ok(entries.into_values().collect())
}

fn insert_entry(
    entries: &mut BTreeMap<String, InputManifestEntry>,
    entry: InputManifestEntry,
) -> Result<(), AppError> {
    if let Some(existing) = entries.get(&entry.path) {
        if existing != &entry {
            return Err(AppError::Provider(format!(
                "Evidence input path={:?} 被 selector 赋予冲突的 role/state",
                entry.path
            )));
        }
        return Ok(());
    }
    entries.insert(entry.path.clone(), entry);
    Ok(())
}

fn regular_file_entry(
    canonical_root: &Path,
    candidate: &Path,
    display_path: &str,
    role: InputRole,
) -> Result<Option<InputManifestEntry>, AppError> {
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::Provider(format!(
            "Evidence input={display_path:?} 必须是项目内普通文件，拒绝 symlink/reparse/directory/special file"
        )));
    }
    let canonical = candidate.canonicalize()?;
    if !canonical.starts_with(canonical_root) {
        return Err(AppError::RepositoryPathOutsideRoot(canonical));
    }
    let mut file = fs::File::open(&canonical)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut DigestWriter(&mut digest))?;
    Ok(Some(InputManifestEntry {
        path: display_path.to_owned(),
        state: InputPathState::PresentRegularFile,
        role,
        content_sha256: Some(format!("sha256_{:x}", digest.finalize())),
        size: Some(metadata.len()),
    }))
}

fn filesystem_tree_files(
    canonical_root: &Path,
    tree_root: &str,
    matcher: &PathMatcherV1,
    limit: usize,
) -> Result<Vec<String>, AppError> {
    let start = if tree_root.is_empty() {
        canonical_root.to_owned()
    } else {
        canonical_root.join(Path::new(tree_root))
    };
    let metadata = match fs::symlink_metadata(&start) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppError::Provider(format!(
            "Evidence filesystem tree root={tree_root:?} 必须是普通项目目录"
        )));
    }
    let mut directories = vec![start];
    let mut files = BTreeSet::new();
    while let Some(directory) = directories.pop() {
        for child in fs::read_dir(directory)? {
            let child = child?;
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(AppError::Provider(format!(
                    "Evidence filesystem selector 遇到 symlink/reparse：{}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(AppError::Provider(format!(
                    "Evidence filesystem selector 遇到 special file：{}",
                    path.display()
                )));
            }
            let relative = path
                .strip_prefix(canonical_root)
                .map_err(|_| AppError::RepositoryPathOutsideRoot(path.clone()))?;
            let display = normalized_relative_path(relative)?;
            let relative_to_tree = if tree_root.is_empty() {
                display.as_str()
            } else {
                display
                    .strip_prefix(tree_root)
                    .and_then(|value| value.strip_prefix('/'))
                    .ok_or_else(|| AppError::RepositoryPathOutsideRoot(path.clone()))?
            };
            if matcher.matches(relative_to_tree) {
                files.insert(display);
                if files.len() > limit {
                    return Err(AppError::Provider(format!(
                        "Evidence filesystem selector 超过 {limit} 个输入，拒绝不受限扫描"
                    )));
                }
            }
        }
    }
    Ok(files.into_iter().collect())
}

fn normalized_relative_path(path: &Path) -> Result<String, AppError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(AppError::RepositoryPathOutsideRoot(PathBuf::from(path)));
        };
        let part = part.to_str().ok_or(AppError::NonUtf8GitPath)?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

struct DigestWriter<'a, D>(&'a mut D);

impl<D: Digest> std::io::Write for DigestWriter<'_, D> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_evidence::{InputPathState, InputSelectorV1};

    use super::{python_compile_contract, resolve_stable};

    fn repository() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-evidence-inputs-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src/pkg")).unwrap();
        fs::write(root.join("src/main.py"), "print('one')\n").unwrap();
        fs::write(root.join("src/pkg/module.py"), "VALUE = 1\n").unwrap();
        fs::write(root.join("README.md"), "unrelated\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&root)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        root
    }

    #[test]
    fn python_manifest_is_profile_scoped_and_content_bound() {
        let root = repository();
        let contract = python_compile_contract("project-a", "main", "src", 1).unwrap();
        let first = resolve_stable(&root, &contract).unwrap();
        assert_eq!(first.entries.len(), 2);
        assert!(
            first
                .entries
                .iter()
                .all(|entry| entry.state == InputPathState::PresentRegularFile)
        );
        assert!(matches!(
            contract.selectors[0],
            InputSelectorV1::Tree { .. }
        ));

        fs::write(root.join("README.md"), "changed but unrelated\n").unwrap();
        let unrelated = resolve_stable(&root, &contract).unwrap();
        assert_eq!(first.manifest_hash, unrelated.manifest_hash);
        assert_ne!(
            first.source_fingerprint_at_creation,
            unrelated.source_fingerprint_at_creation
        );

        fs::write(root.join("src/main.py"), "print('two')\n").unwrap();
        let changed = resolve_stable(&root, &contract).unwrap();
        assert_ne!(first.manifest_hash, changed.manifest_hash);
        fs::remove_dir_all(root).unwrap();
    }
}
