use super::*;

pub fn validate_meanings(
    entry_id: Uuid,
    forms: &DraftFormsStepContent,
    content: &DraftMeaningsStepContent,
    headwords: &WordHeadwordsV2,
    sub_part_parents: &HashMap<String, String>,
) -> Vec<DraftValidationIssue> {
    let mut issues = Vec::new();
    let mut node_types = HashMap::new();
    let sense_group_ids = content
        .sense_groups
        .iter()
        .map(|group| group.id)
        .collect::<HashSet<_>>();
    let form_pos = forms
        .pos
        .iter()
        .map(|pos| (pos.pos_id, pos.pos.as_str()))
        .collect::<HashMap<_, _>>();

    if content.sense_groups.is_empty() {
        issue(
            &mut issues,
            PersistedWordStep::Meanings,
            entry_id,
            "sense_groups",
            "sense_group_required",
            "至少需要一个语义区间",
        );
    }
    for group in &content.sense_groups {
        unique_node(
            &mut issues,
            &mut node_types,
            PersistedWordStep::Meanings,
            group.id,
            "sense_group",
        );
        for (field, name) in [("name_zh", &group.name_zh), ("name_en", &group.name_en)] {
            if name.trim().is_empty() {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    group.id,
                    field,
                    "sense_group_name_required",
                    "语义区间名称不能为空",
                );
            } else if name.chars().count() > 200 {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    group.id,
                    field,
                    "sense_group_name_too_long",
                    "语义区间名称不能超过 200 个字符",
                );
            }
        }
    }

    let mut seen_pos = HashSet::new();
    // 语法结构的方言形状：unified 词条只接受单条 common；distinguish 词条既接受历史的
    // uk + us 双条，也接受收敛后的单条 common（英美方言偏好化 A1 · 后端提案 P1）。
    let allowed_dialects: &[&[Dialect]] = if matches!(headwords, WordHeadwordsV2::Unified { .. }) {
        &[&[Dialect::Common]]
    } else {
        &[&[Dialect::Common], &[Dialect::Uk, Dialect::Us]]
    };
    for pos in &content.pos {
        let Some(pos_code) = form_pos.get(&pos.pos_id).copied() else {
            issue(
                &mut issues,
                PersistedWordStep::Meanings,
                pos.pos_id,
                "pos_id",
                "pos_not_found",
                "词义引用了不存在的基本词性",
            );
            continue;
        };
        if !seen_pos.insert(pos.pos_id) {
            issue(
                &mut issues,
                PersistedWordStep::Meanings,
                pos.pos_id,
                "pos_id",
                "duplicate_pos_meanings",
                "同一基本词性只能有一组词义数据",
            );
        }
        if pos.grammar_structures.is_empty() {
            issue(
                &mut issues,
                PersistedWordStep::Meanings,
                pos.pos_id,
                "grammar_structures",
                "grammar_required",
                "每个词性至少需要一条语法结构",
            );
        }
        let grammar_ids = pos
            .grammar_structures
            .iter()
            .map(|grammar| grammar.id)
            .collect::<HashSet<_>>();
        for grammar in &pos.grammar_structures {
            unique_node(
                &mut issues,
                &mut node_types,
                PersistedWordStep::Meanings,
                grammar.id,
                "grammar_structure",
            );
            let dialects = grammar
                .variants
                .iter()
                .map(|variant| variant.dialect)
                .collect::<HashSet<_>>();
            // 变体去重后必须恰好等于某一种被允许的方言集合，重复方言因此也被挡住。
            let shape_allowed = dialects.len() == grammar.variants.len()
                && allowed_dialects.iter().any(|allowed| {
                    allowed.len() == dialects.len()
                        && allowed.iter().all(|dialect| dialects.contains(dialect))
                });
            if !shape_allowed
                || grammar.variants.iter().any(|variant| {
                    !valid_rich_text(&variant.content) || variant.content.text().trim().is_empty()
                })
            {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    grammar.id,
                    "variants",
                    "grammar_variants_invalid",
                    "语法结构必须包含当前方言的完整文本",
                );
            }
            for variant in &grammar.variants {
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Meanings,
                    variant.id,
                    "text_variant",
                );
            }
        }
        if pos.senses.is_empty() {
            issue(
                &mut issues,
                PersistedWordStep::Meanings,
                pos.pos_id,
                "senses",
                "sense_required",
                "每个词性至少需要一个词义",
            );
        }
        for sense in &pos.senses {
            unique_node(
                &mut issues,
                &mut node_types,
                PersistedWordStep::Meanings,
                sense.id,
                "sense",
            );
            if !valid_level(&sense.level) {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "level",
                    "level_invalid",
                    "CEFR 等级无效",
                );
            }
            if sense.sub_pos.is_empty() {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "sub_pos",
                    "sub_pos_required",
                    "请选择细分词性",
                );
            } else if sub_part_parents
                .get(&sense.sub_pos)
                .is_none_or(|parent| parent != pos_code)
            {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "sub_pos",
                    "invalid_sub_part_of_speech",
                    "细分词性不属于当前基本词性",
                );
            }
            if sense
                .frequency
                .as_deref()
                .is_none_or(|value| !valid_percent(value))
            {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "frequency",
                    "frequency_invalid",
                    "词义词频必须是 0–100 且最多两位小数",
                );
            }
            if sense
                .sense_group_id
                .is_none_or(|group_id| !sense_group_ids.contains(&group_id))
            {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "sense_group_id",
                    "sense_group_not_found",
                    "请选择有效的语义区间",
                );
            }
            if sense.definitions.is_empty() {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "definitions",
                    "definition_required",
                    "每个词义至少需要一条释义",
                );
            }
            let mut has_chinese = false;
            for definition in &sense.definitions {
                match definition {
                    WordDefinitionV2::ZhDefinition { content_id, .. }
                    | WordDefinitionV2::ZhSentence { content_id, .. } => unique_node(
                        &mut issues,
                        &mut node_types,
                        PersistedWordStep::Meanings,
                        *content_id,
                        "text_variant",
                    ),
                    WordDefinitionV2::EnDefinition { content, .. }
                    | WordDefinitionV2::EnSentence { content, .. } => {
                        register_english_text_nodes(&mut issues, &mut node_types, content);
                    }
                }
                let (id, grammar_id, valid_content, chinese) = match definition {
                    WordDefinitionV2::ZhDefinition {
                        id,
                        grammar_structure_id,
                        content,
                        ..
                    }
                    | WordDefinitionV2::ZhSentence {
                        id,
                        grammar_structure_id,
                        content,
                        ..
                    } => (
                        *id,
                        *grammar_structure_id,
                        valid_rich_text(content) && !content.text().trim().is_empty(),
                        true,
                    ),
                    WordDefinitionV2::EnDefinition {
                        id,
                        grammar_structure_id,
                        content,
                        ..
                    }
                    | WordDefinitionV2::EnSentence {
                        id,
                        grammar_structure_id,
                        content,
                        ..
                    } => (
                        *id,
                        *grammar_structure_id,
                        valid_english_text(content),
                        false,
                    ),
                };
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Meanings,
                    id,
                    "definition",
                );
                has_chinese |= chinese && valid_content;
                if !valid_level(definition_level(definition)) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        id,
                        "level",
                        "definition_level_invalid",
                        "释义 CEFR 等级无效",
                    );
                }
                if !valid_content
                    || grammar_id.is_some_and(|grammar_id| !grammar_ids.contains(&grammar_id))
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        id,
                        "content",
                        "definition_invalid",
                        "释义文本或语法结构引用无效",
                    );
                }
            }
            if !has_chinese {
                issue(
                    &mut issues,
                    PersistedWordStep::Meanings,
                    sense.id,
                    "definitions",
                    "native_definition_required",
                    "至少填写一条中文释义",
                );
            }
            for sentence in &sense.sentences {
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Meanings,
                    sentence.id,
                    "sentence",
                );
                register_english_text_nodes(&mut issues, &mut node_types, &sentence.en_text);
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Meanings,
                    sentence.zh_text_id,
                    "text_variant",
                );
                if !valid_level(&sentence.level) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        sentence.id,
                        "level",
                        "sentence_level_invalid",
                        "例句 CEFR 等级无效",
                    );
                }
                let mut link_targets = HashSet::new();
                for link in &sentence.links {
                    if !matches!(link.role.as_str(), "focus" | "context") {
                        issue(
                            &mut issues,
                            PersistedWordStep::Meanings,
                            sentence.id,
                            "links",
                            "sentence_link_role_invalid",
                            "例句链接角色必须是 focus 或 context",
                        );
                    }
                    if !link_targets.insert((link.word_id, link.sense_id)) {
                        issue(
                            &mut issues,
                            PersistedWordStep::Meanings,
                            sentence.id,
                            "links",
                            "duplicate_sentence_link",
                            "同一例句不能重复链接同一词义",
                        );
                    }
                }
                let focus = sentence
                    .links
                    .iter()
                    .filter(|link| link.role == "focus")
                    .collect::<Vec<_>>();
                if !valid_english_text(&sentence.en_text)
                    || !valid_rich_text(&sentence.zh_text)
                    || sentence.zh_text.text().trim().is_empty()
                    || focus.len() != 1
                    || focus[0].word_id != entry_id
                    || focus[0].sense_id != sense.id
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        sentence.id,
                        "sentence",
                        "sentence_incomplete",
                        "例句需包含完整中英文和唯一当前词义焦点",
                    );
                }
            }
            for relation in &sense.relations {
                unique_node(
                    &mut issues,
                    &mut node_types,
                    PersistedWordStep::Meanings,
                    relation.id,
                    "relation",
                );
                if !valid_percent(&relation.score) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        relation.id,
                        "score",
                        "relation_score_invalid",
                        "关联度必须是 0–100 且最多两位小数",
                    );
                }
                if !matches!(
                    relation.relation.as_str(),
                    "synonym" | "antonym" | "derivative"
                ) {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        relation.id,
                        "relation",
                        "relation_type_invalid",
                        "关联词类型无效",
                    );
                }
                if relation.bound_target().is_some()
                    && (relation.prebound_target_word_id.is_some()
                        || relation.prebinding_state.is_some()
                        || relation.pending_target_headword.is_some()
                        || relation.pending_target_gloss.is_some())
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        relation.id,
                        if relation.pending_target_gloss.is_some() {
                            "pending_target_gloss"
                        } else {
                            "pending_target_headword"
                        },
                        "relation_target_shape_invalid",
                        "已绑定关联词不能再携带待建词面或预定义词义",
                    );
                }
                if relation.prebound_target_word_id.is_some()
                    && (relation.target_word_id.is_some()
                        || relation.target_sense_id.is_some()
                        || relation.pending_target_headword.is_some()
                        || !matches!(
                            relation.prebinding_state.as_deref(),
                            Some("waiting_first_sense" | "target_sense_deleted")
                        ))
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        relation.id,
                        "prebound_target_word_id",
                        "relation_target_shape_invalid",
                        "预绑定关联词不携带待建词面，且必须保留稳定目标与服务端状态",
                    );
                }
                if relation.prebound_target_word_id.is_none() && relation.prebinding_state.is_some()
                {
                    issue(
                        &mut issues,
                        PersistedWordStep::Meanings,
                        relation.id,
                        "prebound_target_word_id",
                        "relation_target_shape_invalid",
                        "预绑定状态缺少稳定目标词条",
                    );
                }
            }
        }
    }
    for pos_id in form_pos.keys() {
        if !seen_pos.contains(pos_id) {
            issue(
                &mut issues,
                PersistedWordStep::Meanings,
                *pos_id,
                "pos",
                "pos_meanings_required",
                "每个基本词性都需要词义数据",
            );
        }
    }
    issues
}
