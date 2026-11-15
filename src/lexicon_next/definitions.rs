//! Negotiated per-definition details, stored in the immutable draft revision.
use super::{language, model::*};
use crate::{error::AppError, speech::locale::AccentGroup};
use sqlx::PgConnection;
use std::collections::{HashMap, HashSet};

pub(super) fn has_details(draft: &NextDraft) -> bool {
    draft.senses.iter().flat_map(|s| &s.definitions).any(|d| {
        d.level.is_some()
            || d.grammar_unbound.is_some()
            || d.content.is_some()
            || d.voice_profile.is_some()
            || !d.audio_assets.is_empty()
    })
}

pub(super) fn strip(d: &mut NextDefinition) {
    d.level = None;
    d.grammar_unbound = None;
    d.content = None;
    d.voice_profile = None;
    d.audio_assets.clear();
}

pub(super) fn preserve(draft: &mut NextDraft, previous: &NextDraft) {
    let old: HashMap<_, _> = previous
        .senses
        .iter()
        .flat_map(|s| &s.definitions)
        .map(|d| (d.id, d))
        .collect();
    for d in draft.senses.iter_mut().flat_map(|s| &mut s.definitions) {
        let Some(saved) = old.get(&d.id) else {
            continue;
        };
        d.level = saved.level.clone();
        if d.grammar_structure_id.is_none() {
            d.grammar_unbound = saved.grammar_unbound;
        }
        if d.locale != saved.locale {
            continue;
        }
        d.voice_profile = saved.voice_profile.clone();
        if d.text == saved.text {
            d.content = saved.content.clone();
            d.audio_assets = saved.audio_assets.clone();
        } else if let Some(profile) = &mut d.voice_profile {
            // Both generated versions and recordings describe the previous source.
            for voice in &mut profile.voices {
                voice.audio_asset_id = None;
            }
        }
        // A profile needs a rich text owner for the shared speech source validator.
        if d.content.is_none() && d.voice_profile.is_some() {
            d.content = Some(crate::lexicon::dto::RichTextV3::V2(
                crate::lexicon::dto::RichTextV2V3 {
                    version: 2,
                    text: d.text.clone(),
                    annotations: vec![],
                },
            ));
        }
    }
}

pub(super) fn validate(draft: &NextDraft) -> Result<(), AppError> {
    for d in draft.senses.iter().flat_map(|s| &s.definitions) {
        let english = matches!(d.locale.as_str(), "en" | "en-GB" | "en-US");
        let accent = match d.locale.as_str() {
            "en-GB" => Some(AccentGroup::Uk),
            "en-US" => Some(AccentGroup::Us),
            _ => None,
        };
        if d.level
            .as_deref()
            .is_some_and(|v| !crate::lexicon::validation::valid_level(v))
            || d.grammar_unbound == Some(false)
            || (d.grammar_unbound.is_some() && d.grammar_structure_id.is_some())
            || (!english
                && (d.content.is_some() || d.voice_profile.is_some() || !d.audio_assets.is_empty()))
            || d.content.as_ref().is_some_and(|c| {
                !crate::lexicon::rich_text::is_valid_native(c) || c.text() != d.text
            })
            || (d.content.is_none() && (d.voice_profile.is_some() || !d.audio_assets.is_empty()))
            || d.audio_assets.len() > 8
            || d.audio_assets
                .iter()
                .map(|a| a.id)
                .collect::<HashSet<_>>()
                .len()
                != d.audio_assets.len()
            || d.audio_assets
                .iter()
                .any(|a| accent.is_some_and(|accent| a.locale.accent_group() != accent))
        {
            return Err(language::invalid());
        }
        let mut issues = vec![];
        crate::lexicon::v3_contract::validate_voice_profile(
            d.voice_profile.as_ref(),
            d.id,
            &mut issues,
        );
        if !issues.is_empty()
            || d.voice_profile.as_ref().is_some_and(|p| {
                p.dubbing
                    .as_ref()
                    .is_some_and(|pool| pool.area != crate::lexicon::dto::DubbingAreaV3::Meaning)
                    || p.voices
                        .iter()
                        .any(|voice| accent.is_some_and(|a| voice.accent_group() != Some(a)))
            })
        {
            return Err(language::invalid());
        }
    }
    Ok(())
}

pub(super) async fn validate_bindings(
    conn: &mut PgConnection,
    draft: &NextDraft,
) -> Result<(), AppError> {
    let recordings: Vec<_> = draft
        .senses
        .iter()
        .flat_map(|s| &s.definitions)
        .flat_map(|d| d.audio_assets.iter().map(|a| a.id))
        .collect();
    let generated_recording: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon.audio_generations WHERE asset_id=ANY($1))",
    )
    .bind(&recordings)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if generated_recording {
        // A generated asset must carry its voice setting so its source can be verified.
        return Err(language::invalid());
    }
    let mut owners = Vec::new();
    for d in draft
        .senses
        .iter()
        .flat_map(|s| &s.definitions)
        .filter(|d| d.voice_profile.is_some())
    {
        let mut owner = serde_json::to_value(d).map_err(AppError::internal)?;
        owner["dialect"] = match d.locale.as_str() {
            "en-GB" => "uk",
            "en-US" => "us",
            _ => "common",
        }
        .into();
        owners.push(owner);
    }
    crate::speech::generated::validate_bindings(conn, &serde_json::json!(owners)).await
}
