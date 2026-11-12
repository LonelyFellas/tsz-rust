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
    let normalized = normalize_headword(&q).map_err(|_| {
        AppError::validation(ErrorCode::InvalidQuery, "q", "请输入有效的英文词条名称")
    })?;
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
        pagination: crate::api::page_meta(page, size, total),
    })
}

pub async fn entries(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<DictionaryEntry>, AppError> {
    let mut projected = reading::read_entries_view(tx, ids, WordlistView::Full).await?;
    let mut publications = published::read_current_in(tx, ids).await?;
    let mut result = Vec::new();
    let mut audio_catalog = None;
    for id in ids {
        let (Some(content), Some(publication)) = (projected.remove(id), publications.remove(id))
        else {
            continue;
        };
        let native = publication.content_v3()?;
        let mut audio = own_audio(&native);
        let mut examples = Vec::new();
        for pos in &native.meanings.pos {
            for sense in &pos.senses {
                for sentence in &sense.sentences {
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
                uploaded_audio(
                    &mut audio,
                    sentence.id,
                    source_dialect,
                    &variant.audio_assets,
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
        let mut seen_audio = std::collections::HashSet::new();
        audio.retain(|item| {
            seen_audio.insert((
                item.target_id,
                item.asset_id,
                item.locale.clone(),
                std::mem::discriminant(&item.source_dialect),
            ))
        });
        let audio = playable_audio_with_catalog(tx, audio, &mut audio_catalog).await?;
        result.push(DictionaryEntry {
            content,
            audio,
            examples,
        });
    }
    Ok(result)
}

// The immutable publication is the authority for selectable owned audio. Sharing
// this collector with the reader keeps signing independent of display projection.
pub(crate) fn own_audio(native: &published::PublishedContentV3) -> Vec<DictionaryAudio> {
    let mut audio = Vec::new();
    for pos in &native.forms.pos {
        for form in &pos.forms {
            let variants = form
                .regional_variants
                .variants()
                .into_iter()
                .map(|v| (v.id, v.dialect, v.spelling_speech, v.pronunciations))
                .collect::<Vec<_>>();
            for (variant_id, source_dialect, spelling_speech, pronunciations) in variants {
                let explicit_sources = pronunciations.iter().any(|pronunciation| {
                    pronunciation.synthesis.as_ref().is_some_and(|synthesis| {
                        [&synthesis.uk, &synthesis.us].iter().any(|candidate| {
                            candidate
                                .as_ref()
                                .is_some_and(|candidate| candidate.source.is_some())
                        })
                    })
                });
                if !explicit_sources && let Some(speech) = spelling_speech {
                    generated_audio(
                        &mut audio,
                        variant_id,
                        source_dialect,
                        Some(&speech.voice_profile),
                    );
                    uploaded_audio(&mut audio, variant_id, source_dialect, &speech.audio_assets);
                }
                for pronunciation in pronunciations {
                    if let Some(synthesis) = &pronunciation.synthesis
                        && [&synthesis.uk, &synthesis.us].iter().any(|candidate| {
                            candidate
                                .as_ref()
                                .is_some_and(|candidate| candidate.source.is_some())
                        })
                    {
                        use crate::lexicon::dto::{
                            PhonemeLocaleV3, PronunciationSpeechSourceV3 as Source,
                        };
                        for (side, locale, accent) in [
                            (
                                Dialect::Uk,
                                PhonemeLocaleV3::EnGb,
                                crate::speech::locale::AccentGroup::Uk,
                            ),
                            (
                                Dialect::Us,
                                PhonemeLocaleV3::EnUs,
                                crate::speech::locale::AccentGroup::Us,
                            ),
                        ] {
                            if source_dialect != Dialect::Common && source_dialect != side {
                                continue;
                            }
                            let selected = synthesis.for_locale(locale, source_dialect);
                            let source = synthesis.source_for(locale).unwrap_or(
                                if selected.use_spelling == Some(true) {
                                    Source::Spelling
                                } else if selected.alphabet == RichTextPhonemeAlphabet::Ipa {
                                    Source::Ipa
                                } else {
                                    Source::Ups
                                },
                            );
                            let start = audio.len();
                            match source {
                                Source::None => continue,
                                Source::Upload => uploaded_audio(
                                    &mut audio,
                                    pronunciation.id,
                                    side,
                                    &pronunciation.audio_assets,
                                ),
                                Source::Spelling => {
                                    if let Some(speech) = spelling_speech {
                                        generated_audio(
                                            &mut audio,
                                            pronunciation.id,
                                            side,
                                            Some(&speech.voice_profile),
                                        );
                                    }
                                }
                                Source::Ipa | Source::Ups => {
                                    let candidate = if side == Dialect::Uk {
                                        &synthesis.uk
                                    } else {
                                        &synthesis.us
                                    };
                                    if let Some(candidate) = candidate {
                                        let speech = if source == Source::Ipa {
                                            &candidate.ipa_speech
                                        } else {
                                            &candidate.ups_speech
                                        };
                                        if let Some(speech) = speech {
                                            generated_audio(
                                                &mut audio,
                                                pronunciation.id,
                                                side,
                                                Some(&speech.voice_profile),
                                            );
                                        }
                                    }
                                }
                            }
                            let selected = audio
                                .drain(start..)
                                .filter(|item| item.accent_group == accent)
                                .collect::<Vec<_>>();
                            audio.extend(selected);
                        }
                        continue;
                    }
                    if pronunciation.synthesis.is_none() {
                        generated_audio(
                            &mut audio,
                            pronunciation.id,
                            source_dialect,
                            pronunciation.voice_profile.as_ref(),
                        );
                    }
                    if let Some(synthesis) = &pronunciation.synthesis {
                        if synthesis.use_spelling == Some(true) {
                            if let Some(speech) = spelling_speech {
                                generated_audio(
                                    &mut audio,
                                    pronunciation.id,
                                    source_dialect,
                                    Some(&speech.voice_profile),
                                );
                                uploaded_audio(
                                    &mut audio,
                                    pronunciation.id,
                                    source_dialect,
                                    &speech.audio_assets,
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
                    uploaded_audio(
                        &mut audio,
                        pronunciation.id,
                        source_dialect,
                        &pronunciation.audio_assets,
                    );
                    if pronunciation.synthesis.is_some() {
                        generated_audio(
                            &mut audio,
                            pronunciation.id,
                            source_dialect,
                            pronunciation.voice_profile.as_ref(),
                        );
                    }
                }
            }
        }
    }
    for pos in &native.meanings.pos {
        for grammar in &pos.grammar_structures {
            for variant in &grammar.variants {
                generated_audio(
                    &mut audio,
                    grammar.id,
                    variant.dialect,
                    variant.voice_profile.as_ref(),
                );
                uploaded_audio(
                    &mut audio,
                    grammar.id,
                    variant.dialect,
                    &variant.audio_assets,
                );
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
                        uploaded_audio(&mut audio, *id, source_dialect, &variant.audio_assets);
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
                    uploaded_audio(
                        &mut audio,
                        sentence.id,
                        source_dialect,
                        &variant.audio_assets,
                    );
                }
            }
        }
    }
    audio
}

pub(crate) async fn audio_available(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    asset_id: Uuid,
    models_supported: bool,
) -> Result<bool, AppError> {
    let Some(publication) = published::read_current_in(tx, &[id]).await?.remove(&id) else {
        return Ok(false);
    };
    let native = publication.content_v3()?;
    let allowed = |audio: &DictionaryAudio| {
        audio.asset_id == asset_id && (models_supported || !audio.hd_flash)
    };
    if playable_audio(tx, own_audio(&native))
        .await?
        .iter()
        .any(allowed)
    {
        return Ok(true);
    }
    // Shared sentences have independent publication, hide and withdrawal state.
    // Keep their existing visibility boundary instead of caching it by word revision.
    Ok(entries(tx, &[id])
        .await?
        .into_iter()
        .any(|entry| entry.audio.iter().any(allowed)))
}

fn sourced_english_variants(
    content: &EnglishTextV3,
) -> Vec<(Dialect, &crate::lexicon::dto::RichTextVariantV3)> {
    match content {
        EnglishTextV3::Unified { common } => vec![(Dialect::Common, common)],
        EnglishTextV3::Distinguish { uk, us, .. } => [(Dialect::Uk, uk), (Dialect::Us, us)]
            .into_iter()
            .filter_map(|(dialect, slot)| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => {
                    Some((dialect, variant.as_ref()))
                }
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}

fn uploaded_audio(
    audio: &mut Vec<DictionaryAudio>,
    target_id: Uuid,
    source_dialect: Dialect,
    assets: &[crate::lexicon::audio_assets::dto::AudioAsset],
) {
    for asset in assets {
        let item = DictionaryAudio {
            target_id,
            asset_id: asset.id,
            locale: asset.locale.as_str().into(),
            accent_group: asset.locale.accent_group(),
            source_dialect,
            hd_flash: false,
            setting: None,
            default_recording: asset.is_default,
        };
        // Preserve the first preferred recording from the active source before
        // considering legacy pronunciation recordings or another text dialect.
        let has_default = audio.iter().any(|candidate| {
            candidate.default_recording
                && candidate.target_id == target_id
                && candidate.accent_group == item.accent_group
                && candidate.source_dialect == source_dialect
        });
        let index = (asset.is_default && !has_default)
            .then(|| {
                audio.iter().position(|candidate| {
                    candidate.target_id == target_id && candidate.accent_group == item.accent_group
                })
            })
            .flatten();
        if let Some(index) = index {
            audio.insert(index, item);
        } else {
            audio.push(item);
        }
    }
}

fn generated_audio(
    audio: &mut Vec<DictionaryAudio>,
    target_id: Uuid,
    source_dialect: Dialect,
    profile: Option<&VoiceProfileV3>,
) {
    let stable_side = profile.is_some_and(|profile| profile.dubbing.is_some());
    for setting in profile
        .into_iter()
        .flat_map(|profile| &profile.voices)
        .filter(|setting| setting.enabled)
    {
        if stable_side
            && match source_dialect {
                Dialect::Uk => {
                    setting.accent_group() != Some(crate::speech::locale::AccentGroup::Uk)
                }
                Dialect::Us => {
                    setting.accent_group() != Some(crate::speech::locale::AccentGroup::Us)
                }
                Dialect::Common => false,
            }
        {
            continue;
        }
        if let (Some(asset_id), Some(locale)) = (setting.audio_asset_id, setting.locale) {
            let mut setting = setting.clone();
            if stable_side && setting.accent.is_none() {
                setting.accent = match locale.accent_group() {
                    crate::speech::locale::AccentGroup::Uk => {
                        Some(crate::speech::locale::VoiceAccent::Gb)
                    }
                    crate::speech::locale::AccentGroup::Us => {
                        Some(crate::speech::locale::VoiceAccent::Us)
                    }
                    _ => None,
                };
            }
            audio.push(DictionaryAudio {
                target_id,
                asset_id,
                locale: locale.as_str().into(),
                accent_group: setting
                    .accent_group()
                    .unwrap_or_else(|| locale.accent_group()),
                source_dialect,
                hd_flash: setting.voice_id.starts_with("azure-hd-flash-"),
                setting: Some(setting.clone()),
                default_recording: false,
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

/// Preserve publication selection order; current catalog capability and asset readiness
/// may remove candidates but never replace them with live recommendations.
pub(crate) async fn playable_audio(
    tx: &mut Transaction<'_, Postgres>,
    audio: Vec<DictionaryAudio>,
) -> Result<Vec<DictionaryAudio>, AppError> {
    playable_audio_with_catalog(tx, audio, &mut None).await
}

async fn playable_audio_with_catalog(
    tx: &mut Transaction<'_, Postgres>,
    audio: Vec<DictionaryAudio>,
    catalog: &mut Option<Vec<sqlx::postgres::PgRow>>,
) -> Result<Vec<DictionaryAudio>, AppError> {
    use sqlx::Row;
    if audio.is_empty() {
        return Ok(audio);
    }
    let ids: Vec<_> = audio.iter().map(|item| item.asset_id).collect();
    let assets = sqlx::query("SELECT a.id,a.locale,g.state,g.resolved_content,g.source FROM lexicon.audio_assets a LEFT JOIN lexicon.audio_generations g ON g.asset_id=a.id WHERE a.id=ANY($1) AND a.reclamation_started_at IS NULL ORDER BY a.id FOR SHARE OF a")
        .bind(&ids).fetch_all(&mut **tx).await.map_err(AppError::internal)?;
    let mut result = Vec::new();
    for mut item in audio {
        let Some(asset) = assets
            .iter()
            .find(|row| row.get::<Uuid, _>("id") == item.asset_id)
        else {
            continue;
        };
        if asset.get::<String, _>("locale") != item.locale {
            continue;
        }
        if let Some(setting) = &item.setting {
            if asset.get::<Option<String>, _>("state").as_deref() != Some("ready") {
                continue;
            }
            let catalog = match catalog {
                Some(catalog) => catalog,
                None => catalog.insert(crate::speech::dubbing::playback_catalog(tx).await?),
            };
            let Some(voice) = catalog
                .iter()
                .find(|row| row.get::<String, _>("alias") == setting.voice_id)
            else {
                continue;
            };
            let accent: String = voice.get("accent");
            item.accent_group = match accent.as_str() {
                "gb" => crate::speech::locale::AccentGroup::Uk,
                "us" => crate::speech::locale::AccentGroup::Us,
                _ => crate::speech::locale::AccentGroup::Other,
            };
            let content = serde_json::from_value(asset.get("resolved_content"))
                .map_err(AppError::internal)?;
            let source: serde_json::Value = asset.get("source");
            if !crate::speech::dubbing::setting_available(
                catalog,
                setting,
                &accent,
                &content,
                (source["kind"] == "phoneme").then_some(item.locale.as_str()),
            ) {
                continue;
            }
        } else if asset.get::<Option<String>, _>("state").is_some() {
            continue; // Generated assets cannot enter via the recording path.
        }
        result.push(item);
    }
    Ok(result)
}

#[cfg(test)]
mod recording_default_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_side_sources_hide_paused_and_inactive_cached_audio_from_lookup_and_learning() {
        let uploaded = Uuid::now_v7();
        let generated = Uuid::now_v7();
        let target = Uuid::now_v7();
        let speech = |locale| json!({"content":{"version":2,"text":"far","annotations":[]},"voice_profile":{"voices":[{"voice_id":"voice","locale":locale,"enabled":true,"rate_percent":0,"audio_asset_id":generated}]}});
        let mut value = json!({"pos":[{"pos_id":Uuid::now_v7(),"pos":"noun","form_groups":[],"forms":[{"id":Uuid::now_v7(),"form_type":"base","regional_variants":{"mode":"common","common":{"id":Uuid::now_v7(),"dialect":"common","spelling":"far","origin":"manual","spelling_speech":speech("en-GB"),"pronunciations":[{"id":target,"dict_phonetic":"far","actual_pron":"far","style":"normal","synthesis":{"alphabet":"ipa","ipa":"","ups":"","uk":{"source":"none","ipa":"fɑː","ups":"F AA","ipa_speech":speech("en-GB")},"us":{"source":"upload","ipa":"fɑɹ","ups":"F AA R","ipa_speech":speech("en-US")}},"audio_assets":[{"id":uploaded,"locale":"en-US","gender":"female","content_type":"audio/mpeg","size_bytes":1,"original_name":"fixture.mp3","created_at":"2026-11-12T00:00:00Z"}]}]}}}]}]});
        let forms = serde_json::from_value(value.clone()).unwrap();
        let meanings = serde_json::from_value(json!({"pos":[],"sense_groups":[]})).unwrap();
        let native = published::PublishedContentV3 { forms, meanings };
        let audio = own_audio(&native);
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].target_id, target);
        assert_eq!(audio[0].asset_id, uploaded);
        assert_eq!(
            audio[0].accent_group,
            crate::speech::locale::AccentGroup::Us
        );
        value["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"][0]["synthesis"]
            ["us"]["source"] = json!("ipa");
        let next = published::PublishedContentV3 {
            forms: serde_json::from_value(value).unwrap(),
            meanings: native.meanings,
        };
        let audio = own_audio(&next);
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].asset_id, generated);
        assert!(audio[0].setting.is_some());
    }

    #[test]
    fn split_text_pool_does_not_consume_the_other_sides_retained_audio() {
        let uk = Uuid::now_v7();
        let us = Uuid::now_v7();
        let profile = serde_json::from_value(json!({"voices":[
            {"voice_id":"sonia","locale":"en-GB","enabled":true,"rate_percent":0,"audio_asset_id":uk},
            {"voice_id":"wayne","locale":"en-SG","accent":"us","enabled":true,"rate_percent":0,"audio_asset_id":us}
        ],"dubbing":{"area":"grammar","uk":{"mode":"local"},"us":{"mode":"inherit"}}})).unwrap();
        for (dialect, expected) in [(Dialect::Uk, uk), (Dialect::Us, us)] {
            let mut audio = vec![];
            generated_audio(&mut audio, Uuid::now_v7(), dialect, Some(&profile));
            assert_eq!(audio.len(), 1);
            assert_eq!(audio[0].asset_id, expected);
            assert_eq!(audio[0].source_dialect, dialect);
        }
    }

    #[test]
    fn active_spelling_recording_default_precedes_legacy_pronunciation_default() {
        let target = Uuid::now_v7();
        let spelling: crate::lexicon::audio_assets::dto::AudioAsset =
            serde_json::from_value(json!({
                "id":Uuid::now_v7(),"is_default":true,"locale":"en-GB","gender":"female",
                "content_type":"audio/mpeg","size_bytes":3,"duration_ms":null,
                "original_name":"spelling.mp3","created_at":"2026-11-10T00:00:00Z"
            }))
            .unwrap();
        let legacy = crate::lexicon::audio_assets::dto::AudioAsset {
            id: Uuid::now_v7(),
            original_name: "legacy.mp3".into(),
            ..spelling.clone()
        };
        let mut audio = vec![];
        uploaded_audio(
            &mut audio,
            target,
            Dialect::Common,
            std::slice::from_ref(&spelling),
        );
        uploaded_audio(
            &mut audio,
            target,
            Dialect::Common,
            std::slice::from_ref(&legacy),
        );
        assert_eq!(audio[0].asset_id, spelling.id);
        assert_eq!(audio[1].asset_id, legacy.id);
    }

    #[test]
    fn dictionary_recording_default_precedes_only_its_own_target_and_accent() {
        let target = Uuid::now_v7();
        let uk = Uuid::now_v7();
        let us = Uuid::now_v7();
        let profile = serde_json::from_value(json!({"voices":[
            {"voice_id":"sonia","enabled":true,"rate_percent":0,"locale":"en-GB","audio_asset_id":uk},
            {"voice_id":"jenny","enabled":true,"rate_percent":0,"locale":"en-US","audio_asset_id":us}
        ]})).unwrap();
        let mut audio = vec![];
        generated_audio(&mut audio, target, Dialect::Common, Some(&profile));
        let recording = serde_json::from_value(json!({
            "id":Uuid::now_v7(),"is_default":true,"locale":"en-GB","gender":"female",
            "content_type":"audio/mpeg","size_bytes":3,"duration_ms":null,
            "original_name":"fixture.mp3","created_at":"2026-11-10T00:00:00Z"
        }))
        .unwrap();
        uploaded_audio(&mut audio, target, Dialect::Common, &[recording]);
        assert_ne!(audio[0].asset_id, uk);
        assert_eq!(
            audio[0].accent_group,
            crate::speech::locale::AccentGroup::Uk
        );
        assert_eq!(audio[1].asset_id, uk);
        assert_eq!(audio[2].asset_id, us);
    }
}
