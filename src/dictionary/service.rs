use super::dto::*;
use crate::{
    error::{AppError, ErrorCode},
    lexicon::{dto::*, normalization::normalize_headword, published},
    wordlists::{dto::*, service as reading},
};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub async fn search(pool: &PgPool, query: DictionaryQuery) -> Result<DictionaryPage, AppError> {
    let (page, size, q) = reading::pagination(&WordlistQuery {
        q: Some(query.q),
        page: query.page,
        page_size: query.page_size,
    })?;
    let normalized = normalize_headword(&q)
        .map_err(|_| AppError::validation(ErrorCode::InvalidQuery, "q", "请输入有效的英文词汇"))?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // One read snapshot keeps counts, matching and publication content consistent.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let from = r#"FROM lexicon.entries e
        JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id
        WHERE e.archived_at IS NULL AND p.content_schema_version=3 AND e.language='en'
          AND (strpos(lower(p.snapshot#>>'{presentation,label}'),$1)>0 OR EXISTS (
            SELECT 1 FROM lexicon.surface_sources s WHERE s.entry_id=e.id
              AND s.publication_id=p.id AND s.content_scope='current_publication'
              AND NOT s.is_deleted AND strpos(s.normalized_surface,$1)>0))"#;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) {from}")))
        .bind(&normalized.key)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let selected: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT e.id {from} ORDER BY lower(p.snapshot#>>'{{presentation,label}}') COLLATE "C", e.id LIMIT $2 OFFSET $3"#)))
        .bind(&normalized.key).bind(i64::from(size)).bind(i64::from(page-1)*i64::from(size))
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let items = entries(&mut tx, &selected).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(DictionaryPage {
        items,
        pagination: reading::page_meta(page, size, total),
    })
}

pub async fn entries(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<DictionaryEntry>, AppError> {
    let mut projected = reading::read_entries_view(tx, ids, WordlistView::Full).await?;
    let mut publications = published::read_current_in(tx, ids).await?;
    let mut result = Vec::new();
    for id in ids {
        let (Some(content), Some(publication)) = (projected.remove(id), publications.remove(id))
        else {
            continue;
        };
        let native = publication.content_v3()?;
        let mut audio = Vec::new();
        for pos in &native.forms.pos {
            for form in &pos.forms {
                let variants = match &form.regional_variants {
                    WordRegionalVariantsV3::Common { common } => {
                        vec![(
                            common.id,
                            Dialect::Common,
                            &common.spelling_speech,
                            &common.pronunciations,
                        )]
                    }
                    WordRegionalVariantsV3::UkUs { uk, us } => vec![
                        (uk.id, Dialect::Uk, &uk.spelling_speech, &uk.pronunciations),
                        (us.id, Dialect::Us, &us.spelling_speech, &us.pronunciations),
                    ],
                };
                for (variant_id, source_dialect, spelling_speech, pronunciations) in variants {
                    if let Some(speech) = spelling_speech {
                        generated_audio(
                            &mut audio,
                            variant_id,
                            source_dialect,
                            Some(&speech.voice_profile),
                        );
                    }
                    for pronunciation in pronunciations {
                        generated_audio(
                            &mut audio,
                            pronunciation.id,
                            source_dialect,
                            pronunciation.voice_profile.as_ref(),
                        );
                        if let Some(synthesis) = &pronunciation.synthesis {
                            if synthesis.use_spelling == Some(true) {
                                if let Some(speech) = spelling_speech {
                                    generated_audio(
                                        &mut audio,
                                        pronunciation.id,
                                        source_dialect,
                                        Some(&speech.voice_profile),
                                    );
                                }
                            } else {
                                for (phoneme_dialect, candidate) in
                                    [(Dialect::Uk, &synthesis.uk), (Dialect::Us, &synthesis.us)]
                                        .into_iter()
                                        .filter_map(|(dialect, value)| {
                                            value.as_ref().map(|value| (dialect, value))
                                        })
                                {
                                    let speech = match synthesis.alphabet {
                                        RichTextPhonemeAlphabet::Ipa => &candidate.ipa_speech,
                                        RichTextPhonemeAlphabet::Ups => &candidate.ups_speech,
                                    };
                                    if let Some(speech) = speech {
                                        generated_audio(
                                            &mut audio,
                                            pronunciation.id,
                                            phoneme_dialect,
                                            Some(&speech.voice_profile),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for pos in &native.forms.pos {
            for form in &pos.forms {
                let variants = match &form.regional_variants {
                    WordRegionalVariantsV3::Common { common } => {
                        vec![(Dialect::Common, &common.pronunciations)]
                    }
                    WordRegionalVariantsV3::UkUs { uk, us } => {
                        vec![
                            (Dialect::Uk, &uk.pronunciations),
                            (Dialect::Us, &us.pronunciations),
                        ]
                    }
                };
                for (source_dialect, variant) in variants {
                    for sound in variant {
                        audio.extend(sound.audio_assets.iter().map(|a| DictionaryAudio {
                            target_id: sound.id,
                            asset_id: a.id,
                            locale: a.locale.as_str().into(),
                            accent_group: a.locale.accent_group(),
                            source_dialect,
                            hd_flash: false,
                        }));
                    }
                }
            }
        }
        let mut examples = Vec::new();
        for pos in &native.meanings.pos {
            for grammar in &pos.grammar_structures {
                for variant in &grammar.variants {
                    generated_audio(
                        &mut audio,
                        grammar.id,
                        variant.dialect,
                        variant.voice_profile.as_ref(),
                    );
                    audio.extend(variant.audio_assets.iter().map(|a| DictionaryAudio {
                        target_id: grammar.id,
                        asset_id: a.id,
                        locale: a.locale.as_str().into(),
                        accent_group: a.locale.accent_group(),
                        source_dialect: variant.dialect,
                        hd_flash: false,
                    }));
                }
            }
            for sense in &pos.senses {
                for definition in &sense.definitions {
                    if let WordDefinitionV3::EnDefinition { id, content, .. }
                    | WordDefinitionV3::EnSentence { id, content, .. } = definition
                    {
                        for (source_dialect, variant) in sourced_english_variants(content) {
                            generated_audio(
                                &mut audio,
                                *id,
                                source_dialect,
                                variant.voice_profile.as_ref(),
                            );
                        }
                    }
                }
                for sentence in &sense.sentences {
                    for (source_dialect, variant) in sourced_english_variants(&sentence.en_text) {
                        generated_audio(
                            &mut audio,
                            sentence.id,
                            source_dialect,
                            variant.voice_profile.as_ref(),
                        );
                    }
                    examples.push(DictionaryExample {
                        id: sentence.id,
                        sense_id: sense.id,
                        texts: english(&sentence.en_text),
                        translation: sentence.zh_text.clone(),
                    });
                }
            }
        }
        // Shared examples have an independent publication/withdrawal boundary.
        // Only current published associations to a sense in this publication are readable.
        let shared: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
            r#"SELECT DISTINCT a.target_sense_id, p.snapshot
            FROM lexicon.shared_sentence_publication_annotations a
            JOIN lexicon.shared_sentences s ON s.current_publication_id=a.publication_id
            JOIN lexicon.shared_sentence_publications p ON p.id=s.current_publication_id
            WHERE a.target_entry_id=$1 AND a.target_sense_id IS NOT NULL
              AND s.deleted_at IS NULL AND s.withdrawn_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM lexicon.shared_sentence_publication_hides h
                WHERE h.publication_id=$2 AND h.sense_id=a.target_sense_id AND h.sentence_id=s.id)
            ORDER BY a.target_sense_id, p.snapshot"#,
        )
        .bind(id)
        .bind(publication.publication_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::internal)?;
        for (sense_id, snapshot) in shared {
            if !content
                .pos
                .iter()
                .any(|pos| pos.senses.iter().any(|sense| sense.id == sense_id))
            {
                continue;
            }
            let shared: crate::lexicon::shared_sentences::SharedSentenceContent =
                serde_json::from_value(snapshot).map_err(AppError::internal)?;
            let sentence = shared.sentence;
            for (source_dialect, variant) in sourced_english_variants(&sentence.en_text) {
                generated_audio(
                    &mut audio,
                    sentence.id,
                    source_dialect,
                    variant.voice_profile.as_ref(),
                );
            }
            if examples
                .iter()
                .any(|e| e.id == sentence.id && e.sense_id == sense_id)
            {
                continue;
            }
            examples.push(DictionaryExample {
                id: sentence.id,
                sense_id,
                texts: english(&sentence.en_text),
                translation: sentence.zh_text,
            });
        }
        result.push(DictionaryEntry {
            content,
            audio,
            examples,
        });
    }
    Ok(result)
}

fn sourced_english_variants(
    content: &EnglishTextV3,
) -> Vec<(Dialect, &crate::lexicon::dto::RichTextVariantV3)> {
    match content {
        EnglishTextV3::Unified { common } => vec![(Dialect::Common, common)],
        EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => Some((dialect, variant)),
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}

fn generated_audio(
    audio: &mut Vec<DictionaryAudio>,
    target_id: Uuid,
    source_dialect: Dialect,
    profile: Option<&VoiceProfileV3>,
) {
    for setting in profile
        .into_iter()
        .flat_map(|profile| &profile.voices)
        .filter(|setting| setting.enabled)
    {
        if let (Some(asset_id), Some(locale)) = (setting.audio_asset_id, setting.locale) {
            audio.push(DictionaryAudio {
                target_id,
                asset_id,
                locale: locale.as_str().into(),
                accent_group: locale.accent_group(),
                source_dialect,
                hd_flash: setting.voice_id.starts_with("azure-hd-flash-"),
            });
        }
    }
}

fn english(content: &EnglishTextV3) -> Vec<WordlistText> {
    match content {
        EnglishTextV3::Unified { common } => vec![WordlistText {
            dialect: Dialect::Common,
            content: common.value.clone(),
        }],
        EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => Some(WordlistText {
                    dialect,
                    content: variant.value.clone(),
                }),
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}
