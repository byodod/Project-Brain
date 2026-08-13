use crate::{
    ActionDescriptor, BrainConfig, CURRENT_SCHEMA_VERSION, CoreError, Decision, DecisionKind,
    Evidence, MemoryStatus, Rule, RuleEffect, path_has_prefix,
};

/// 对一组已经验证的仓库规则执行确定性匹配和决策聚合。
pub struct RuleEngine<'a> {
    config: &'a BrainConfig,
}

impl<'a> RuleEngine<'a> {
    /// 创建规则引擎并验证配置。
    ///
    /// # Errors
    ///
    /// 当 schema 版本不受支持，或规则拥有无效的阻断权限组合时返回错误。
    pub fn new(config: &'a BrainConfig) -> Result<Self, CoreError> {
        config.validate()?;
        Ok(Self { config })
    }

    /// 对一个标准化动作进行评估。
    ///
    /// # Errors
    ///
    /// 当动作 schema 版本不受支持时返回错误。
    pub fn evaluate(&self, action: &ActionDescriptor) -> Result<Decision, CoreError> {
        action.validate()?;

        let matching: Vec<&Rule> = self
            .config
            .rules
            .iter()
            .filter(|rule| rule.status == MemoryStatus::Active)
            .filter(|rule| matches_rule(rule, action))
            .collect();

        let evidence: Vec<Evidence> = matching
            .iter()
            .map(|rule| Evidence {
                rule_id: rule.id.clone(),
                effect: rule.effect,
                message: rule.message.clone(),
                rationale: rule.rationale.clone(),
            })
            .collect();

        let context: Vec<String> = matching
            .iter()
            .filter(|rule| rule.effect != RuleEffect::Block)
            .map(|rule| rule.message.clone())
            .collect();

        let decision = if matching.iter().any(|rule| rule.effect == RuleEffect::Block) {
            DecisionKind::Block
        } else if matching
            .iter()
            .any(|rule| rule.effect == RuleEffect::Escalate)
        {
            DecisionKind::Escalate
        } else if matching
            .iter()
            .any(|rule| rule.effect == RuleEffect::InjectContext)
        {
            DecisionKind::AllowWithContext
        } else {
            DecisionKind::Allow
        };

        let summary = match decision {
            DecisionKind::Allow => "未命中需要改变行为的规则".to_owned(),
            DecisionKind::AllowWithContext => "允许执行，并注入相关项目约束".to_owned(),
            DecisionKind::Block => "命中确定性硬规则，拒绝执行".to_owned(),
            DecisionKind::Escalate => "需要显式决策后再继续".to_owned(),
        };

        Ok(Decision {
            schema_version: CURRENT_SCHEMA_VERSION,
            decision,
            summary,
            context,
            evidence,
        })
    }
}

fn matches_rule(rule: &Rule, action: &ActionDescriptor) -> bool {
    let action_matches = rule.actions.is_empty() || rule.actions.contains(&action.action);
    if !action_matches {
        return false;
    }

    let operation_name_matches = rule.operations.is_empty()
        || rule
            .operations
            .iter()
            .any(|operation| operation.eq_ignore_ascii_case(&action.operation));
    if !operation_name_matches {
        return false;
    }

    let searchable_operation = format!(
        "{} {}",
        action.operation,
        action.command.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    let operation_matches = rule.operation_contains.is_empty()
        || rule
            .operation_contains
            .iter()
            .any(|needle| searchable_operation.contains(&needle.to_lowercase()));
    if !operation_matches {
        return false;
    }

    let path_matches = rule.include_paths.is_empty()
        || action.target_files.iter().any(|target| {
            rule.include_paths
                .iter()
                .any(|prefix| path_has_prefix(target, prefix))
        });
    if !path_matches {
        return false;
    }

    !action.target_files.iter().any(|target| {
        rule.exclude_paths
            .iter()
            .any(|prefix| path_has_prefix(target, prefix))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::RuleEngine;
    use crate::{
        ActionDescriptor, ActionKind, Authority, BrainConfig, CURRENT_SCHEMA_VERSION, CoreError,
        DecisionKind, MemoryStatus, Rule, RuleEffect, RuleStrength, StopReconcileConfig,
    };

    fn action(kind: ActionKind, path: &str) -> ActionDescriptor {
        ActionDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            event_id: "event-1".to_owned(),
            session_id: "session-1".to_owned(),
            cwd: "/repo".to_owned(),
            action: kind,
            operation: "apply_patch".to_owned(),
            target_files: vec![path.to_owned()],
            command: None,
            metadata: BTreeMap::new(),
        }
    }

    fn rule(id: &str, effect: RuleEffect, strength: RuleStrength) -> Rule {
        Rule {
            id: id.to_owned(),
            status: MemoryStatus::Active,
            authority: Authority::RepositoryRule,
            strength,
            effect,
            include_paths: vec!["src/domain".to_owned()],
            exclude_paths: Vec::new(),
            actions: vec![ActionKind::Modify],
            operations: Vec::new(),
            operation_contains: Vec::new(),
            message: format!("message for {id}"),
            rationale: String::new(),
        }
    }

    #[test]
    fn hard_block_has_priority_over_context() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![
                rule("context", RuleEffect::InjectContext, RuleStrength::Soft),
                rule("block", RuleEffect::Block, RuleStrength::Hard),
            ],
        };
        let decision = RuleEngine::new(&config)
            .unwrap()
            .evaluate(&action(ActionKind::Modify, "src/domain/model.rs"))
            .unwrap();

        assert_eq!(decision.decision, DecisionKind::Block);
        assert_eq!(decision.evidence.len(), 2);
    }

    #[test]
    fn unrelated_paths_are_allowed() {
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![rule("block", RuleEffect::Block, RuleStrength::Hard)],
        };
        let decision = RuleEngine::new(&config)
            .unwrap()
            .evaluate(&action(ActionKind::Modify, "src/ui/view.rs"))
            .unwrap();

        assert_eq!(decision.decision, DecisionKind::Allow);
    }

    #[test]
    fn inferred_rules_cannot_receive_blocking_authority() {
        let mut invalid = rule("inferred", RuleEffect::Block, RuleStrength::Hard);
        invalid.authority = Authority::AgentInference;
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![invalid],
        };

        assert!(matches!(
            RuleEngine::new(&config),
            Err(CoreError::InvalidRule { .. })
        ));
    }

    #[test]
    fn inactive_rules_do_not_change_decisions() {
        let mut inactive = rule("old", RuleEffect::Block, RuleStrength::Hard);
        inactive.status = MemoryStatus::Superseded;
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![inactive],
        };
        let decision = RuleEngine::new(&config)
            .unwrap()
            .evaluate(&action(ActionKind::Modify, "src/domain/model.rs"))
            .unwrap();

        assert_eq!(decision.decision, DecisionKind::Allow);
    }

    #[test]
    fn operation_constraint_prevents_cross_adapter_false_activation() {
        let mut bash_only = rule("bash-only", RuleEffect::Block, RuleStrength::Hard);
        bash_only.operations = vec!["Bash".to_owned()];
        let config = BrainConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            project_key: "project_test".to_owned(),
            project_name: "test".to_owned(),
            stop_reconcile: StopReconcileConfig::default(),
            rules: vec![bash_only],
        };
        let decision = RuleEngine::new(&config)
            .unwrap()
            .evaluate(&action(ActionKind::Modify, "src/domain/model.rs"))
            .unwrap();

        assert_eq!(decision.decision, DecisionKind::Allow);
    }
}
