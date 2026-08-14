use std::path::Path;

use brain_evidence::EvidenceFreshness;

use crate::git;

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

#[cfg(test)]
mod tests {
    use brain_evidence::EvidenceFreshness;

    use super::{CurrentSourceVerification, effective_evidence_freshness};

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
}
