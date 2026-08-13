use std::path::Path;

use brain_core::{
    ProjectLanguageProfile, SemanticProviderFormat, SemanticProviderProfile, path_has_prefix,
};
use brain_scip::{
    ScipImportProfile, ScipImportStats, ScipLanguageCapabilities, ScipLanguageMapping,
};
use brain_store::{BrainStore, SemanticApplyResult};
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
    pub stats: ScipImportStats,
    pub lineage_observations: u64,
    pub apply: SemanticApplyResult,
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
    let imported =
        brain_scip::import_file(root, project_key, &head_revision, input, &import_profile)?;
    validate_project_roots(&imported.snapshot, language_profiles)?;
    let apply = store.apply_semantic_snapshot(
        &imported.snapshot,
        &configured.id,
        &imported.lineage_observations,
        &[],
    )?;
    Ok(ScipIndexReport {
        schema_version: brain_core::CURRENT_SCHEMA_VERSION,
        experimental: true,
        project_key: project_key.to_owned(),
        provider_profile: configured.id.clone(),
        provider: imported.snapshot.provider,
        capabilities: imported.capabilities,
        producer_name: imported.producer_name,
        producer_version: imported.producer_version,
        languages: imported.languages,
        source_revision: imported.snapshot.source_revision,
        stats: imported.stats,
        lineage_observations: u64::try_from(imported.lineage_observations.len())
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
