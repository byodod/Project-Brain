use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use brain_core::{
    ProjectLanguageProfile, SemanticProviderFormat, SemanticProviderProfile, path_has_prefix,
};
use brain_scip::{
    ScipImport, ScipImportProfile, ScipImportStats, ScipLanguageCapabilities, ScipLanguageMapping,
};
use brain_store::{
    BrainStore, SemanticApplyResult, SemanticSnapshotSource, SemanticSourceManifest,
};
use brain_symbols::{ProviderDescriptor, SourceFileState, SourceLanguage};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{error::AppError, git};

#[derive(Debug, Serialize)]
pub struct ScipIndexReport {
    pub schema_version: u32,
    pub experimental: bool,
    pub project_key: String,
    pub provider_profile: String,
    pub provider: ProviderDescriptor,
    pub capabilities: Vec<ScipLanguageCapabilities>,
    pub producer_name: String,
    pub producer_version: String,
    pub languages: Vec<SourceLanguage>,
    pub source_revision: String,
    pub source: SemanticSnapshotSource,
    pub stats: ScipImportStats,
    pub coverage: SemanticCoverageReport,
    pub lineage_observations: u64,
    pub apply: SemanticApplyResult,
}

pub struct PreparedScipIndex {
    project_key: String,
    provider_profile: String,
    imported: ScipImport,
    source: SemanticSnapshotSource,
    coverage: SemanticCoverageReport,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScipStabilityEvidence {
    pub semantic_snapshot_fingerprint: String,
    pub document_manifest_hash: String,
    pub document_count: u64,
    pub coverage_status: &'static str,
}

const COVERAGE_PATH_SAMPLE_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct SemanticCoverageReport {
    pub status: &'static str,
    pub expected_source_files: u64,
    pub indexed_source_files: u64,
    pub provider_documents: u64,
    pub missing_source_files: u64,
    pub missing_source_file_sample: Vec<String>,
    pub provider_only_files: u64,
    pub provider_only_file_sample: Vec<String>,
    pub languages: Vec<LanguageCoverageReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LanguageCoverageReport {
    pub language: String,
    pub status: &'static str,
    pub expected_source_files: u64,
    pub indexed_source_files: u64,
    pub provider_documents: u64,
    pub missing_source_files: u64,
    pub missing_source_file_sample: Vec<String>,
    pub provider_only_files: u64,
    pub provider_only_file_sample: Vec<String>,
    pub recognized_extensions: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderCoverageReport {
    pub provider_profile: String,
    pub provider_contract_id: String,
    pub status: &'static str,
    pub snapshot_fingerprint: Option<String>,
    pub source_fresh: Option<bool>,
    pub source: Option<SemanticSnapshotSource>,
    pub coverage: Option<SemanticCoverageReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticCoverageDoctorReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub profiles: Vec<ProviderCoverageReport>,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
}

impl PreparedScipIndex {
    pub fn attest_trusted_provider(
        &mut self,
        registration_id: &str,
        executable_sha256: &str,
        artifact_sha256: &str,
    ) {
        self.source = SemanticSnapshotSource::trusted_provider(
            self.source.worktree_fingerprint.clone(),
            self.source.head_revision.clone(),
            self.source.worktree_clean,
            registration_id.to_owned(),
            executable_sha256.to_owned(),
            artifact_sha256.to_owned(),
        );
    }

    pub fn stability_evidence(&self) -> ScipStabilityEvidence {
        ScipStabilityEvidence {
            semantic_snapshot_fingerprint: self.imported.snapshot.source_revision.clone(),
            document_manifest_hash: document_manifest_hash(&self.imported.snapshot.sources),
            document_count: u64::try_from(self.imported.snapshot.sources.len()).unwrap_or(u64::MAX),
            coverage_status: self.coverage.status,
        }
    }

    pub fn document_paths(&self) -> BTreeSet<String> {
        self.imported
            .snapshot
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    root: &Path,
    project_key: &str,
    language_profiles: &[ProjectLanguageProfile],
    provider_profiles: &[SemanticProviderProfile],
    store: &BrainStore,
    provider_profile_id: &str,
    input: &Path,
) -> Result<ScipIndexReport, AppError> {
    let prepared = prepare(
        root,
        project_key,
        language_profiles,
        provider_profiles,
        provider_profile_id,
        input,
    )?;
    commit(store, prepared)
}

pub fn prepare(
    root: &Path,
    project_key: &str,
    language_profiles: &[ProjectLanguageProfile],
    provider_profiles: &[SemanticProviderProfile],
    provider_profile_id: &str,
    input: &Path,
) -> Result<PreparedScipIndex, AppError> {
    let configured = provider_profiles
        .iter()
        .find(|profile| profile.id == provider_profile_id)
        .ok_or_else(|| {
            AppError::ScipProfileMismatch(format!(
                "找不到 semantic provider profile={provider_profile_id:?}"
            ))
        })?;
    if configured.format != SemanticProviderFormat::Scip {
        return Err(AppError::ScipProfileMismatch(format!(
            "provider profile={provider_profile_id:?} 不是 SCIP"
        )));
    }
    let import_profile = import_profile(configured)?;

    let head_revision = git::head_revision(root)?;
    let source = SemanticSnapshotSource::offline(
        git::worktree_fingerprint(root)?,
        head_revision.clone(),
        git::worktree_is_clean(root)?,
    );
    let imported =
        brain_scip::import_file(root, project_key, &head_revision, input, &import_profile)?;
    validate_project_roots(&imported.snapshot, language_profiles)?;
    let coverage = evaluate_coverage(
        root,
        language_profiles,
        configured,
        &imported.snapshot.sources,
    )?;
    let after = SemanticSnapshotSource::offline(
        git::worktree_fingerprint(root)?,
        git::head_revision(root)?,
        git::worktree_is_clean(root)?,
    );
    if after != source {
        return Err(AppError::ScipProfileMismatch(
            "源码或 Git 基线在 SCIP 导入期间发生变化；拒绝准备 semantic snapshot".to_owned(),
        ));
    }
    Ok(PreparedScipIndex {
        project_key: project_key.to_owned(),
        provider_profile: configured.id.clone(),
        imported,
        source,
        coverage,
    })
}

pub fn commit(
    store: &BrainStore,
    prepared: PreparedScipIndex,
) -> Result<ScipIndexReport, AppError> {
    ensure_committable_coverage(&prepared.coverage)?;
    let apply = store.apply_semantic_snapshot(
        &prepared.imported.snapshot,
        &prepared.provider_profile,
        &prepared.imported.lineage_observations,
        &[],
        &prepared.source,
    )?;
    Ok(ScipIndexReport {
        schema_version: brain_core::CURRENT_SCHEMA_VERSION,
        experimental: true,
        project_key: prepared.project_key,
        provider_profile: prepared.provider_profile,
        provider: prepared.imported.snapshot.provider,
        capabilities: prepared.imported.capabilities,
        producer_name: prepared.imported.producer_name,
        producer_version: prepared.imported.producer_version,
        languages: prepared.imported.languages,
        source_revision: prepared.imported.snapshot.source_revision,
        source: prepared.source,
        stats: prepared.imported.stats,
        coverage: prepared.coverage,
        lineage_observations: u64::try_from(prepared.imported.lineage_observations.len())
            .unwrap_or(u64::MAX),
        apply,
    })
}

fn ensure_committable_coverage(coverage: &SemanticCoverageReport) -> Result<(), AppError> {
    if coverage.status == "complete" {
        return Ok(());
    }
    Err(AppError::ScipProfileMismatch(format!(
        "semantic coverage={}；仅 complete 快照可以成为治理事实（indexed={}/{}, missing_sample={:?}）",
        coverage.status,
        coverage.indexed_source_files,
        coverage.expected_source_files,
        coverage.missing_source_file_sample
    )))
}

fn document_manifest_hash(sources: &[SourceFileState]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"project-brain/scip-document-manifest/v1\0");
    let paths = sources
        .iter()
        .map(|source| source.path.as_str())
        .collect::<BTreeSet<_>>();
    for path in paths {
        digest.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(path.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub fn doctor_coverage(
    root: &Path,
    project_key: &str,
    language_profiles: &[ProjectLanguageProfile],
    provider_profiles: &[SemanticProviderProfile],
    store: &BrainStore,
) -> SemanticCoverageDoctorReport {
    if provider_profiles.is_empty() {
        return SemanticCoverageDoctorReport {
            schema_version: brain_core::CURRENT_SCHEMA_VERSION,
            status: "not_applicable",
            profiles: Vec::new(),
            issues: Vec::new(),
            warnings: Vec::new(),
        };
    }
    let current_worktree = git::worktree_fingerprint(root);
    let current_head = git::head_revision(root);
    let mut reports = Vec::new();
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    for configured in provider_profiles {
        let assessment = assess_provider_coverage(
            root,
            project_key,
            language_profiles,
            configured,
            store,
            current_worktree.as_ref().ok(),
            current_head.as_ref().ok(),
        );
        reports.push(assessment.report);
        issues.extend(assessment.issues);
        warnings.extend(assessment.warnings);
    }
    let status = if !issues.is_empty() {
        "degraded"
    } else if reports.iter().all(|report| report.status == "complete") {
        "ready"
    } else {
        "advisory"
    };
    SemanticCoverageDoctorReport {
        schema_version: brain_core::CURRENT_SCHEMA_VERSION,
        status,
        profiles: reports,
        issues,
        warnings,
    }
}

struct ProviderCoverageAssessment {
    report: ProviderCoverageReport,
    issues: Vec<String>,
    warnings: Vec<String>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "coverage assessment keeps the repository, profile, store and freshness evidence explicit"
)]
fn assess_provider_coverage(
    root: &Path,
    project_key: &str,
    language_profiles: &[ProjectLanguageProfile],
    configured: &SemanticProviderProfile,
    store: &BrainStore,
    current_worktree: Option<&String>,
    current_head: Option<&String>,
) -> ProviderCoverageAssessment {
    let (contract_id, manifest) = match load_provider_manifest(project_key, configured, store) {
        Ok(value) => value,
        Err((contract_id, status, issue)) => {
            return ProviderCoverageAssessment {
                report: empty_provider_report(configured, contract_id, status),
                issues: vec![issue],
                warnings: Vec::new(),
            };
        }
    };
    let Some(manifest) = manifest else {
        return ProviderCoverageAssessment {
            report: empty_provider_report(configured, contract_id, "not_indexed"),
            issues: Vec::new(),
            warnings: vec![format!(
                "provider profile={} 尚无 semantic snapshot；覆盖率未声明为完整",
                configured.id
            )],
        };
    };
    if !manifest.recorded {
        return ProviderCoverageAssessment {
            report: provider_report(
                configured,
                contract_id,
                "legacy_manifest_missing",
                manifest,
                None,
                None,
            ),
            issues: vec![format!(
                "provider profile={} 最新快照来自旧数据库且没有源码 manifest；必须重新索引",
                configured.id
            )],
            warnings: Vec::new(),
        };
    }
    let coverage = match evaluate_coverage(root, language_profiles, configured, &manifest.sources) {
        Ok(coverage) => coverage,
        Err(error) => {
            return ProviderCoverageAssessment {
                report: provider_report(
                    configured,
                    contract_id,
                    "coverage_error",
                    manifest,
                    None,
                    None,
                ),
                issues: vec![format!(
                    "provider profile={} 覆盖率计算失败：{error}",
                    configured.id
                )],
                warnings: Vec::new(),
            };
        }
    };
    let fresh = current_worktree
        .zip(current_head)
        .is_some_and(|(worktree, head)| {
            manifest.source.worktree_fingerprint == *worktree
                && manifest.source.head_revision == *head
        });
    let (status, issues, warnings) = coverage_status(configured, &coverage, fresh);
    ProviderCoverageAssessment {
        report: provider_report(
            configured,
            contract_id,
            status,
            manifest,
            Some(fresh),
            Some(coverage),
        ),
        issues,
        warnings,
    }
}

fn load_provider_manifest(
    project_key: &str,
    configured: &SemanticProviderProfile,
    store: &BrainStore,
) -> Result<(String, Option<SemanticSourceManifest>), (String, &'static str, String)> {
    let import_profile = import_profile(configured)
        .map_err(|error| (String::new(), "invalid_profile", error.to_string()))?;
    let contract_id = brain_scip::provider_contract_id(&import_profile);
    let manifest = store
        .latest_semantic_source_manifest(project_key, &configured.id, &contract_id)
        .map_err(|error| {
            (
                contract_id.clone(),
                "store_error",
                format!(
                    "provider profile={} 无法读取 semantic source manifest：{error}",
                    configured.id
                ),
            )
        })?;
    Ok((contract_id, manifest))
}

fn coverage_status(
    configured: &SemanticProviderProfile,
    coverage: &SemanticCoverageReport,
    fresh: bool,
) -> (&'static str, Vec<String>, Vec<String>) {
    if !fresh {
        return (
            "stale",
            vec![format!(
                "provider profile={} 的覆盖率基线已过期；必须对当前 worktree/HEAD 重新索引",
                configured.id
            )],
            Vec::new(),
        );
    }
    if coverage.status == "partial" {
        return (
            "partial",
            vec![format!(
                "provider profile={} 仅索引 {}/{} 个已声明源码文件",
                configured.id, coverage.indexed_source_files, coverage.expected_source_files
            )],
            Vec::new(),
        );
    }
    if coverage.status == "unverifiable" {
        return (
            "unverifiable",
            Vec::new(),
            vec![format!(
                "provider profile={} 包含 Project Brain 尚无扩展名契约的语言；覆盖率不可验证",
                configured.id
            )],
        );
    }
    ("complete", Vec::new(), Vec::new())
}

fn empty_provider_report(
    configured: &SemanticProviderProfile,
    contract_id: String,
    status: &'static str,
) -> ProviderCoverageReport {
    ProviderCoverageReport {
        provider_profile: configured.id.clone(),
        provider_contract_id: contract_id,
        status,
        snapshot_fingerprint: None,
        source_fresh: None,
        source: None,
        coverage: None,
    }
}

fn provider_report(
    configured: &SemanticProviderProfile,
    contract_id: String,
    status: &'static str,
    manifest: SemanticSourceManifest,
    source_fresh: Option<bool>,
    coverage: Option<SemanticCoverageReport>,
) -> ProviderCoverageReport {
    ProviderCoverageReport {
        provider_profile: configured.id.clone(),
        provider_contract_id: contract_id,
        status,
        snapshot_fingerprint: Some(manifest.snapshot_fingerprint),
        source_fresh,
        source: Some(manifest.source),
        coverage,
    }
}

fn import_profile(configured: &SemanticProviderProfile) -> Result<ScipImportProfile, AppError> {
    Ok(ScipImportProfile {
        id: configured.id.clone(),
        producer: configured.producer.clone(),
        contract_version: configured.contract_version,
        language_mappings: configured
            .language_mappings
            .iter()
            .map(|mapping| {
                SourceLanguage::parse(&mapping.language)
                    .map(|language| ScipLanguageMapping {
                        raw_language: mapping.raw_language.clone(),
                        language,
                        allow_missing_language: mapping.allow_missing_language,
                    })
                    .ok_or_else(|| {
                        AppError::ScipProfileMismatch(format!(
                            "provider profile={} 映射到无效 language={:?}",
                            configured.id, mapping.language
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn evaluate_coverage(
    root: &Path,
    language_profiles: &[ProjectLanguageProfile],
    provider_profile: &SemanticProviderProfile,
    sources: &[SourceFileState],
) -> Result<SemanticCoverageReport, AppError> {
    let repository_files = git::repository_files(root)?;
    evaluate_coverage_from_files(
        root,
        language_profiles,
        provider_profile,
        sources,
        &repository_files,
    )
}

fn evaluate_coverage_from_files(
    root: &Path,
    language_profiles: &[ProjectLanguageProfile],
    provider_profile: &SemanticProviderProfile,
    sources: &[SourceFileState],
    repository_files: &[String],
) -> Result<SemanticCoverageReport, AppError> {
    let mapped_languages = provider_profile
        .language_mappings
        .iter()
        .map(|mapping| mapping.language.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut languages = Vec::new();
    let mut all_expected = BTreeSet::new();
    let mut all_indexed = BTreeSet::new();
    let mut all_provider = BTreeSet::new();
    let mut any_unverifiable = false;
    for language in mapped_languages {
        let evaluated = evaluate_language_coverage(
            root,
            language_profiles,
            provider_profile,
            sources,
            repository_files,
            language,
        )?;
        all_expected.extend(evaluated.expected);
        all_indexed.extend(evaluated.indexed);
        all_provider.extend(evaluated.provider_documents);
        any_unverifiable |= evaluated.unverifiable;
        languages.push(evaluated.report);
    }
    let missing = all_expected
        .difference(&all_indexed)
        .cloned()
        .collect::<BTreeSet<_>>();
    let provider_only = all_provider
        .difference(&all_expected)
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(SemanticCoverageReport {
        status: if !missing.is_empty() {
            "partial"
        } else if any_unverifiable {
            "unverifiable"
        } else {
            "complete"
        },
        expected_source_files: count(&all_expected),
        indexed_source_files: count(&all_indexed),
        provider_documents: count(&all_provider),
        missing_source_files: count(&missing),
        missing_source_file_sample: sample(&missing),
        provider_only_files: count(&provider_only),
        provider_only_file_sample: sample(&provider_only),
        languages,
    })
}

struct EvaluatedLanguageCoverage {
    report: LanguageCoverageReport,
    expected: BTreeSet<String>,
    indexed: BTreeSet<String>,
    provider_documents: BTreeSet<String>,
    unverifiable: bool,
}

fn evaluate_language_coverage(
    root: &Path,
    language_profiles: &[ProjectLanguageProfile],
    provider_profile: &SemanticProviderProfile,
    sources: &[SourceFileState],
    repository_files: &[String],
    language: String,
) -> Result<EvaluatedLanguageCoverage, AppError> {
    let project_profile = language_profiles
        .iter()
        .find(|profile| profile.language.eq_ignore_ascii_case(&language))
        .ok_or_else(|| {
            AppError::ScipProfileMismatch(format!(
                "provider profile={} 映射到未声明 language={language}",
                provider_profile.id
            ))
        })?;
    let extensions = recognized_extensions(&language);
    let provider_documents = sources
        .iter()
        .filter(|source| source.language.as_str().eq_ignore_ascii_case(&language))
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();
    if extensions.is_empty() {
        let report = LanguageCoverageReport {
            language,
            status: "unverifiable",
            expected_source_files: 0,
            indexed_source_files: 0,
            provider_documents: count(&provider_documents),
            missing_source_files: 0,
            missing_source_file_sample: Vec::new(),
            provider_only_files: count(&provider_documents),
            provider_only_file_sample: sample(&provider_documents),
            recognized_extensions: extensions,
        };
        return Ok(EvaluatedLanguageCoverage {
            report,
            expected: BTreeSet::new(),
            indexed: BTreeSet::new(),
            provider_documents,
            unverifiable: true,
        });
    }
    let expected = repository_files
        .iter()
        .filter(|path| profile_contains(project_profile, path))
        .filter(|path| has_extension(path, &extensions))
        .filter(|path| root.join(PathBuf::from(path)).is_file())
        .cloned()
        .collect::<BTreeSet<_>>();
    let indexed = expected
        .intersection(&provider_documents)
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing = expected
        .difference(&provider_documents)
        .cloned()
        .collect::<BTreeSet<_>>();
    let provider_only = provider_documents
        .difference(&expected)
        .cloned()
        .collect::<BTreeSet<_>>();
    let report = LanguageCoverageReport {
        language,
        status: if missing.is_empty() {
            "complete"
        } else {
            "partial"
        },
        expected_source_files: count(&expected),
        indexed_source_files: count(&indexed),
        provider_documents: count(&provider_documents),
        missing_source_files: count(&missing),
        missing_source_file_sample: sample(&missing),
        provider_only_files: count(&provider_only),
        provider_only_file_sample: sample(&provider_only),
        recognized_extensions: extensions,
    };
    Ok(EvaluatedLanguageCoverage {
        report,
        expected,
        indexed,
        provider_documents,
        unverifiable: false,
    })
}

fn recognized_extensions(language: &str) -> Vec<&'static str> {
    match language {
        "rust" => vec!["rs"],
        "python" => vec!["py", "pyi", "pyw"],
        "csharp" | "c#" => vec!["cs"],
        "visualbasic" | "visual-basic" | "visual_basic" | "vb" => vec!["vb"],
        "fsharp" | "f#" => vec!["fs", "fsi", "fsx"],
        _ => Vec::new(),
    }
}

fn profile_contains(profile: &ProjectLanguageProfile, path: &str) -> bool {
    profile.roots.is_empty() || profile.roots.iter().any(|root| path_has_prefix(path, root))
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
}

fn count(values: &BTreeSet<String>) -> u64 {
    u64::try_from(values.len()).unwrap_or(u64::MAX)
}

fn sample(values: &BTreeSet<String>) -> Vec<String> {
    values
        .iter()
        .take(COVERAGE_PATH_SAMPLE_LIMIT)
        .cloned()
        .collect()
}

fn validate_project_roots(
    snapshot: &brain_symbols::SymbolSnapshot,
    profiles: &[ProjectLanguageProfile],
) -> Result<(), AppError> {
    for source in &snapshot.sources {
        let profile = profiles
            .iter()
            .find(|profile| {
                profile
                    .language
                    .eq_ignore_ascii_case(source.language.as_str())
            })
            .ok_or_else(|| {
                AppError::ScipProfileMismatch(format!(
                    "未声明 language={}",
                    source.language.as_str()
                ))
            })?;
        if !profile.roots.is_empty()
            && !profile
                .roots
                .iter()
                .any(|root| path_has_prefix(&source.path, root))
        {
            return Err(AppError::ScipProfileMismatch(format!(
                "{} 越出 language={} 声明 roots",
                source.path,
                source.language.as_str()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_core::{
        ProjectLanguageProfile, SemanticLanguageMapping, SemanticProviderFormat,
        SemanticProviderProfile,
    };
    use brain_store::{BrainStore, SemanticSnapshotSource};
    use brain_symbols::{
        IdentityQuality, ProviderDescriptor, SourceFileState, SourceLanguage, SymbolSnapshot,
    };

    use super::{
        LanguageCoverageReport, SemanticCoverageReport, doctor_coverage, document_manifest_hash,
        ensure_committable_coverage, evaluate_coverage_from_files, import_profile,
        validate_project_roots,
    };
    use crate::git;

    fn snapshot(language: SourceLanguage, path: &str) -> SymbolSnapshot {
        SymbolSnapshot::for_worktree(
            "project_test",
            ProviderDescriptor {
                id: "scip-fixture-v1".to_owned(),
                version: "contract-1".to_owned(),
                identity_quality: IdentityQuality::Semantic,
            },
            "head",
            vec![SourceFileState::from_source(
                path, language, b"source", false,
            )],
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn project_language_profiles_gate_language_and_root() {
        let csharp = snapshot(SourceLanguage::csharp(), "src/App/Program.cs");
        let allowed = vec![ProjectLanguageProfile {
            language: "csharp".to_owned(),
            roots: vec!["src/App".to_owned()],
        }];
        assert!(validate_project_roots(&csharp, &allowed).is_ok());
        let project_root = vec![ProjectLanguageProfile {
            language: "csharp".to_owned(),
            roots: vec![".".to_owned()],
        }];
        assert!(validate_project_roots(&csharp, &project_root).is_ok());
        assert!(validate_project_roots(&csharp, &[]).is_err());

        let outside = snapshot(SourceLanguage::csharp(), "tests/AppTests.cs");
        assert!(validate_project_roots(&outside, &allowed).is_err());
    }

    #[test]
    fn coverage_reports_missing_declared_sources_without_guessing_unknown_languages() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-coverage-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.py"), "def a(): pass\n").unwrap();
        fs::write(root.join("src/b.py"), "def b(): pass\n").unwrap();
        let profiles = vec![ProjectLanguageProfile {
            language: "python".to_owned(),
            roots: vec!["src".to_owned()],
        }];
        let provider = SemanticProviderProfile {
            id: "python-main".to_owned(),
            format: SemanticProviderFormat::Scip,
            producer: "scip-python".to_owned(),
            contract_version: 1,
            language_mappings: vec![SemanticLanguageMapping {
                raw_language: Some("python".to_owned()),
                language: "python".to_owned(),
                allow_missing_language: false,
            }],
        };
        let indexed = vec![SourceFileState::from_source(
            "src/a.py",
            SourceLanguage::python(),
            b"def a(): pass\n",
            false,
        )];
        let report = evaluate_coverage_from_files(
            &root,
            &profiles,
            &provider,
            &indexed,
            &["src/a.py".to_owned(), "src/b.py".to_owned()],
        )
        .unwrap();
        assert_eq!(report.status, "partial");
        assert_eq!(report.expected_source_files, 2);
        assert_eq!(report.indexed_source_files, 1);
        assert_eq!(report.missing_source_file_sample, vec!["src/b.py"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_complete_coverage_can_be_committed() {
        let report = |status| SemanticCoverageReport {
            status,
            expected_source_files: 2,
            indexed_source_files: if status == "complete" { 2 } else { 1 },
            provider_documents: 2,
            missing_source_files: u64::from(status != "complete"),
            missing_source_file_sample: if status == "complete" {
                Vec::new()
            } else {
                vec!["src/missing.rs".to_owned()]
            },
            provider_only_files: 0,
            provider_only_file_sample: Vec::new(),
            languages: vec![LanguageCoverageReport {
                language: "rust".to_owned(),
                status,
                expected_source_files: 2,
                indexed_source_files: 1,
                provider_documents: 1,
                missing_source_files: 1,
                missing_source_file_sample: vec!["src/missing.rs".to_owned()],
                provider_only_files: 0,
                provider_only_file_sample: Vec::new(),
                recognized_extensions: vec!["rs"],
            }],
        };

        assert!(ensure_committable_coverage(&report("complete")).is_ok());
        assert!(ensure_committable_coverage(&report("partial")).is_err());
        assert!(ensure_committable_coverage(&report("unverifiable")).is_err());
    }

    #[test]
    fn stability_document_manifest_is_order_independent_and_path_sensitive() {
        let first =
            SourceFileState::from_source("src/a.rs", SourceLanguage::rust(), b"fn a() {}", false);
        let second =
            SourceFileState::from_source("src/b.rs", SourceLanguage::rust(), b"fn b() {}", false);
        assert_eq!(
            document_manifest_hash(&[first.clone(), second.clone()]),
            document_manifest_hash(&[second.clone(), first])
        );
        assert_ne!(
            document_manifest_hash(std::slice::from_ref(&second)),
            document_manifest_hash(&[SourceFileState {
                path: "src/c.rs".to_owned(),
                ..second
            }])
        );
    }

    #[test]
    fn doctor_degrades_when_recorded_provider_manifest_is_partial() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-coverage-doctor-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.py"), "def a(): pass\n").unwrap();
        fs::write(root.join("src/b.py"), "def b(): pass\n").unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let profiles = vec![ProjectLanguageProfile {
            language: "python".to_owned(),
            roots: vec!["src".to_owned()],
        }];
        let provider = SemanticProviderProfile {
            id: "python-main".to_owned(),
            format: SemanticProviderFormat::Scip,
            producer: "scip-python".to_owned(),
            contract_version: 1,
            language_mappings: vec![SemanticLanguageMapping {
                raw_language: None,
                language: "python".to_owned(),
                allow_missing_language: true,
            }],
        };
        let contract_id = brain_scip::provider_contract_id(&import_profile(&provider).unwrap());
        let snapshot = SymbolSnapshot::for_worktree(
            "project_test",
            ProviderDescriptor {
                id: contract_id,
                version: "contract-1".to_owned(),
                identity_quality: IdentityQuality::Semantic,
            },
            &git::head_revision(&root).unwrap(),
            vec![SourceFileState::from_source(
                "src/a.py",
                SourceLanguage::python(),
                b"def a(): pass\n",
                false,
            )],
            Vec::new(),
            Vec::new(),
        );
        let store = BrainStore::open_in_memory().unwrap();
        store
            .apply_semantic_snapshot(
                &snapshot,
                "python-main",
                &[],
                &[],
                &SemanticSnapshotSource::offline(
                    git::worktree_fingerprint(&root).unwrap(),
                    git::head_revision(&root).unwrap(),
                    false,
                ),
            )
            .unwrap();
        let report = doctor_coverage(&root, "project_test", &profiles, &[provider], &store);
        assert_eq!(report.status, "degraded");
        assert_eq!(report.profiles[0].status, "partial");
        assert_eq!(
            report.profiles[0]
                .coverage
                .as_ref()
                .unwrap()
                .missing_source_file_sample,
            vec!["src/b.py"]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
