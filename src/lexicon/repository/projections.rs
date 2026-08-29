use super::*;

// --- form projection ---

pub(super) async fn insert_headwords(
    tx: &mut Transaction<'_, Postgres>,
    word: &AdminWordV2,
) -> Result<(), LexiconRepositoryError> {
    let values: Vec<(Dialect, &str)> = match &word.headwords {
        WordHeadwordsV2::Unified { common } => vec![(Dialect::Common, common)],
        WordHeadwordsV2::Distinguish { uk, us, .. } => {
            vec![(Dialect::Uk, uk), (Dialect::Us, us)]
        }
    };

    for (dialect, value) in &values {
        let origin = if word.detection_snapshot.builtin_dictionary_status == "matched" {
            headword_origin(
                &word.detection_snapshot.headwords,
                word.detection_snapshot.matched_dialect,
                &word.headwords,
                *dialect,
                value,
            )
        } else {
            TextOrigin::Manual
        };
        let normalized = normalize_headword(value)
            .map_err(|_| LexiconRepositoryError::Invariant("headword was not normalized"))?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_headwords (
                id, entry_id, dialect, headword, normalized_headword,
                normalization_version, origin
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(word.id)
        .bind(dialect_string(*dialect))
        .bind(&normalized.display)
        .bind(&normalized.key)
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .bind(origin_string(origin))
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;
    }

    let keys: Vec<(Dialect, String)> = match &word.headwords {
        WordHeadwordsV2::Unified { common } => {
            let key = normalize_headword(common)
                .map_err(|_| LexiconRepositoryError::Invariant("headword was not normalized"))?
                .key;
            vec![(Dialect::Uk, key.clone()), (Dialect::Us, key)]
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => vec![
            (
                Dialect::Uk,
                normalize_headword(uk)
                    .map_err(|_| LexiconRepositoryError::Invariant("headword was not normalized"))?
                    .key,
            ),
            (
                Dialect::Us,
                normalize_headword(us)
                    .map_err(|_| LexiconRepositoryError::Invariant("headword was not normalized"))?
                    .key,
            ),
        ],
    };

    for (dialect, normalized) in keys {
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_headword_keys (
                entry_id, language, kind, dialect_scope, normalized_headword, normalization_version
            ) VALUES ($1, 'en', $2, $3, $4, $5)
            "#,
        )
        .bind(word.id)
        .bind(kind_string(word.kind))
        .bind(dialect_string(dialect))
        .bind(normalized)
        .bind(HEADWORD_NORMALIZATION_VERSION)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;
    }
    Ok(())
}

pub(super) async fn insert_forms(
    tx: &mut Transaction<'_, Postgres>,
    word: &AdminWordV2,
    catalog_parts: &HashMap<String, Uuid>,
) -> Result<(), LexiconRepositoryError> {
    for (pos_index, pos) in word.forms.pos.iter().enumerate() {
        let part_id =
            catalog_parts
                .get(&pos.pos)
                .copied()
                .ok_or(LexiconRepositoryError::Invariant(
                    "part of speech disappeared",
                ))?;
        insert_node(tx, pos.pos_id, word.id, "pos", None, POS_ROLE, false).await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_pos (
                id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode, sort_order
            ) VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(pos.pos_id)
        .bind(word.id)
        .bind(part_id)
        .bind(&pos.dialect_rules.spelling_mode)
        .bind(&pos.dialect_rules.phonetic_mode)
        .bind(pos_index as i32)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;

        insert_node(
            tx,
            pos.base_form.id,
            word.id,
            "form_slot",
            Some(pos.pos_id),
            BASE_FORM_ROLE,
            true,
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.form_slots (
                id, entry_id, entry_pos_id, form_group_id, form_type, sort_order
            ) VALUES ($1, $2, $3, NULL, 'base', 0)
            "#,
        )
        .bind(pos.base_form.id)
        .bind(word.id)
        .bind(pos.pos_id)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;
        insert_form_variants(tx, word.id, pos.base_form.id, &pos.base_form.variants).await?;

        for (group_index, group) in pos.form_groups.iter().enumerate() {
            insert_node(
                tx,
                group.id,
                word.id,
                "form_group",
                Some(pos.pos_id),
                FORM_GROUP_ROLE,
                false,
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.form_groups (
                    id, entry_id, entry_pos_id, is_regular, sort_order
                ) VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(group.id)
            .bind(word.id)
            .bind(pos.pos_id)
            .bind(group.is_regular)
            .bind(group_index as i32)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;

            for (slot_index, slot) in group.slots.iter().enumerate() {
                let node_role = form_slot_role(&slot.form_type);
                insert_node(
                    tx,
                    slot.id,
                    word.id,
                    "form_slot",
                    Some(group.id),
                    &node_role,
                    true,
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.form_slots (
                        id, entry_id, entry_pos_id, form_group_id, form_type, sort_order
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    "#,
                )
                .bind(slot.id)
                .bind(word.id)
                .bind(pos.pos_id)
                .bind(group.id)
                .bind(&slot.form_type)
                .bind(slot_index as i32)
                .execute(&mut **tx)
                .await
                .map_err(map_entry_write_error)?;
                insert_form_variants(tx, word.id, slot.id, &slot.variants).await?;
            }
        }
    }
    Ok(())
}

pub(super) async fn insert_form_variants(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    slot_id: Uuid,
    variants: &[crate::lexicon::dto::WordFormVariantV2],
) -> Result<(), LexiconRepositoryError> {
    for (variant_index, variant) in variants.iter().enumerate() {
        let node_role = form_variant_role(variant.dialect);
        insert_node(
            tx,
            variant.id,
            entry_id,
            "form_variant",
            Some(slot_id),
            &node_role,
            true,
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.form_variants (
                id, entry_id, form_slot_id, dialect, spelling, origin, sort_order
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(variant.id)
        .bind(entry_id)
        .bind(slot_id)
        .bind(dialect_string(variant.dialect))
        .bind(&variant.spelling)
        .bind(origin_string(variant.origin))
        .bind(variant_index as i32)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;

        for (pronunciation_index, pronunciation) in variant.pronunciations.iter().enumerate() {
            insert_node(
                tx,
                pronunciation.id,
                entry_id,
                "pronunciation",
                Some(variant.id),
                PRONUNCIATION_ROLE,
                false,
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.pronunciations (
                    id, entry_id, form_variant_id, dict_phonetic, actual_pron, style, sort_order
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(pronunciation.id)
            .bind(entry_id)
            .bind(variant.id)
            .bind(&pronunciation.dict_phonetic)
            .bind(&pronunciation.actual_pron)
            .bind(match pronunciation.style {
                crate::lexicon::dto::PronunciationStyle::Normal => "normal",
                crate::lexicon::dto::PronunciationStyle::Strong => "strong",
                crate::lexicon::dto::PronunciationStyle::Weak => "weak",
            })
            .bind(pronunciation_index as i32)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;
        }
    }
    Ok(())
}

// --- meaning projection ---

pub(super) async fn insert_meanings(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    meanings: &DraftMeaningsStepContent,
    sub_parts: &HashMap<String, Uuid>,
) -> Result<(), LexiconRepositoryError> {
    for (index, group) in meanings.sense_groups.iter().enumerate() {
        insert_node(
            tx,
            group.id,
            entry_id,
            "sense_group",
            None,
            SENSE_GROUP_ROLE,
            false,
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO lexicon.sense_groups (id, entry_id, name_zh, name_en, sort_order)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(group.id)
        .bind(entry_id)
        .bind(&group.name_zh)
        .bind(&group.name_en)
        .bind(index as i32)
        .execute(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;
    }

    for pos_meanings in &meanings.pos {
        for (grammar_index, grammar) in pos_meanings.grammar_structures.iter().enumerate() {
            insert_node(
                tx,
                grammar.id,
                entry_id,
                "grammar_structure",
                Some(pos_meanings.pos_id),
                GRAMMAR_STRUCTURE_ROLE,
                false,
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.grammar_structures (id, entry_id, entry_pos_id, sort_order)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(grammar.id)
            .bind(entry_id)
            .bind(pos_meanings.pos_id)
            .bind(grammar_index as i32)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;
            for (variant_index, variant) in grammar.variants.iter().enumerate() {
                insert_text_variant(
                    tx,
                    variant.id,
                    entry_id,
                    grammar.id,
                    "content",
                    "en",
                    variant.dialect,
                    &variant.content,
                    TextOrigin::Manual,
                    variant_index as i32,
                )
                .await?;
            }
        }

        for (sense_index, sense) in pos_meanings.senses.iter().enumerate() {
            let sub_part_id = if sense.sub_pos.is_empty() {
                None
            } else {
                Some(sub_parts.get(&sense.sub_pos).copied().ok_or(
                    LexiconRepositoryError::Invariant("sub part of speech disappeared"),
                )?)
            };
            insert_node(
                tx,
                sense.id,
                entry_id,
                "sense",
                Some(pos_meanings.pos_id),
                SENSE_ROLE,
                false,
            )
            .await?;
            sqlx::query(
                r#"
                INSERT INTO lexicon.senses (
                    id, entry_id, entry_pos_id, sub_part_of_speech_id, sense_group_id,
                    level, frequency, depends_on_context, sort_order
                ) VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8, $9)
                "#,
            )
            .bind(sense.id)
            .bind(entry_id)
            .bind(pos_meanings.pos_id)
            .bind(sub_part_id)
            .bind(sense.sense_group_id)
            .bind(&sense.level)
            .bind(sense.frequency.as_deref())
            .bind(sense.depends_on_context)
            .bind(sense_index as i32)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;

            for (definition_index, definition) in sense.definitions.iter().enumerate() {
                insert_definition(tx, entry_id, sense.id, definition, definition_index as i32)
                    .await?;
            }
            for (sentence_index, sentence) in sense.sentences.iter().enumerate() {
                insert_node(
                    tx,
                    sentence.id,
                    entry_id,
                    "sentence",
                    Some(sense.id),
                    SENTENCE_ROLE,
                    false,
                )
                .await?;
                sqlx::query(
                    "INSERT INTO lexicon.sentences (id, entry_id, sense_id, level, sort_order) VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(sentence.id)
                .bind(entry_id)
                .bind(sense.id)
                .bind(&sentence.level)
                .bind(sentence_index as i32)
                .execute(&mut **tx)
                .await
                .map_err(map_entry_write_error)?;
                insert_english_text(tx, entry_id, sentence.id, "en_text", &sentence.en_text)
                    .await?;
                insert_text_variant(
                    tx,
                    sentence.zh_text_id,
                    entry_id,
                    sentence.id,
                    "zh_text",
                    "zh",
                    Dialect::Common,
                    &sentence.zh_text,
                    TextOrigin::Manual,
                    0,
                )
                .await?;
                for (link_index, link) in sentence.links.iter().enumerate() {
                    sqlx::query(
                        r#"
                        INSERT INTO lexicon.sentence_links (
                            sentence_id, entry_id, target_entry_id, target_sense_id, role, sort_order
                        ) VALUES ($1, $2, $3, $4, $5, $6)
                        "#,
                    )
                    .bind(sentence.id)
                    .bind(entry_id)
                    .bind(link.word_id)
                    .bind(link.sense_id)
                    .bind(&link.role)
                    .bind(link_index as i32)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_entry_write_error)?;
                }
            }
            for (relation_index, relation) in sense.relations.iter().enumerate() {
                insert_node(
                    tx,
                    relation.id,
                    entry_id,
                    "relation",
                    Some(sense.id),
                    RELATION_ROLE,
                    false,
                )
                .await?;
                sqlx::query(
                    r#"
                    INSERT INTO lexicon.relations (
                        id, entry_id, source_sense_id, relation_type,
                        target_entry_id, target_sense_id, score,
                        target_headword_snapshot, target_gloss_snapshot,
                        pending_target_headword, pending_target_gloss, sort_order
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7::numeric, $8, $9, $10, $11, $12)
                    "#,
                )
                .bind(relation.id)
                .bind(entry_id)
                .bind(sense.id)
                .bind(&relation.relation)
                .bind(relation.target_word_id)
                .bind(relation.target_sense_id)
                .bind(&relation.score)
                // 待物化的关联词没有目标义项可快照，必须落 NULL 而不是空串——
                // lexicon_relations_target_shape_check 要求两组字段严格互斥。
                .bind(
                    relation.target_word_id.and(
                        relation
                            .target_headword
                            .clone()
                            .or_else(|| Some(String::new())),
                    ),
                )
                .bind(
                    relation.target_word_id.and(
                        relation
                            .target_gloss
                            .clone()
                            .or_else(|| Some(String::new())),
                    ),
                )
                .bind(relation.pending_target_headword.as_deref())
                .bind(relation.pending_target_gloss.as_deref())
                .bind(relation_index as i32)
                .execute(&mut **tx)
                .await
                .map_err(map_entry_write_error)?;
            }
        }
    }
    Ok(())
}

pub(super) async fn insert_definition(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    sense_id: Uuid,
    definition: &WordDefinitionV2,
    sort_order: i32,
) -> Result<(), LexiconRepositoryError> {
    let (id, level, grammar_id, kind, language) = match definition {
        WordDefinitionV2::ZhDefinition {
            id,
            level,
            grammar_structure_id,
            ..
        } => (*id, level, *grammar_structure_id, "definition", "zh"),
        WordDefinitionV2::ZhSentence {
            id,
            level,
            grammar_structure_id,
            ..
        } => (*id, level, *grammar_structure_id, "sentence", "zh"),
        WordDefinitionV2::EnDefinition {
            id,
            level,
            grammar_structure_id,
            ..
        } => (*id, level, *grammar_structure_id, "definition", "en"),
        WordDefinitionV2::EnSentence {
            id,
            level,
            grammar_structure_id,
            ..
        } => (*id, level, *grammar_structure_id, "sentence", "en"),
    };
    insert_node(
        tx,
        id,
        entry_id,
        "definition",
        Some(sense_id),
        definition_role(definition),
        false,
    )
    .await?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.definitions (
            id, entry_id, sense_id, level, definition_kind, language,
            grammar_structure_id, sort_order
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(sense_id)
    .bind(level)
    .bind(kind)
    .bind(language)
    .bind(grammar_id)
    .bind(sort_order)
    .execute(&mut **tx)
    .await
    .map_err(map_entry_write_error)?;

    match definition {
        WordDefinitionV2::ZhDefinition {
            content_id,
            content,
            ..
        }
        | WordDefinitionV2::ZhSentence {
            content_id,
            content,
            ..
        } => {
            insert_text_variant(
                tx,
                *content_id,
                entry_id,
                id,
                "content",
                "zh",
                Dialect::Common,
                content,
                TextOrigin::Manual,
                0,
            )
            .await?;
        }
        WordDefinitionV2::EnDefinition { content, .. }
        | WordDefinitionV2::EnSentence { content, .. } => {
            insert_english_text(tx, entry_id, id, "content", content).await?;
        }
    }
    Ok(())
}

pub(super) async fn insert_english_text(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    owner_id: Uuid,
    field_role: &str,
    content: &EnglishTextV2,
) -> Result<(), LexiconRepositoryError> {
    match content {
        EnglishTextV2::Unified { common } => {
            insert_text_variant(
                tx,
                common.id,
                entry_id,
                owner_id,
                field_role,
                "en",
                Dialect::Common,
                &common.value,
                common.origin,
                0,
            )
            .await?;
        }
        EnglishTextV2::Distinguish { uk, us, .. } => {
            for (index, (dialect, slot)) in [(Dialect::Uk, uk), (Dialect::Us, us)]
                .into_iter()
                .enumerate()
            {
                if let DialectVariantSlotV2::Ready { variant } = slot {
                    insert_text_variant(
                        tx,
                        variant.id,
                        entry_id,
                        owner_id,
                        field_role,
                        "en",
                        dialect,
                        &variant.value,
                        variant.origin,
                        index as i32,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_text_variant(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    owner_id: Uuid,
    field_role: &str,
    language: &str,
    dialect: Dialect,
    content: &RichText,
    origin: TextOrigin,
    sort_order: i32,
) -> Result<(), LexiconRepositoryError> {
    let node_role = text_variant_role(field_role, language, dialect);
    insert_node(
        tx,
        id,
        entry_id,
        "text_variant",
        Some(owner_id),
        &node_role,
        true,
    )
    .await?;
    let content_json = serde_json::to_value(content)?;
    let content_hash = sha256_json(content)?;
    sqlx::query(
        r#"
        INSERT INTO lexicon.text_variants (
            id, entry_id, owner_node_id, field_role, language, dialect,
            rich_text_version, content, plain_text, content_hash, origin, sort_order
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(owner_id)
    .bind(field_role)
    .bind(language)
    .bind(dialect_string(dialect))
    .bind(content.version() as i16)
    .bind(content_json)
    .bind(content.text())
    .bind(content_hash)
    .bind(origin_string(origin))
    .bind(sort_order)
    .execute(&mut **tx)
    .await
    .map_err(map_entry_write_error)?;
    Ok(())
}
