use std::path::Path;

use brain_core::{
    ProjectLanguageProfile, SemanticProviderFormat, SemanticProviderProfile, path_has_prefix,
};
use brain_scip::{
    ScipImport, ScipImportProfile, ScipImportStats, ScipLanguageCapabilities, ScipLanguageMapping,
};
use brain_store::{BrainStore, SemanticApplyResult, SemanticSnapshotSource};
use brain_symbols::{ProviderDescriptor, SourceLanguage};
use serde::Serialize;

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
    pub lineage_observations: u64,
    pub apply: SemanticApplyResult,
}

pub struct PreparedScipIndex {
    project_key: String,
    provider_profile: String,
    imported: ScipImport,
    source: SemanticSnapshotSource,
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
    let import_profile = ScipImportProfile {
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
    };

    let head_revision = git::head_revision(root)?;
    let source = SemanticSnapshotSource::offline(
        git::worktree_fingerprint(root)?,
        head_revision.clone(),
        git::worktree_is_clean(root)?,
    );
    let imported =
        brain_scip::import_file(root, project_key, &head_revision, input, &import_profile)?;
    validate_project_roots(&imported.snapshot, language_profiles)?;
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
    })
}

pub fn commit(
    store: &BrainStore,
    prepared: PreparedScipIndex,
) -> Result<ScipIndexReport, AppError> {
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
        lineage_observations: u64::try_from(prepared.imported.lineage_observations.len())
            .unwrap_or(u64::MAX),
        apply,
    })
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
    use brain_core::ProjectLanguageProfile;
    use brain_symbols::{
        IdentityQuality, ProviderDescriptor, SourceFileState, SourceLanguage, SymbolSnapshot,
    };

    use super::validate_project_roots;

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
}
