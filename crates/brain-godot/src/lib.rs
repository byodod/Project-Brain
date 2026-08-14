use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use brain_evidence::{
    ArtifactEdge, ArtifactEdgeKind, ArtifactNode, EvidenceAuthority, EvidenceCoverage,
    EvidenceFinding, EvidencePlane, EvidenceProvider, EvidenceSnapshot, FindingAuthority,
    FindingSeverity,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const GODOT_PROBE_SCHEMA_VERSION: u32 = 1;
pub const GODOT_PROVIDER_ID: &str = "godot-engine-resolver";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GodotProbeResult {
    pub schema_version: u32,
    pub before: ProbeProjectState,
    pub after: ProbeProjectState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeProjectState {
    pub project_sha256: String,
    pub main_scene: ProbeReference,
    pub autoloads: Vec<ProbeAutoload>,
    pub resources: Vec<ProbeResource>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeReference {
    pub raw: String,
    pub resolved: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeAutoload {
    pub name: String,
    pub raw: String,
    pub resolved: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeResource {
    pub path: String,
    pub resource_type: String,
    pub uid: String,
    pub sha256: String,
    pub loaded: bool,
    pub dependencies: Vec<ProbeDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeDependency {
    pub raw: String,
    pub uid: String,
    pub type_name: String,
    pub fallback_path: String,
    pub resolved: String,
    pub exists: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GodotEvidenceReport {
    pub snapshot: EvidenceSnapshot,
    pub resource_count: usize,
    pub dependency_count: usize,
    pub loaded_resource_count: usize,
    pub error_count: usize,
}

/// 把 Godot 真实加载探针输出转换为 provider-neutral Engine Evidence Snapshot。
///
/// # Errors
///
/// 当探针 schema 不受支持、项目路径越界、运行期间源码漂移、探针哈希与当前文件不一致，或生成的
/// `ArtifactGraph` 违反 Evidence Protocol 时返回错误。
pub fn build_engine_evidence(
    project_root: &Path,
    project_key: &str,
    engine_identity: &str,
    probe: &GodotProbeResult,
    diagnostics: &[String],
) -> Result<GodotEvidenceReport, GodotError> {
    if probe.schema_version != GODOT_PROBE_SCHEMA_VERSION {
        return Err(GodotError::UnsupportedProbeSchema {
            actual: probe.schema_version,
            expected: GODOT_PROBE_SCHEMA_VERSION,
        });
    }
    verify_probe_stability(&probe.before, &probe.after)?;
    let state = &probe.after;
    let root = project_root.canonicalize()?;
    let project_bytes = verified_file(&root, "project.godot", &state.project_sha256)?;
    let provider = EvidenceProvider {
        id: GODOT_PROVIDER_ID.to_owned(),
        version: engine_identity.to_owned(),
        contract_version: 1,
        authority: EvidenceAuthority::Deterministic,
    };
    let mut assembly = EvidenceAssembly::new(project_key, &project_bytes);
    collect_resources(&root, project_key, &state.resources, &mut assembly)?;
    add_project_reference(
        &root,
        project_key,
        &assembly.project_id,
        "main scene",
        &state.main_scene.raw,
        &state.main_scene.resolved,
        ArtifactEdgeKind::MainScene,
        &mut assembly.artifacts,
        &mut assembly.edges,
        &mut assembly.findings,
        &mut assembly.manifest,
    )?;
    let mut autoload_names = BTreeSet::new();
    for autoload in &state.autoloads {
        if autoload.name.trim().is_empty() || !autoload_names.insert(autoload.name.as_str()) {
            return Err(GodotError::InvalidAutoload(autoload.name.clone()));
        }
        add_project_reference(
            &root,
            project_key,
            &assembly.project_id,
            &format!("autoload {}", autoload.name),
            &autoload.raw,
            &autoload.resolved,
            ArtifactEdgeKind::DeclaresAutoload,
            &mut assembly.artifacts,
            &mut assembly.edges,
            &mut assembly.findings,
            &mut assembly.manifest,
        )?;
    }
    for diagnostic in diagnostics {
        if !diagnostic.trim().is_empty() {
            assembly.findings.push(error_finding(
                "GODOT_ENGINE_DIAGNOSTIC",
                diagnostic.trim().to_owned(),
                None,
                None,
            ));
        }
    }

    assembly.manifest.sort();
    assembly.manifest.dedup();
    let source_fingerprint = source_manifest_fingerprint(&assembly.manifest);
    let snapshot = EvidenceSnapshot::new(
        project_key,
        EvidencePlane::Engine,
        provider,
        &source_fingerprint,
        EvidenceCoverage::Complete,
        Vec::new(),
        assembly.artifacts.into_values().collect(),
        assembly.edges,
        assembly.findings,
    )?;
    let error_count = snapshot
        .findings
        .iter()
        .filter(|finding| finding.severity == FindingSeverity::Error)
        .count();
    Ok(GodotEvidenceReport {
        snapshot,
        resource_count: state.resources.len(),
        dependency_count: assembly.dependency_count,
        loaded_resource_count: assembly.loaded_resource_count,
        error_count,
    })
}

struct EvidenceAssembly {
    project_id: String,
    artifacts: BTreeMap<String, ArtifactNode>,
    findings: Vec<EvidenceFinding>,
    edges: Vec<ArtifactEdge>,
    manifest: Vec<(String, String)>,
    dependency_count: usize,
    loaded_resource_count: usize,
}

impl EvidenceAssembly {
    fn new(project_key: &str, project_bytes: &[u8]) -> Self {
        let project_node = ArtifactNode::from_provider_key(
            project_key,
            GODOT_PROVIDER_ID,
            "godot_project_config",
            "project.godot",
            "project.godot",
            Some("project.godot"),
            project_bytes,
        );
        let project_id = project_node.id.clone();
        Self {
            project_id,
            artifacts: BTreeMap::from([("project.godot".to_owned(), project_node)]),
            findings: Vec::new(),
            edges: Vec::new(),
            manifest: vec![("project.godot".to_owned(), raw_sha256(project_bytes))],
            dependency_count: 0,
            loaded_resource_count: 0,
        }
    }
}

fn collect_resources(
    root: &Path,
    project_key: &str,
    resources: &[ProbeResource],
    assembly: &mut EvidenceAssembly,
) -> Result<(), GodotError> {
    let mut resource_paths = BTreeSet::new();
    for resource in resources {
        collect_resource(root, project_key, resource, assembly, &mut resource_paths)?;
    }
    Ok(())
}

fn collect_resource(
    root: &Path,
    project_key: &str,
    resource: &ProbeResource,
    assembly: &mut EvidenceAssembly,
    resource_paths: &mut BTreeSet<String>,
) -> Result<(), GodotError> {
    let path = project_relative_path(&resource.path)?;
    if !resource_paths.insert(path.clone()) {
        return Err(GodotError::DuplicateResource(path));
    }
    let bytes = verified_file(root, &path, &resource.sha256)?;
    assembly.manifest.push((path.clone(), raw_sha256(&bytes)));
    let resource_id = ensure_artifact(
        &mut assembly.artifacts,
        project_key,
        &path,
        resource_kind(&path, &resource.resource_type),
        &bytes,
    );
    if resource.loaded {
        assembly.loaded_resource_count += 1;
    } else {
        assembly.findings.push(error_finding(
            "GODOT_RESOURCE_LOAD_FAILED",
            format!("Godot 无法加载资源 res://{path}"),
            Some(resource_id.clone()),
            Some(path.clone()),
        ));
    }
    let mut dependency_keys = BTreeSet::new();
    for dependency in &resource.dependencies {
        assembly.dependency_count += 1;
        let key = (
            dependency.raw.as_str(),
            dependency.resolved.as_str(),
            dependency.type_name.as_str(),
        );
        if !dependency_keys.insert(key) {
            return Err(GodotError::DuplicateDependency(path));
        }
        add_dependency(
            root,
            project_key,
            &resource_id,
            &path,
            dependency,
            &mut assembly.artifacts,
            &mut assembly.edges,
            &mut assembly.findings,
            &mut assembly.manifest,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_dependency(
    root: &Path,
    project_key: &str,
    resource_id: &str,
    resource_path: &str,
    dependency: &ProbeDependency,
    artifacts: &mut BTreeMap<String, ArtifactNode>,
    edges: &mut Vec<ArtifactEdge>,
    findings: &mut Vec<EvidenceFinding>,
    manifest: &mut Vec<(String, String)>,
) -> Result<(), GodotError> {
    if dependency.resolved.trim().is_empty() {
        findings.push(error_finding(
            "GODOT_UID_UNRESOLVED",
            format!("Godot 无法解析依赖 {}", dependency.raw),
            Some(resource_id.to_owned()),
            Some(resource_path.to_owned()),
        ));
        return Ok(());
    }
    let path = project_relative_path(&dependency.resolved)?;
    if cache_path(&path) {
        findings.push(error_finding(
            "GODOT_CACHE_REFERENCE",
            format!("权威资源不得依赖 Godot 缓存 res://{path}"),
            Some(resource_id.to_owned()),
            Some(path),
        ));
        return Ok(());
    }
    let current_exists = root.join(&path).is_file();
    if current_exists != dependency.exists {
        return Err(GodotError::SourceDrift(path));
    }
    let bytes = if dependency.exists {
        let bytes = verified_file(root, &path, &dependency.sha256)?;
        manifest.push((path.clone(), raw_sha256(&bytes)));
        bytes
    } else {
        Vec::new()
    };
    let target_id = ensure_artifact(
        artifacts,
        project_key,
        &path,
        resource_kind(&path, &dependency.type_name),
        &bytes,
    );
    edges.push(ArtifactEdge {
        source_id: resource_id.to_owned(),
        target_id: target_id.clone(),
        kind: dependency_edge_kind(&dependency.type_name),
    });
    if !dependency.exists {
        findings.push(error_finding(
            "GODOT_MISSING_DEPENDENCY",
            format!("资源依赖不存在：res://{path}"),
            Some(target_id),
            Some(path),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_project_reference(
    root: &Path,
    project_key: &str,
    project_id: &str,
    label: &str,
    raw: &str,
    resolved: &str,
    edge_kind: ArtifactEdgeKind,
    artifacts: &mut BTreeMap<String, ArtifactNode>,
    edges: &mut Vec<ArtifactEdge>,
    findings: &mut Vec<EvidenceFinding>,
    manifest: &mut Vec<(String, String)>,
) -> Result<(), GodotError> {
    if raw.trim().is_empty() {
        return Ok(());
    }
    if resolved.trim().is_empty() {
        findings.push(error_finding(
            "GODOT_PROJECT_REFERENCE_UNRESOLVED",
            format!("Godot 无法解析 {label}: {raw}"),
            Some(project_id.to_owned()),
            Some("project.godot".to_owned()),
        ));
        return Ok(());
    }
    let path = project_relative_path(resolved)?;
    if cache_path(&path) {
        findings.push(error_finding(
            "GODOT_CACHE_REFERENCE",
            format!("{label} 不得指向 Godot 缓存 res://{path}"),
            Some(project_id.to_owned()),
            Some(path),
        ));
        return Ok(());
    }
    let candidate = root.join(&path);
    let bytes = if candidate.is_file() {
        let bytes = read_inside_root(root, &path)?;
        manifest.push((path.clone(), raw_sha256(&bytes)));
        bytes
    } else {
        Vec::new()
    };
    let target_id = ensure_artifact(
        artifacts,
        project_key,
        &path,
        resource_kind(&path, ""),
        &bytes,
    );
    edges.push(ArtifactEdge {
        source_id: project_id.to_owned(),
        target_id: target_id.clone(),
        kind: edge_kind,
    });
    if bytes.is_empty() && !candidate.is_file() {
        findings.push(error_finding(
            "GODOT_PROJECT_REFERENCE_MISSING",
            format!("{label} 不存在：res://{path}"),
            Some(target_id),
            Some(path),
        ));
    }
    Ok(())
}

fn ensure_artifact(
    artifacts: &mut BTreeMap<String, ArtifactNode>,
    project_key: &str,
    path: &str,
    kind: &str,
    bytes: &[u8],
) -> String {
    artifacts
        .entry(path.to_owned())
        .or_insert_with(|| {
            ArtifactNode::from_provider_key(
                project_key,
                GODOT_PROVIDER_ID,
                kind,
                path,
                path,
                Some(path),
                bytes,
            )
        })
        .id
        .clone()
}

fn verified_file(root: &Path, relative: &str, expected: &str) -> Result<Vec<u8>, GodotError> {
    validate_raw_sha(expected)?;
    let bytes = read_inside_root(root, relative)?;
    if raw_sha256(&bytes) != expected {
        return Err(GodotError::SourceDrift(relative.to_owned()));
    }
    Ok(bytes)
}

fn read_inside_root(root: &Path, relative: &str) -> Result<Vec<u8>, GodotError> {
    let path = root.join(relative);
    let canonical = path
        .canonicalize()
        .map_err(|error| GodotError::ReadSource(path.clone(), error))?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(GodotError::PathOutsideProject(relative.to_owned()));
    }
    Ok(fs::read(canonical)?)
}

fn project_relative_path(resource_path: &str) -> Result<String, GodotError> {
    let Some(path) = resource_path.strip_prefix("res://") else {
        return Err(GodotError::InvalidResourcePath(resource_path.to_owned()));
    };
    if path.is_empty()
        || path.contains('\\')
        || path.contains(':')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(GodotError::InvalidResourcePath(resource_path.to_owned()));
    }
    Ok(path.to_owned())
}

fn cache_path(path: &str) -> bool {
    path == ".godot" || path.starts_with(".godot/")
}

fn resource_kind(path: &str, declared_type: &str) -> &'static str {
    let extension = Path::new(path).extension().and_then(|value| value.to_str());
    if declared_type.eq_ignore_ascii_case("PackedScene")
        || extension.is_some_and(|value| value.eq_ignore_ascii_case("tscn"))
    {
        "godot_scene"
    } else if declared_type.to_ascii_lowercase().contains("script")
        || extension.is_some_and(|value| matches_ignore_ascii_case(value, &["gd", "cs"]))
    {
        "godot_script"
    } else if extension.is_some_and(|value| matches_ignore_ascii_case(value, &["tres", "res"])) {
        "godot_resource"
    } else {
        "source_asset"
    }
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn dependency_edge_kind(declared_type: &str) -> ArtifactEdgeKind {
    if declared_type.eq_ignore_ascii_case("PackedScene") {
        ArtifactEdgeKind::Instances
    } else if declared_type.to_ascii_lowercase().contains("script") {
        ArtifactEdgeKind::AttachesScript
    } else {
        ArtifactEdgeKind::UsesResource
    }
}

fn error_finding(
    code: &str,
    message: String,
    artifact_id: Option<String>,
    path: Option<String>,
) -> EvidenceFinding {
    EvidenceFinding {
        code: code.to_owned(),
        severity: FindingSeverity::Error,
        authority: FindingAuthority::DeterministicViolation,
        message,
        artifact_id,
        path,
    }
}

fn validate_raw_sha(value: &str) -> Result<(), GodotError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GodotError::InvalidProbeHash(value.to_owned()));
    }
    Ok(())
}

fn raw_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn source_manifest_fingerprint(manifest: &[(String, String)]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"project-brain/godot-source-manifest/v1\0");
    for (path, hash) in manifest {
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update((hash.len() as u64).to_be_bytes());
        digest.update(hash.as_bytes());
    }
    format!("godot_source_v1_{:x}", digest.finalize())
}

fn verify_probe_stability(
    before: &ProbeProjectState,
    after: &ProbeProjectState,
) -> Result<(), GodotError> {
    let mut before = before.clone();
    let mut after = after.clone();
    normalize_probe_state(&mut before);
    normalize_probe_state(&mut after);
    for resource in &mut before.resources {
        resource.loaded = false;
    }
    for resource in &mut after.resources {
        resource.loaded = false;
    }
    if before != after {
        return Err(GodotError::SourceDrift(
            "engine-resolved project state".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_probe_state(state: &mut ProbeProjectState) {
    state.autoloads.sort_by(|left, right| {
        (&left.name, &left.raw, &left.resolved).cmp(&(&right.name, &right.raw, &right.resolved))
    });
    for resource in &mut state.resources {
        resource.dependencies.sort_by(|left, right| {
            (
                &left.raw,
                &left.uid,
                &left.type_name,
                &left.fallback_path,
                &left.resolved,
            )
                .cmp(&(
                    &right.raw,
                    &right.uid,
                    &right.type_name,
                    &right.fallback_path,
                    &right.resolved,
                ))
        });
    }
    state
        .resources
        .sort_by(|left, right| left.path.cmp(&right.path));
}

#[derive(Debug, Error)]
pub enum GodotError {
    #[error("I/O 操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Evidence(#[from] brain_evidence::EvidenceError),
    #[error("不支持 Godot probe schema_version={actual}，当前版本为 {expected}")]
    UnsupportedProbeSchema { actual: u32, expected: u32 },
    #[error("Godot probe 返回非法 SHA-256：{0:?}")]
    InvalidProbeHash(String),
    #[error("Godot 资源路径无效：{0:?}")]
    InvalidResourcePath(String),
    #[error("Godot 资源解析后越出项目：{0}")]
    PathOutsideProject(String),
    #[error("读取 Godot 源文件失败 {0}：{1}")]
    ReadSource(PathBuf, std::io::Error),
    #[error("Godot 探针运行期间源文件发生变化：{0}")]
    SourceDrift(String),
    #[error("Godot probe 返回重复资源：{0}")]
    DuplicateResource(String),
    #[error("Godot probe 返回重复依赖：{0}")]
    DuplicateDependency(String),
    #[error("Godot autoload 名称为空或重复：{0:?}")]
    InvalidAutoload(String),
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use brain_evidence::{EvidenceFreshness, FindingSeverity};

    use super::*;

    fn temp_project() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-godot-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("project.godot"), b"[application]\n").unwrap();
        fs::write(root.join("scenes/main.tscn"), b"[gd_scene]\n").unwrap();
        fs::write(root.join("scripts/main.gd"), b"extends Node\n").unwrap();
        root
    }

    fn hash(path: &Path) -> String {
        raw_sha256(&fs::read(path).unwrap())
    }

    fn probe(root: &Path) -> GodotProbeResult {
        let project_hash = hash(&root.join("project.godot"));
        let scene_hash = hash(&root.join("scenes/main.tscn"));
        let script_hash = hash(&root.join("scripts/main.gd"));
        let state = ProbeProjectState {
            project_sha256: project_hash,
            main_scene: ProbeReference {
                raw: "uid://main".to_owned(),
                resolved: "res://scenes/main.tscn".to_owned(),
            },
            autoloads: Vec::new(),
            resources: vec![ProbeResource {
                path: "res://scenes/main.tscn".to_owned(),
                resource_type: "PackedScene".to_owned(),
                uid: "uid://main".to_owned(),
                sha256: scene_hash,
                loaded: true,
                dependencies: vec![ProbeDependency {
                    raw: "uid://script::Script::res://scripts/main.gd".to_owned(),
                    uid: "uid://script".to_owned(),
                    type_name: "Script".to_owned(),
                    fallback_path: "res://scripts/main.gd".to_owned(),
                    resolved: "res://scripts/main.gd".to_owned(),
                    exists: true,
                    sha256: script_hash,
                }],
            }],
        };
        GodotProbeResult {
            schema_version: GODOT_PROBE_SCHEMA_VERSION,
            before: state.clone(),
            after: state,
        }
    }

    #[test]
    fn creates_separate_engine_artifact_graph_from_real_probe_observations() {
        let root = temp_project();
        let report =
            build_engine_evidence(&root, "project-a", "4.6.0+sha256.test", &probe(&root), &[])
                .unwrap();

        report.snapshot.validate().unwrap();
        assert_eq!(report.resource_count, 1);
        assert_eq!(report.dependency_count, 1);
        assert_eq!(report.loaded_resource_count, 1);
        assert_eq!(report.error_count, 0);
        assert_eq!(report.snapshot.artifacts.len(), 3);
        assert!(
            report
                .snapshot
                .edges
                .iter()
                .any(|edge| { edge.kind == ArtifactEdgeKind::MainScene })
        );
        assert!(
            report
                .snapshot
                .edges
                .iter()
                .any(|edge| { edge.kind == ArtifactEdgeKind::AttachesScript })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_dependency_is_complete_deterministic_error_evidence() {
        let root = temp_project();
        let mut input = probe(&root);
        for state in [&mut input.before, &mut input.after] {
            let dependency = &mut state.resources[0].dependencies[0];
            dependency.resolved = "res://missing.png".to_owned();
            dependency.exists = false;
            dependency.sha256.clear();
        }
        let report =
            build_engine_evidence(&root, "project-a", "4.6.0+sha256.test", &input, &[]).unwrap();
        let finding = report
            .snapshot
            .findings
            .iter()
            .find(|item| item.code == "GODOT_MISSING_DEPENDENCY")
            .unwrap();

        assert_eq!(finding.severity, FindingSeverity::Error);
        assert!(
            report
                .snapshot
                .finding_can_hard_block(finding, EvidenceFreshness::Fresh, true)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_drift_and_cache_authority_are_not_silently_accepted() {
        let root = temp_project();
        let mut drifted = probe(&root);
        drifted.after.resources[0].sha256 = "0".repeat(64);
        assert!(matches!(
            build_engine_evidence(&root, "project-a", "4.6.0+sha256.test", &drifted, &[]),
            Err(GodotError::SourceDrift(_))
        ));

        let mut cache = probe(&root);
        for state in [&mut cache.before, &mut cache.after] {
            let dependency = &mut state.resources[0].dependencies[0];
            dependency.resolved = "res://.godot/imported/texture.ctex".to_owned();
            dependency.exists = true;
        }
        let report =
            build_engine_evidence(&root, "project-a", "4.6.0+sha256.test", &cache, &[]).unwrap();
        assert!(
            report
                .snapshot
                .findings
                .iter()
                .any(|item| item.code == "GODOT_CACHE_REFERENCE")
        );
        assert!(
            report
                .snapshot
                .artifacts
                .iter()
                .all(|item| !item.path.as_deref().is_some_and(cache_path))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
