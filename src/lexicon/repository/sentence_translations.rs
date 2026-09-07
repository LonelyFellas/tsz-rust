use super::*;

impl LexiconRepository {
    pub(crate) async fn prepare_v3_sentence_translation_aliases(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        meanings: &crate::lexicon::dto::DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconRepositoryError> {
        for sentence in meanings
            .pos
            .iter()
            .flat_map(|pos| &pos.senses)
            .flat_map(|sense| &sense.sentences)
        {
            sqlx::query(
                r#"
                UPDATE lexicon.nodes
                SET node_role = 'meanings.zh_text:zh:common', stable_slot = TRUE
                WHERE id = $1
                  AND entry_id = $2
                  AND node_type = 'text_variant'
                  AND parent_node_id = $3
                "#,
            )
            .bind(sentence.zh_text_id)
            .bind(entry_id)
            .bind(sentence.id)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;
        }
        Ok(())
    }

    pub(crate) async fn replace_v3_sentence_translations(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        meanings: &crate::lexicon::dto::DraftMeaningsStepContentV3,
    ) -> Result<(), LexiconRepositoryError> {
        for pos in &meanings.pos {
            for sense in &pos.senses {
                for sentence in &sense.sentences {
                    let (primary_index, primary) = sentence
                        .zh_translations
                        .iter()
                        .enumerate()
                        .find(|(_, translation)| translation.id == sentence.zh_text_id)
                        .ok_or(LexiconRepositoryError::Invariant(
                            "V3 sentence translation alias is missing from canonical list",
                        ))?;
                    let updated = sqlx::query(
                        r#"
                        UPDATE lexicon.text_variants
                        SET field_role = $3,
                            sort_order = $4
                        WHERE id = $1
                          AND entry_id = $2
                          AND owner_node_id = $5
                          AND field_role = 'zh_text'
                        "#,
                    )
                    .bind(primary.id)
                    .bind(entry_id)
                    .bind(primary.band.field_role())
                    .bind(primary_index as i32)
                    .bind(sentence.id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_entry_write_error)?;
                    if updated.rows_affected() != 1 {
                        return Err(LexiconRepositoryError::Invariant(
                            "V3 sentence primary translation row was not rebuilt",
                        ));
                    }
                    sqlx::query(
                        r#"
                        UPDATE lexicon.nodes
                        SET node_role = $3, stable_slot = FALSE
                        WHERE id = $1
                          AND entry_id = $2
                          AND node_type = 'text_variant'
                          AND parent_node_id = $4
                        "#,
                    )
                    .bind(primary.id)
                    .bind(entry_id)
                    .bind(crate::lexicon::node_identity::SENTENCE_TRANSLATION_ROLE)
                    .bind(sentence.id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_entry_write_error)?;

                    for (index, translation) in sentence.zh_translations.iter().enumerate() {
                        if translation.id == primary.id {
                            continue;
                        }
                        let content: RichText =
                            serde_json::from_value(serde_json::to_value(&translation.content)?)?;
                        insert_text_variant(
                            tx,
                            translation.id,
                            entry_id,
                            sentence.id,
                            translation.band.field_role(),
                            "zh",
                            Dialect::Common,
                            &content,
                            TextOrigin::Manual,
                            index as i32,
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }
}
