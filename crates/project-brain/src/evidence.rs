use std::path::Path;

use brain_evidence::{EvidenceFreshness, EvidenceInputManifestV1};

use crate::{evidence_inputs, git};

/// 当前工作树 Source 指纹的实时验证结果。
///
/// 这是消费 Evidence authority 时的独立信任输入，不会覆盖 ledger 中记录的 freshness。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CurrentSourceVerification {
    Verified(String),
    Unavailable(String),
}

impl CurrentSourceVerification {
    pub(crate) fn inspect(root: &Path) -> Self {
        match git::worktree_fingerprint(root) {
            Ok(fingerprint) => Self::Verified(fingerprint),
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    pub(crate) fn fingerprint(&self) -> Option<&str> {
        match self {
            Self::Verified(fingerprint) => Some(fingerprint),
            Self::Unavailable(_) => None,
        }
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Self::Verified(_) => None,
            Self::Unavailable(error) => Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveEvidenceFreshness {
    pub(crate) freshness: EvidenceFreshness,
    pub(crate) reason: Option<String>,
}

/// 将 ledger 中的持久化 freshness 与当前 Source 指纹合成为权限消费状态。
///
/// 该函数只能保持或降低信任：
/// - 已记录 stale 永远保持 stale；
/// - 当前指纹不一致会把 fresh/unknown 降为 stale；
/// - 当前指纹不可验证会把 fresh 降为 unknown；
/// - 指纹重新相同也不会把 stale/unknown 自动恢复为 fresh。
#[cfg(test)]
pub(crate) fn effective_evidence_freshness(
    recorded: EvidenceFreshness,
    evidence_source_fingerprint: &str,
    current: &CurrentSourceVerification,
) -> EffectiveEvidenceFreshness {
    if recorded == EvidenceFreshness::Stale {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Stale,
            reason: None,
        };
    }
    match current {
        CurrentSourceVerification::Verified(current_fingerprint)
            if current_fingerprint != evidence_source_fingerprint =>
        {
            EffectiveEvidenceFreshness {
                freshness: EvidenceFreshness::Stale,
                reason: Some(format!(
                    "Evidence Source fingerprint={evidence_source_fingerprint} 与当前 Source fingerprint={current_fingerprint} 不一致"
                )),
            }
        }
        CurrentSourceVerification::Unavailable(error) => EffectiveEvidenceFreshness {
            freshness: match recorded {
                EvidenceFreshness::Fresh | EvidenceFreshness::Unknown => EvidenceFreshness::Unknown,
                EvidenceFreshness::Stale => unreachable!("stale 已在前面返回"),
            },
            reason: Some(format!("当前 Source fingerprint 无法验证：{error}")),
        },
        CurrentSourceVerification::Verified(_) => EffectiveEvidenceFreshness {
            freshness: recorded,
            reason: None,
        },
    }
}

/// EffectiveFreshnessV2：整项目 Source 相同时沿用 v1 快路径；Source 不同时只有
/// input-aware、覆盖完整且实时重算完全一致的 persisted-fresh head 才保持 fresh。
/// 任何持久化降权都不会被实时验证自动恢复。
pub(crate) fn effective_evidence_freshness_v2(
    root: &Path,
    recorded: EvidenceFreshness,
    evidence_source_fingerprint: &str,
    input_manifest: Option<&EvidenceInputManifestV1>,
    current: &CurrentSourceVerification,
) -> EffectiveEvidenceFreshness {
    if recorded != EvidenceFreshness::Fresh {
        return EffectiveEvidenceFreshness {
            freshness: recorded,
            reason: None,
        };
    }
    let CurrentSourceVerification::Verified(current_fingerprint) = current else {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Unknown,
            reason: current
                .error()
                .map(|error| format!("当前 Source fingerprint 无法验证：{error}")),
        };
    };
    if current_fingerprint == evidence_source_fingerprint {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Fresh,
            reason: None,
        };
    }
    let Some(input_manifest) = input_manifest else {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Stale,
            reason: Some(format!(
                "LegacyProjectWide Evidence Source fingerprint={evidence_source_fingerprint} 与当前 Source fingerprint={current_fingerprint} 不一致"
            )),
        };
    };
    if input_manifest.source_fingerprint_at_creation != evidence_source_fingerprint {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Unknown,
            reason: Some(
                "Evidence Input Manifest 与 Snapshot Source fingerprint 绑定不一致".to_owned(),
            ),
        };
    }
    if !input_manifest.contract.coverage.hard_authority_eligible() {
        return EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Unknown,
            reason: Some(
                "Evidence input dependency coverage=incomplete，不能保留 hard authority".to_owned(),
            ),
        };
    }
    match evidence_inputs::resolve_stable(root, &input_manifest.contract) {
        Ok(current_manifest)
            if current_manifest.source_fingerprint_at_creation != *current_fingerprint =>
        {
            EffectiveEvidenceFreshness {
                freshness: EvidenceFreshness::Unknown,
                reason: Some(
                    "Evidence input manifest 验证期间 whole Source 发生并发变化".to_owned(),
                ),
            }
        }
        Ok(current_manifest) if current_manifest.manifest_hash == input_manifest.manifest_hash => {
            EffectiveEvidenceFreshness {
                freshness: EvidenceFreshness::Fresh,
                reason: Some(format!(
                    "当前 whole Source 已变化，但完整 Evidence input manifest={} 仍逐字一致",
                    input_manifest.manifest_hash
                )),
            }
        }
        Ok(current_manifest) => EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Stale,
            reason: Some(format!(
                "Evidence input manifest 已变化：stored={}, current={}",
                input_manifest.manifest_hash, current_manifest.manifest_hash
            )),
        },
        Err(error) => EffectiveEvidenceFreshness {
            freshness: EvidenceFreshness::Unknown,
            reason: Some(format!("Evidence input manifest 实时验证失败：{error}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use brain_evidence::{DependencyCoverage, EvidenceFreshness, InputDependencyContractV1};

    use super::{
        CurrentSourceVerification, effective_evidence_freshness, effective_evidence_freshness_v2,
    };
    use crate::{evidence_inputs, git};

    #[test]
    fn effective_freshness_never_restores_recorded_non_fresh_evidence() {
        let current = CurrentSourceVerification::Verified("source-a".to_owned());
        assert_eq!(
            effective_evidence_freshness(EvidenceFreshness::Stale, "source-a", &current).freshness,
            EvidenceFreshness::Stale
        );
        assert_eq!(
            effective_evidence_freshness(EvidenceFreshness::Unknown, "source-a", &current)
                .freshness,
            EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn source_mismatch_is_stale_and_unavailable_source_is_unknown() {
        let mismatch = CurrentSourceVerification::Verified("source-b".to_owned());
        assert_eq!(
            effective_evidence_freshness(EvidenceFreshness::Fresh, "source-a", &mismatch).freshness,
            EvidenceFreshness::Stale
        );
        assert_eq!(
            effective_evidence_freshness(EvidenceFreshness::Unknown, "source-a", &mismatch)
                .freshness,
            EvidenceFreshness::Stale
        );

        let unavailable = CurrentSourceVerification::Unavailable("git unavailable".to_owned());
        assert_eq!(
            effective_evidence_freshness(EvidenceFreshness::Fresh, "source-a", &unavailable)
                .freshness,
            EvidenceFreshness::Unknown
        );
    }

    #[test]
    fn v2_preserves_only_unchanged_complete_scoped_inputs() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "project-brain-effective-freshness-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.py"), "VALUE = 1\n").unwrap();
        fs::write(root.join("README.md"), "first\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&root)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );

        let contract =
            evidence_inputs::python_compile_contract("project-a", "main", "src", 1).unwrap();
        let stored = evidence_inputs::resolve_stable(&root, &contract).unwrap();
        let evidence_source = stored.source_fingerprint_at_creation.clone();

        fs::write(root.join("README.md"), "unrelated change\n").unwrap();
        let unrelated_source = git::worktree_fingerprint(&root).unwrap();
        assert_ne!(evidence_source, unrelated_source);
        assert_eq!(
            effective_evidence_freshness_v2(
                &root,
                EvidenceFreshness::Fresh,
                &evidence_source,
                Some(&stored),
                &CurrentSourceVerification::Verified(unrelated_source.clone()),
            )
            .freshness,
            EvidenceFreshness::Fresh
        );

        fs::write(root.join("src/main.py"), "VALUE = 2\n").unwrap();
        let changed_source = git::worktree_fingerprint(&root).unwrap();
        assert_eq!(
            effective_evidence_freshness_v2(
                &root,
                EvidenceFreshness::Fresh,
                &evidence_source,
                Some(&stored),
                &CurrentSourceVerification::Verified(changed_source.clone()),
            )
            .freshness,
            EvidenceFreshness::Stale
        );
        assert_eq!(
            effective_evidence_freshness_v2(
                &root,
                EvidenceFreshness::Unknown,
                &evidence_source,
                Some(&stored),
                &CurrentSourceVerification::Verified(evidence_source.clone()),
            )
            .freshness,
            EvidenceFreshness::Unknown
        );

        let incomplete_contract = InputDependencyContractV1::new(
            &contract.project_key,
            &contract.profile_id,
            &contract.provider_contract_id,
            contract.provider_contract_version,
            &contract.profile_contract_hash,
            contract.selectors.clone(),
            DependencyCoverage::Incomplete,
        )
        .unwrap();
        let incomplete = evidence_inputs::resolve_stable(&root, &incomplete_contract).unwrap();
        assert_eq!(
            effective_evidence_freshness_v2(
                &root,
                EvidenceFreshness::Fresh,
                &incomplete.source_fingerprint_at_creation,
                Some(&incomplete),
                &CurrentSourceVerification::Verified("different-source".to_owned()),
            )
            .freshness,
            EvidenceFreshness::Unknown
        );

        fs::remove_dir_all(root).unwrap();
    }
}
