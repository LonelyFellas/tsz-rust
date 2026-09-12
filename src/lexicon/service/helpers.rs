use super::*;

/// 并列展示的方言顺序：common 或管理员主词侧在前。
/// 与列表行 SQL（repository/query.rs 的 ORDER BY CASE）保持同一规则。
pub(super) fn ordered_headword_sides(headwords: &WordHeadwordsV2) -> Vec<(Dialect, &str)> {
    match headwords {
        WordHeadwordsV2::Unified { common } => vec![(Dialect::Common, common.as_str())],
        WordHeadwordsV2::Distinguish {
            uk,
            us,
            source_dialect,
        } => match source_dialect {
            SourceDialect::Uk => vec![(Dialect::Uk, uk.as_str()), (Dialect::Us, us.as_str())],
            SourceDialect::Us => vec![(Dialect::Us, us.as_str()), (Dialect::Uk, uk.as_str())],
        },
    }
}

pub(super) fn normalize_submitted_headwords(
    headwords: &mut WordHeadwordsV2,
) -> Result<(), LexiconServiceError> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            *common = NormalizedHeadword::parse(common)
                .map_err(map_headword_error)?
                .display;
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            *uk = NormalizedHeadword::parse(uk)
                .map_err(map_headword_error)?
                .display;
            *us = NormalizedHeadword::parse(us)
                .map_err(map_headword_error)?
                .display;
        }
    }
    Ok(())
}

pub(super) fn relation_target_entry_ids(meanings: &DraftMeaningsStepContent) -> Vec<Uuid> {
    let mut entry_ids = meanings
        .pos
        .iter()
        .flat_map(|pos| pos.senses.iter())
        .flat_map(|sense| sense.relations.iter())
        // 待物化的关联词还没有目标词条，自然也没有需要一起锁的上下文。
        .filter_map(|relation| relation.target_word_id.or(relation.prebound_target_word_id))
        .collect::<Vec<_>>();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    entry_ids
}

pub(super) fn map_dictionary_pos(values: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    for value in values {
        let normalized = value.trim().to_ascii_lowercase();
        let mapped = match normalized.as_str() {
            "noun" | "name" | "proper noun" => Some("noun"),
            "pronoun" | "pron" => Some("pronoun"),
            "verb" | "auxiliary" | "modal" => Some("verb"),
            "adjective" | "adj" => Some("adjective"),
            "adverb" | "adv" => Some("adverb"),
            "preposition" | "prep" => Some("preposition"),
            "article" => Some("article"),
            "determiner" | "det" => Some("determiner"),
            "conjunction" | "conj" => Some("conjunction"),
            "numeral" | "number" | "num" => Some("numeral"),
            "interjection" | "intj" => Some("interjection"),
            _ => None,
        };
        if let Some(mapped) = mapped
            && !output.iter().any(|existing| existing == mapped)
        {
            output.push(mapped.to_owned());
        }
    }
    output
}

pub(super) fn family_dialect(family: &str) -> Option<Dialect> {
    match family {
        "british_core" | "british_influenced" => Some(Dialect::Uk),
        "american_core" | "american_influenced" => Some(Dialect::Us),
        _ => None,
    }
}

pub(super) fn parse_v3_kind(value: &str) -> Option<WordEntryKindV3> {
    match value {
        "word" => Some(WordEntryKindV3::Word),
        "phrase" => Some(WordEntryKindV3::Phrase),
        _ => None,
    }
}

pub(super) const fn v3_kind_string(kind: WordEntryKindV3) -> &'static str {
    match kind {
        WordEntryKindV3::Word => "word",
        WordEntryKindV3::Phrase => "phrase",
    }
}

pub(super) const fn entry_kind_from_v3(kind: WordEntryKindV3) -> EntryKind {
    match kind {
        WordEntryKindV3::Word => EntryKind::Word,
        WordEntryKindV3::Phrase => EntryKind::Phrase,
    }
}

pub(super) fn kind_string_owned(kind: EntryKind) -> String {
    match kind {
        EntryKind::Word => "word",
        EntryKind::Phrase => "phrase",
    }
    .to_owned()
}

pub(super) fn status_string_owned(status: AdminWordStatus) -> String {
    match status {
        AdminWordStatus::Draft => "draft",
        AdminWordStatus::Published => "published",
        AdminWordStatus::Archived => "archived",
    }
    .to_owned()
}

pub(super) fn max_reachable_step(completed: &[String]) -> WordCreationStep {
    if completed.iter().any(|step| step == "meanings") {
        WordCreationStep::Preview
    } else if completed.iter().any(|step| step == "forms") {
        WordCreationStep::Meanings
    } else {
        WordCreationStep::Forms
    }
}

pub(super) fn valid_level(value: &str) -> bool {
    matches!(value, "A1" | "A2" | "B1" | "B2" | "C1" | "C2")
}

pub(super) fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

pub(super) fn map_headword_error(error: HeadwordNormalizationError) -> LexiconServiceError {
    LexiconServiceError::InvalidField {
        field: "headword",
        message: headword_error_message(error),
    }
}

pub(super) fn map_surface_error(error: HeadwordNormalizationError) -> LexiconServiceError {
    LexiconServiceError::InvalidField {
        field: "surface",
        message: headword_error_message(error),
    }
}

fn headword_error_message(error: HeadwordNormalizationError) -> &'static str {
    match error {
        HeadwordNormalizationError::Empty => "headword is required",
        HeadwordNormalizationError::TooLong => "headword is too long",
        HeadwordNormalizationError::ControlCharacter => "headword contains control characters",
        HeadwordNormalizationError::UnsupportedCharacter => {
            "headword must contain only Latin letters, digits and - ' . & / , characters"
        }
        HeadwordNormalizationError::MissingLatinLetter => {
            "headword must contain at least one Latin letter"
        }
    }
}

pub(super) fn surface_projection_error(_error: HeadwordNormalizationError) -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
        "persisted surface normalization failed",
    ))
}

pub(super) fn repository_error(error: LexiconRepositoryError) -> LexiconServiceError {
    match error {
        LexiconRepositoryError::TargetPublicationBusy => LexiconServiceError::ReferenceConflict,
        LexiconRepositoryError::ReferenceTargetChanged => LexiconServiceError::ReferenceConflict,
        LexiconRepositoryError::SurfaceContextBusy => LexiconServiceError::ReferenceConflict,
        other => LexiconServiceError::Repository(other),
    }
}

pub(super) fn database_error(error: sqlx::Error) -> LexiconServiceError {
    repository_error(LexiconRepositoryError::Database(error))
}

pub(super) fn serialization_error(error: serde_json::Error) -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Serialization(error))
}

pub(super) fn invariant_record() -> LexiconServiceError {
    LexiconServiceError::Repository(LexiconRepositoryError::Invariant(
        "stored entry shape is invalid",
    ))
}

/// 草稿态写权限：**从未发布**的草稿只有创建者本人与超管能写。
///
/// 「草稿」只表示尚未对 C 端发布——它在 admin 内部对所有管理员照常可见（列表、详情），
/// 但可见不等于可写。已发布词条（含带未发布修订的）不走这条限制，全员可编辑。
///
/// 判定顺序与 `delete_entry_in_transaction` 一致：归属先于其他业务校验，避免把
/// 「这条处于什么状态」的信息泄露给无权处置它的管理员。
pub(super) fn ensure_draft_writable(
    record: &EntryRecord,
    actor_id: Uuid,
    is_super_admin: bool,
) -> Result<(), LexiconServiceError> {
    if record.current_publication_id.is_none()
        && !is_super_admin
        && record.created_by_admin_id != actor_id
    {
        return Err(LexiconServiceError::EntryEditForbidden);
    }
    Ok(())
}

/// 单词词条只能挂单词词性、短语词条只能挂短语词性；不一致的每个 pos 节点各报一条，
/// 锚在 forms 步的 pos 节点上。迁移前建的短语词条可能还挂着单词词性，这里是它们被拦下的地方。
pub(super) fn part_of_speech_kind_issues<'a>(
    entry_kind: &str,
    parts: &[crate::lexicon::model::CatalogPartRecord],
    pos: impl Iterator<Item = (Uuid, &'a str)>,
) -> Vec<DraftValidationIssue> {
    let message = if entry_kind == "phrase" {
        "该词性属于单词目录，短语词条请改选短语词性"
    } else {
        "该词性属于短语目录，单词词条请改选单词词性"
    };
    pos.filter(|(_, code)| {
        parts
            .iter()
            .any(|part| part.code == *code && part.kind != entry_kind)
    })
    .map(|(pos_id, _)| DraftValidationIssue {
        step: PersistedWordStep::Forms,
        node_id: pos_id,
        field: "pos".to_owned(),
        code: crate::lexicon::dto::V3ValidationIssueCode::PartOfSpeechKindMismatch
            .as_str()
            .to_owned(),
        message: message.to_owned(),
        reference_location: None,
        // 前端按 node_location.pos_id 把问题归到对应的词性位置，省掉就只能落进「无位置」分组。
        node_location: Some(crate::lexicon::dto::DraftNodeLocation {
            node_role: "forms.pos".to_owned(),
            pos: None,
            pos_id: Some(pos_id),
            form_group_index: None,
            form_group_id: None,
            membership_id: None,
            form_id: None,
            variant_id: None,
            pronunciation_id: None,
            form_type: None,
            dialect: None,
            ancestor_node_ids: vec![pos_id],
        }),
    })
    .collect()
}
