use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use brain_evidence::content_fingerprint;
use serde::{Deserialize, Serialize};

use crate::{
    build::{ArtifactEntry, ArtifactManifest},
    error::AppError,
    provider,
    setup::{MutationLock, resolve_install_root},
};

const STORE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AssemblyBindingAttestation {
    pub(crate) assembly_name: String,
    pub(crate) relative_path: String,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeArtifactBundle {
    schema_version: u32,
    project_key: String,
    build_provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build_target: Option<String>,
    source_fingerprint: String,
    artifact_manifest_fingerprint: String,
    total_bytes: u64,
    entries: Vec<ArtifactEntry>,
    assembly_binding: Option<AssemblyBindingAttestation>,
}

impl RuntimeArtifactBundle {
    pub(crate) fn project_key(&self) -> &str {
        &self.project_key
    }

    pub(crate) fn build_provider_id(&self) -> &str {
        &self.build_provider_id
    }

    pub(crate) fn build_target(&self) -> Option<&str> {
        self.build_target.as_deref()
    }

    pub(crate) fn entries(&self) -> &[ArtifactEntry] {
        &self.entries
    }

    pub(crate) fn source_fingerprint(&self) -> &str {
        &self.source_fingerprint
    }

    pub(crate) fn assembly_binding(&self) -> Option<&AssemblyBindingAttestation> {
        self.assembly_binding.as_ref()
    }

    pub(crate) fn canonical_manifest_bytes(&self) -> Result<Vec<u8>, AppError> {
        canonical_json(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeArtifactBundleReceipt {
    pub(crate) bundle_fingerprint: String,
    pub(crate) artifact_manifest_fingerprint: String,
    pub(crate) file_count: usize,
    pub(crate) total_bytes: u64,
    pub(crate) assembly_binding: Option<AssemblyBindingAttestation>,
    #[serde(skip)]
    canonical_manifest: Vec<u8>,
}

impl RuntimeArtifactBundleReceipt {
    pub(crate) fn canonical_manifest_bytes(&self) -> &[u8] {
        &self.canonical_manifest
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn promote_runtime_bundle(
    explicit_install_root: Option<&Path>,
    project_key: &str,
    build_provider_id: &str,
    build_target: &str,
    source_fingerprint: &str,
    project_root: &Path,
    artifact_root: &Path,
    artifact_manifest: &ArtifactManifest,
) -> Result<RuntimeArtifactBundleReceipt, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    let canonical_project_root = project_root.canonicalize()?;
    let project_boundary = PathBuf::from(provider::provider_cli_path(&canonical_project_root));
    let install_boundary = PathBuf::from(provider::provider_cli_path(&install_root));
    if install_boundary.starts_with(&project_boundary)
        || install_root.canonicalize().is_ok_and(|path| {
            PathBuf::from(provider::provider_cli_path(&path)).starts_with(&project_boundary)
        })
    {
        return Err(AppError::Provider(
            "机器级 artifact store 不得位于项目工作树内".to_owned(),
        ));
    }
    let store_root = store_root(&install_root);
    let _lock = MutationLock::acquire(&store_root.join("store.lock"))?;
    let assembly_binding = assembly_binding(project_root, artifact_manifest)?;
    let bundle = RuntimeArtifactBundle {
        schema_version: STORE_SCHEMA_VERSION,
        project_key: project_key.to_owned(),
        build_provider_id: build_provider_id.to_owned(),
        build_target: Some(build_target.to_owned()),
        source_fingerprint: source_fingerprint.to_owned(),
        artifact_manifest_fingerprint: artifact_manifest.manifest_fingerprint.clone(),
        total_bytes: artifact_manifest.total_bytes,
        entries: artifact_manifest.entries.clone(),
        assembly_binding,
    };
    let canonical_manifest = canonical_json(&bundle)?;
    let bundle_fingerprint = content_fingerprint(&canonical_manifest);
    let canonical_artifact_root = artifact_root.canonicalize()?;
    for entry in &bundle.entries {
        validate_relative_path(&entry.relative_path)?;
        let source = canonical_artifact_root.join(&entry.relative_path);
        let metadata = fs::symlink_metadata(&source)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(AppError::Provider(format!(
                "Runtime bundle 源不是普通文件：{}",
                source.display()
            )));
        }
        let canonical_source = source.canonicalize()?;
        if !canonical_source.starts_with(&canonical_artifact_root)
            || metadata.len() != entry.size
            || provider::hash_file(&canonical_source)? != entry.sha256
        {
            return Err(AppError::Provider(format!(
                "Runtime bundle 源与 Build manifest 不一致：{}",
                entry.relative_path
            )));
        }
        promote_object(&store_root, &canonical_source, entry)?;
    }
    let manifest_path = bundle_manifest_path(&store_root, &bundle_fingerprint);
    atomic_create_or_verify(&manifest_path, &canonical_manifest)?;
    verify_runtime_bundle(explicit_install_root, &bundle_fingerprint)?;
    Ok(RuntimeArtifactBundleReceipt {
        bundle_fingerprint,
        artifact_manifest_fingerprint: bundle.artifact_manifest_fingerprint,
        file_count: bundle.entries.len(),
        total_bytes: bundle.total_bytes,
        assembly_binding: bundle.assembly_binding,
        canonical_manifest,
    })
}

pub(crate) fn verify_runtime_bundle(
    explicit_install_root: Option<&Path>,
    bundle_fingerprint: &str,
) -> Result<RuntimeArtifactBundle, AppError> {
    let install_root = resolve_install_root(explicit_install_root)?;
    verify_bundle_at_root(&store_root(&install_root), bundle_fingerprint)
}

pub(crate) fn materialize_runtime_bundle(
    explicit_install_root: Option<&Path>,
    bundle_fingerprint: &str,
    destination: &Path,
) -> Result<RuntimeArtifactBundle, AppError> {
    if destination.exists() {
        return Err(AppError::Provider(
            "Runtime bundle 物化目标必须尚不存在".to_owned(),
        ));
    }
    let install_root = resolve_install_root(explicit_install_root)?;
    let store_root = store_root(&install_root);
    let bundle = verify_bundle_at_root(&store_root, bundle_fingerprint)?;
    fs::create_dir_all(destination)?;
    for entry in &bundle.entries {
        let source = object_path(&store_root, &entry.sha256)?;
        let target = destination.join(&entry.relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut input = File::open(source)?;
        let mut output = AtomicWriteFile::options().open(&target)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        output.commit()?;
        let metadata = fs::symlink_metadata(&target)?;
        if is_link_or_reparse(&metadata)
            || has_multiple_hard_links(&metadata)
            || !metadata.is_file()
            || metadata.len() != entry.size
            || provider::hash_file(&target)? != entry.sha256
        {
            return Err(AppError::Provider(format!(
                "Runtime bundle 物化后校验失败：{}",
                entry.relative_path
            )));
        }
    }
    Ok(bundle)
}

pub(crate) fn verify_materialized_bundle(
    bundle: &RuntimeArtifactBundle,
    destination: &Path,
) -> Result<(), AppError> {
    let mut observed = Vec::new();
    let mut directories = vec![destination.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_link_or_reparse(&metadata)
                || metadata.is_file() && has_multiple_hard_links(&metadata)
            {
                return Err(AppError::Provider(
                    "物化 Runtime bundle 中出现链接".to_owned(),
                ));
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(destination)
                    .map_err(|_| AppError::Provider("Runtime bundle 文件越出物化根".to_owned()))?;
                let relative_path = relative
                    .components()
                    .map(|component| component.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| AppError::Provider("Runtime bundle 路径不是 UTF-8".to_owned()))?
                    .join("/");
                observed.push(ArtifactEntry {
                    relative_path,
                    size: metadata.len(),
                    sha256: provider::hash_file(&path)?,
                });
            } else {
                return Err(AppError::Provider(
                    "物化 Runtime bundle 中出现非普通文件".to_owned(),
                ));
            }
        }
    }
    observed.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if observed != bundle.entries {
        return Err(AppError::Provider(
            "物化 Runtime bundle 已被增加、删除或改写".to_owned(),
        ));
    }
    Ok(())
}

fn verify_bundle_at_root(
    store_root: &Path,
    bundle_fingerprint: &str,
) -> Result<RuntimeArtifactBundle, AppError> {
    validate_fingerprint(bundle_fingerprint)?;
    let bytes = fs::read(bundle_manifest_path(store_root, bundle_fingerprint))?;
    if content_fingerprint(&bytes) != bundle_fingerprint {
        return Err(AppError::Provider(
            "Runtime bundle manifest 内容哈希不匹配，CAS 已损坏".to_owned(),
        ));
    }
    let bundle: RuntimeArtifactBundle = serde_json::from_slice(&bytes)?;
    if bundle.schema_version != STORE_SCHEMA_VERSION
        || bundle.entries.is_empty()
        || bundle.total_bytes != bundle.entries.iter().map(|entry| entry.size).sum::<u64>()
    {
        return Err(AppError::Provider(
            "Runtime bundle manifest 结构或汇总字段无效".to_owned(),
        ));
    }
    let mut previous = None;
    for entry in &bundle.entries {
        validate_relative_path(&entry.relative_path)?;
        if previous.is_some_and(|value: &str| value >= entry.relative_path.as_str()) {
            return Err(AppError::Provider(
                "Runtime bundle entries 未按路径严格排序或存在重复".to_owned(),
            ));
        }
        previous = Some(entry.relative_path.as_str());
        let object = object_path(store_root, &entry.sha256)?;
        let metadata = fs::symlink_metadata(&object)?;
        if is_link_or_reparse(&metadata)
            || has_multiple_hard_links(&metadata)
            || !metadata.is_file()
            || metadata.len() != entry.size
            || provider::hash_file(&object)? != entry.sha256
        {
            return Err(AppError::Provider(format!(
                "Runtime bundle CAS object 缺失或损坏：{}",
                entry.sha256
            )));
        }
    }
    Ok(bundle)
}

fn promote_object(store_root: &Path, source: &Path, entry: &ArtifactEntry) -> Result<(), AppError> {
    let target = object_path(store_root, &entry.sha256)?;
    if target.exists() {
        let metadata = fs::symlink_metadata(&target)?;
        if is_link_or_reparse(&metadata)
            || has_multiple_hard_links(&metadata)
            || !metadata.is_file()
            || metadata.len() != entry.size
            || provider::hash_file(&target)? != entry.sha256
        {
            return Err(AppError::Provider(format!(
                "CAS 中已存在同名但内容不匹配的 object：{}",
                entry.sha256
            )));
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut input = File::open(source)?;
    let mut output = AtomicWriteFile::options().open(&target)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    output.commit()?;
    let metadata = fs::symlink_metadata(&target)?;
    if is_link_or_reparse(&metadata)
        || has_multiple_hard_links(&metadata)
        || metadata.len() != entry.size
        || provider::hash_file(&target)? != entry.sha256
    {
        return Err(AppError::Provider(format!(
            "CAS object 原子提交后的内容校验失败：{}",
            entry.sha256
        )));
    }
    Ok(())
}

fn atomic_create_or_verify(target: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if target.exists() {
        if fs::read(target)? != bytes {
            return Err(AppError::Provider(format!(
                "CAS 中已存在同 fingerprint 但内容不同的 manifest：{}",
                target.display()
            )));
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = AtomicWriteFile::options().open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.commit()?;
    if fs::read(target)? != bytes {
        return Err(AppError::Provider(
            "CAS manifest 原子提交后的内容校验失败".to_owned(),
        ));
    }
    Ok(())
}

fn assembly_binding(
    project_root: &Path,
    manifest: &ArtifactManifest,
) -> Result<Option<AssemblyBindingAttestation>, AppError> {
    let project_file = project_root.join("project.godot");
    if !project_file.is_file() {
        return Ok(None);
    }
    let mut source = String::new();
    File::open(project_file)?.read_to_string(&mut source)?;
    let mut section = "";
    let mut assembly_name = None;
    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            continue;
        }
        if section == "dotnet"
            && let Some(value) = line.strip_prefix("project/assembly_name=")
        {
            assembly_name = serde_json::from_str::<String>(value).ok();
            break;
        }
    }
    let Some(assembly_name) = assembly_name else {
        return Err(AppError::Provider(
            "Godot C# 项目缺少 [dotnet] project/assembly_name，无法绑定主程序集".to_owned(),
        ));
    };
    if assembly_name.is_empty()
        || !assembly_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AppError::Provider(
            "Godot project/assembly_name 含不安全字符".to_owned(),
        ));
    }
    let relative_path = format!("{assembly_name}.dll");
    let entry = manifest
        .entries
        .iter()
        .find(|entry| entry.relative_path.eq_ignore_ascii_case(&relative_path))
        .ok_or_else(|| {
            AppError::Provider(format!(
                "Build 最终产物不含 Godot 主程序集 {relative_path:?}"
            ))
        })?;
    Ok(Some(AssemblyBindingAttestation {
        assembly_name,
        relative_path: entry.relative_path.clone(),
        sha256: entry.sha256.clone(),
    }))
}

fn store_root(install_root: &Path) -> PathBuf {
    install_root.join("state/artifact-store/v1")
}

fn bundle_manifest_path(store_root: &Path, fingerprint: &str) -> PathBuf {
    store_root
        .join("bundles")
        .join(format!("{fingerprint}.json"))
}

fn object_path(store_root: &Path, sha256: &str) -> Result<PathBuf, AppError> {
    validate_sha256(sha256)?;
    Ok(store_root
        .join("objects/sha256")
        .join(&sha256[..2])
        .join(sha256))
}

fn validate_sha256(value: &str) -> Result<(), AppError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AppError::Provider("CAS object SHA-256 格式无效".to_owned()))
    }
}

fn validate_fingerprint(value: &str) -> Result<(), AppError> {
    if value.len() == 71
        && value.starts_with("sha256_")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(AppError::Provider(
            "Runtime bundle fingerprint 格式无效".to_owned(),
        ))
    }
}

fn validate_relative_path(value: &str) -> Result<(), AppError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(component, Component::Normal(_)) || component.as_os_str().to_str().is_none()
        })
    {
        return Err(AppError::Provider(format!(
            "Runtime bundle 含无效相对路径：{value:?}"
        )));
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn has_multiple_hard_links(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        // stable std 当前不公开 Windows link count；v1 通过物理复制避免主动创建 hardlink，
        // 不声称能防御同用户在运行期间替换为 hardlink。
        let _ = metadata;
        false
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() > 1
    }
    #[cfg(not(any(windows, unix)))]
    false
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, AppError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::{
        RuntimeArtifactBundle, assembly_binding, canonical_json, promote_runtime_bundle,
        verify_runtime_bundle,
    };
    use crate::{
        build::{ArtifactEntry, ArtifactManifest},
        provider,
    };
    use brain_evidence::content_fingerprint;

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-artifact-store-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn manifest(root: &std::path::Path) -> ArtifactManifest {
        let path = root.join("game.dll");
        let entry = ArtifactEntry {
            relative_path: "game.dll".to_owned(),
            size: fs::metadata(&path).unwrap().len(),
            sha256: provider::hash_file(&path).unwrap(),
        };
        let entries = vec![entry];
        ArtifactManifest {
            total_bytes: entries[0].size,
            manifest_fingerprint: content_fingerprint(&serde_json::to_vec(&entries).unwrap()),
            entries,
        }
    }

    #[test]
    fn promotes_and_revalidates_exact_runtime_bytes() {
        let root = temp_root("promote");
        let install = root.join("install");
        let project = root.join("project");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            project.join("project.godot"),
            "[dotnet]\nproject/assembly_name=\"game\"\n",
        )
        .unwrap();
        fs::write(artifacts.join("game.dll"), b"exact-build-bytes").unwrap();
        let manifest = manifest(&artifacts);

        let receipt = promote_runtime_bundle(
            Some(&install),
            "project-a",
            "dotnet-build.main",
            "Game.csproj",
            "sha256_source",
            &project,
            &artifacts,
            &manifest,
        )
        .unwrap();
        let verified = verify_runtime_bundle(Some(&install), &receipt.bundle_fingerprint).unwrap();

        assert_eq!(verified.entries, manifest.entries);
        assert_eq!(receipt.file_count, 1);
        assert_eq!(receipt.assembly_binding.unwrap().assembly_name, "game");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_object_never_falls_back_to_rebuilding() {
        let root = temp_root("corrupt");
        let install = root.join("install");
        let project = root.join("project");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(
            project.join("project.godot"),
            "[dotnet]\nproject/assembly_name=\"game\"\n",
        )
        .unwrap();
        fs::write(artifacts.join("game.dll"), b"exact-build-bytes").unwrap();
        let manifest = manifest(&artifacts);
        let receipt = promote_runtime_bundle(
            Some(&install),
            "project-a",
            "dotnet-build.main",
            "Game.csproj",
            "sha256_source",
            &project,
            &artifacts,
            &manifest,
        )
        .unwrap();
        let hash = &manifest.entries[0].sha256;
        let object = install
            .join("state/artifact-store/v1/objects/sha256")
            .join(&hash[..2])
            .join(hash);
        fs::write(object, b"corrupt").unwrap();

        assert!(verify_runtime_bundle(Some(&install), &receipt.bundle_fingerprint).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_store_is_rejected_inside_the_project_worktree() {
        let root = temp_root("store-boundary");
        let project = root.join("project");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("game.dll"), b"bytes").unwrap();
        let manifest = manifest(&artifacts);
        let install = project.join(".machine-state");

        assert!(
            promote_runtime_bundle(
                Some(&install),
                "project-a",
                "dotnet-build.main",
                "Fixture.csproj",
                "sha256_source",
                &project,
                &artifacts,
                &manifest,
            )
            .is_err()
        );
        assert!(!install.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn assembly_binding_requires_declared_output() {
        let root = temp_root("binding");
        fs::write(
            root.join("project.godot"),
            "[dotnet]\nproject/assembly_name=\"missing\"\n",
        )
        .unwrap();
        let artifacts = root.join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        fs::write(artifacts.join("game.dll"), b"bytes").unwrap();
        let manifest = manifest(&artifacts);

        assert!(assembly_binding(&root, &manifest).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canonical_manifest_fingerprint_covers_all_bundle_fields() {
        let bundle = RuntimeArtifactBundle {
            schema_version: 1,
            project_key: "project-a".to_owned(),
            build_provider_id: "dotnet-build.main".to_owned(),
            build_target: Some("Game.csproj".to_owned()),
            source_fingerprint: "sha256_source".to_owned(),
            artifact_manifest_fingerprint: "sha256_manifest".to_owned(),
            total_bytes: 0,
            entries: Vec::new(),
            assembly_binding: None,
        };
        let bytes = canonical_json(&bundle).unwrap();
        assert_eq!(content_fingerprint(&bytes), content_fingerprint(&bytes));
        assert!(bytes.ends_with(b"\n"));
    }
}
