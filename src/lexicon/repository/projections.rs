use super::*;

// --- form projection ---

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
                        prebound_target_entry_id, prebinding_reason,
                        pending_target_headword, pending_target_gloss, sort_order
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7::numeric, $8, $9,
                        $10, $11, $12, $13, $14
                    )
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
                .bind(relation.prebound_target_word_id)
                .bind(relation.prebinding_state.as_deref())
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
        !field_role.starts_with("zh_translation_"),
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
