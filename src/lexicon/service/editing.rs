use super::*;

// --- editor ---

impl LexiconService {}

impl LexiconService {}

// --- editor support ---

// --- storage validation ---

/// 存储安全网拦下内容时回给调用方的 issue 列表。
///
/// `meaning_storage_is_safe` / `canonicalize_meanings` 与 `validate_meanings` 是两份
/// 独立维护的判定清单，判定条件并不完全重合。安全网拦下、而 `validate_meanings` 恰好
/// 没产出任何 issue 时，兜一条存储层问题，避免回出 `field_issues` 为空、前端无处定位
/// 的 422。
pub(super) fn meanings_storage_issues(
    entry_id: Uuid,
    issues: Vec<DraftValidationIssue>,
) -> Vec<DraftValidationIssue> {
    if !issues.is_empty() {
        return issues;
    }
    // 走到这里说明两份清单已经漂移：安全网拦下了 validate_meanings 认可的内容。
    // 兜底保住了响应体，但漂移本身是要修的，留一条日志让它可观测而不是被悄悄抹平。
    tracing::warn!(
        %entry_id,
        "存储安全网拦下了 validate_meanings 未报错的词义内容，两份判定清单可能已漂移"
    );
    vec![DraftValidationIssue {
        step: PersistedWordStep::Meanings,
        node_id: entry_id,
        field: "content".to_owned(),
        code: "meanings_storage_unsafe".to_owned(),
        message: "词义内容无法安全存储".to_owned(),
        reference_location: None,
        node_location: None,
    }]
}

pub(super) fn meaning_storage_is_safe(
    entry_id: Uuid,
    forms: &DraftFormsStepContent,
    meanings: &DraftMeaningsStepContent,
    sub_part_parents: &HashMap<String, String>,
) -> bool {
    let pos_codes = forms
        .pos
        .iter()
        .map(|pos| (pos.pos_id, pos.pos.as_str()))
        .collect::<HashMap<_, _>>();
    let group_ids = meanings
        .sense_groups
        .iter()
        .map(|group| group.id)
        .collect::<std::collections::HashSet<_>>();
    let mut seen_pos = std::collections::HashSet::new();
    let mut node_ids = std::collections::HashSet::new();
    for group in &meanings.sense_groups {
        if !node_ids.insert(group.id)
            || group.name_zh.chars().count() > 200
            || group.name_en.chars().count() > 200
        {
            return false;
        }
    }
    for pos in &meanings.pos {
        let Some(pos_code) = pos_codes.get(&pos.pos_id).copied() else {
            return false;
        };
        if !seen_pos.insert(pos.pos_id) {
            return false;
        }
        let grammar_ids = pos
            .grammar_structures
            .iter()
            .map(|grammar| grammar.id)
            .collect::<std::collections::HashSet<_>>();
        for grammar in &pos.grammar_structures {
            if !node_ids.insert(grammar.id) {
                return false;
            }
            let mut dialects = std::collections::HashSet::new();
            for variant in &grammar.variants {
                if !node_ids.insert(variant.id) || !dialects.insert(variant.dialect) {
                    return false;
                }
            }
        }
        for sense in &pos.senses {
            if !node_ids.insert(sense.id)
                || !valid_level(&sense.level)
                || sense
                    .frequency
                    .as_deref()
                    .is_some_and(|value| !valid_fixed_percent(value))
                || sense
                    .sense_group_id
                    .is_some_and(|group_id| !group_ids.contains(&group_id))
                || (!sense.sub_pos.is_empty()
                    && sub_part_parents
                        .get(&sense.sub_pos)
                        .is_none_or(|parent| parent != pos_code))
            {
                return false;
            }
            for definition in &sense.definitions {
                let id = definition_id(definition);
                let grammar_id = definition_grammar_id(definition);
                if !node_ids.insert(id)
                    || !valid_level(definition_level(definition))
                    || grammar_id.is_some_and(|id| !grammar_ids.contains(&id))
                {
                    return false;
                }
            }
            for sentence in &sense.sentences {
                let mut link_targets = std::collections::HashSet::new();
                let focus = sentence
                    .links
                    .iter()
                    .filter(|link| link.role == "focus")
                    .collect::<Vec<_>>();
                if !node_ids.insert(sentence.id)
                    || !valid_level(&sentence.level)
                    || focus.len() > 1
                    || focus
                        .first()
                        .is_some_and(|link| link.word_id != entry_id || link.sense_id != sense.id)
                    || sentence.links.iter().any(|link| {
                        !matches!(link.role.as_str(), "focus" | "context")
                            || !link_targets.insert((link.word_id, link.sense_id))
                    })
                {
                    return false;
                }
            }
            for relation in &sense.relations {
                if !node_ids.insert(relation.id)
                    || !matches!(
                        relation.relation.as_str(),
                        "synonym" | "antonym" | "derivative"
                    )
                    || !valid_fixed_percent(&relation.score)
                {
                    return false;
                }
            }
        }
    }
    true
}

pub(super) fn definition_id(definition: &WordDefinitionV2) -> Uuid {
    match definition {
        WordDefinitionV2::ZhDefinition { id, .. }
        | WordDefinitionV2::ZhSentence { id, .. }
        | WordDefinitionV2::EnDefinition { id, .. }
        | WordDefinitionV2::EnSentence { id, .. } => *id,
    }
}

pub(super) fn definition_grammar_id(definition: &WordDefinitionV2) -> Option<Uuid> {
    match definition {
        WordDefinitionV2::ZhDefinition {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::ZhSentence {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::EnDefinition {
            grammar_structure_id,
            ..
        }
        | WordDefinitionV2::EnSentence {
            grammar_structure_id,
            ..
        } => *grammar_structure_id,
    }
}

pub(super) fn definition_level(definition: &WordDefinitionV2) -> &str {
    match definition {
        WordDefinitionV2::ZhDefinition { level, .. }
        | WordDefinitionV2::ZhSentence { level, .. }
        | WordDefinitionV2::EnDefinition { level, .. }
        | WordDefinitionV2::EnSentence { level, .. } => level,
    }
}

pub(super) fn valid_fixed_percent(value: &str) -> bool {
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let decimal = parts.next();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.chars().all(|character| character.is_ascii_digit())
        || decimal.is_some_and(|value| {
            value.is_empty()
                || value.len() > 2
                || !value.chars().all(|character| character.is_ascii_digit())
        })
    {
        return false;
    }
    value
        .parse::<f64>()
        .is_ok_and(|number| (0.0..=100.0).contains(&number))
}

#[cfg(test)]
mod meanings_storage_issue_tests {
    use super::*;

    #[test]
    fn storage_rejection_never_returns_an_empty_issue_list() {
        let entry_id = Uuid::now_v7();
        let issues = meanings_storage_issues(entry_id, Vec::new());
        assert_eq!(issues.len(), 1, "空 issues 必须兜一条存储层问题");
        assert_eq!(issues[0].code, "meanings_storage_unsafe");
        assert_eq!(issues[0].step, PersistedWordStep::Meanings);
        assert_eq!(
            issues[0].node_id, entry_id,
            "存储层问题锚定到词条本身，前端才有定位落点"
        );
    }

    #[test]
    fn existing_issues_are_passed_through_untouched() {
        let entry_id = Uuid::now_v7();
        let node_id = Uuid::now_v7();
        let original = vec![DraftValidationIssue {
            step: PersistedWordStep::Meanings,
            node_id,
            field: "pos_id".to_owned(),
            code: "pos_not_found".to_owned(),
            message: "词义引用了不存在的基本词性".to_owned(),
            reference_location: None,
            node_location: None,
        }];
        let issues = meanings_storage_issues(entry_id, original);
        assert_eq!(issues.len(), 1, "已有具体 issue 时不得被兜底问题稀释");
        assert_eq!(
            issues[0].code, "pos_not_found",
            "已有具体 issue 时不得被兜底问题替换"
        );
        assert_eq!(
            issues[0].node_id, node_id,
            "具体 issue 的节点定位必须原样保留"
        );
    }
}
