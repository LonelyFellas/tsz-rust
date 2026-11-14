use super::{language, model::*};
use crate::error::{AppError, ErrorCode};
use sqlx::PgConnection;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(super) fn validate(draft: &NextDraft, ids: &mut HashSet<Uuid>) -> Result<(), AppError> {
    let grammars = draft.grammar_structures.as_deref().unwrap_or_default();
    for grammar in grammars {
        language::identity(ids, grammar.id)?;
        if !language::POS.contains(&grammar.part_of_speech.as_str()) {
            return Err(language::invalid());
        }
        let mut locales = HashSet::new();
        for variant in &grammar.variants {
            language::identity(ids, variant.id)?;
            let mut issues = Vec::new();
            crate::lexicon::v3_contract::validate_voice_profile(
                variant.voice_profile.as_ref(),
                variant.id,
                &mut issues,
            );
            if !issues.is_empty()
                || variant
                    .voice_profile
                    .as_ref()
                    .and_then(|profile| profile.dubbing.as_ref())
                    .is_some_and(|pool| pool.area != crate::lexicon::dto::DubbingAreaV3::Grammar)
                || variant.audio_assets.len() > 8
                || variant
                    .audio_assets
                    .iter()
                    .map(|asset| asset.id)
                    .collect::<HashSet<_>>()
                    .len()
                    != variant.audio_assets.len()
            {
                return Err(language::invalid());
            }

            if !matches!(variant.locale.as_str(), "en" | "en-GB" | "en-US")
                || !locales.insert(variant.locale.as_str())
                || !crate::lexicon::rich_text::is_valid_native(&variant.content)
            {
                return Err(language::invalid());
            }
        }
        if locales.contains("en") && locales.len() > 1 {
            return Err(language::invalid());
        }
    }
    for sense in &draft.senses {
        if draft.grammar_structures.is_none()
            && (sense.sub_pos.is_some()
                || sense
                    .definitions
                    .iter()
                    .any(|d| d.grammar_structure_id.is_some()))
        {
            return Err(language::invalid());
        }
        for definition in &sense.definitions {
            if definition
                .grammar_structure_id
                .is_some_and(|id| !grammars.iter().any(|g| g.id == id))
            {
                return Err(AppError::unprocessable(
                    ErrorCode::InvalidRequestBody,
                    "释义引用的语法结构不存在，请重新选择",
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn catalog(conn: &mut PgConnection) -> Result<HashMap<String, String>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.code,p.code::text FROM catalog.sub_parts_of_speech s JOIN catalog.parts_of_speech p ON p.id=s.part_of_speech_id"
    ).fetch_all(conn).await.map_err(AppError::internal)?;
    Ok(rows.into_iter().collect())
}

pub(super) async fn validate_catalog(
    conn: &mut PgConnection,
    draft: &NextDraft,
) -> Result<(), AppError> {
    if draft.grammar_structures.is_none() {
        return Ok(());
    }
    let parents = catalog(conn).await?;
    for (pos, sub) in draft
        .grammar_structures
        .iter()
        .flatten()
        .map(|g| (Some(g.part_of_speech.as_str()), g.sub_pos.as_deref()))
        .chain(
            draft
                .senses
                .iter()
                .map(|s| (s.part_of_speech.as_deref(), s.sub_pos.as_deref())),
        )
    {
        if let Some(sub) = sub.filter(|s| !s.is_empty())
            && parents.get(sub).map(String::as_str) != pos
        {
            return Err(AppError::unprocessable(
                ErrorCode::InvalidRequestBody,
                "细分词性不属于当前基本词性，请重新选择",
            ));
        }
    }
    Ok(())
}

pub(super) fn valid_subcategory(pos: &str, sub: &str, parents: &HashMap<String, String>) -> bool {
    if sub.is_empty() {
        !parents.values().any(|parent| parent == pos)
    } else {
        parents.get(sub).is_some_and(|parent| parent == pos)
    }
}

pub(super) fn matches_sense(
    grammar: &NextGrammarStructure,
    sense: &NextSense,
    parents: &HashMap<String, String>,
) -> bool {
    Some(grammar.part_of_speech.as_str()) == sense.part_of_speech.as_deref()
        && grammar.sub_pos.as_deref().unwrap_or("") == sense.sub_pos.as_deref().unwrap_or("")
        && valid_subcategory(
            &grammar.part_of_speech,
            grammar.sub_pos.as_deref().unwrap_or(""),
            parents,
        )
}

/// Incomplete bindings may be saved; the language view reports what still needs repair.
pub(super) fn issues(
    draft: &NextDraft,
    parents: &HashMap<String, String>,
) -> Vec<NextGrammarIssue> {
    let Some(grammars) = &draft.grammar_structures else {
        return vec![];
    };
    let by_id: HashMap<_, _> = grammars.iter().map(|g| (g.id, g)).collect();
    let mut issues = Vec::new();
    let mut add = |node_id, field: &str, message: &str| {
        issues.push(NextGrammarIssue {
            node_id,
            field: field.into(),
            message: message.into(),
        })
    };
    for grammar in grammars {
        let sub = grammar.sub_pos.as_deref().unwrap_or("");
        if !valid_subcategory(&grammar.part_of_speech, sub, parents) {
            add(
                grammar.id,
                "sub_pos",
                "语法结构需要绑定当前基本词性下有效的细分词性",
            );
        }
        if grammar.variants.is_empty()
            || grammar
                .variants
                .iter()
                .any(|v| v.content.text().trim().is_empty())
            || (grammar.variants.iter().any(|v| v.locale != "en") && grammar.variants.len() != 2)
        {
            add(grammar.id, "variants", "请完整填写语法结构的英美内容");
        }
    }
    for sense in &draft.senses {
        let sub = sense.sub_pos.as_deref().unwrap_or("");
        if !valid_subcategory(sense.part_of_speech.as_deref().unwrap_or(""), sub, parents) {
            add(sense.id, "sub_pos", "请选择义项的细分词性");
        }
        for definition in &sense.definitions {
            if definition
                .grammar_structure_id
                .and_then(|id| by_id.get(&id))
                .is_none_or(|g| !matches_sense(g, sense, parents))
            {
                add(
                    definition.id,
                    "grammar_structure_id",
                    "请选择与义项细分词性匹配的语法结构",
                );
            }
        }
    }
    issues
}

pub(super) async fn persist(
    conn: &mut PgConnection,
    entry: Uuid,
    draft: &NextDraft,
) -> Result<(), AppError> {
    let ids = draft
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|g| std::iter::once(g.id).chain(g.variants.iter().map(|v| v.id)))
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    let claimed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=ANY($1))",
    )
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    if claimed {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "grammar identity already belongs to the forms editor",
        ));
    }
    // The shared speech validator uses owner dialect to scope inherited recommendations.
    let mut audio = serde_json::to_value(&draft.grammar_structures).map_err(AppError::internal)?;
    if let Some(grammars) = audio.as_array_mut() {
        for grammar in grammars {
            for variant in grammar["variants"].as_array_mut().into_iter().flatten() {
                variant["dialect"] = match variant["locale"].as_str() {
                    Some("en-GB") => "uk",
                    Some("en-US") => "us",
                    _ => "common",
                }
                .into();
            }
        }
    }
    crate::speech::generated::validate_bindings(conn, &audio).await?;
    for grammar in draft.grammar_structures.iter().flatten() {
        super::repository::node(conn, grammar.id, entry, "grammar", None).await?;
        for variant in &grammar.variants {
            let kind = match variant.locale.as_str() {
                "en" => "grammar_en",
                "en-GB" => "grammar_en_gb",
                _ => "grammar_en_us",
            };
            super::repository::node(conn, variant.id, entry, kind, Some(grammar.id)).await?;
        }
    }
    Ok(())
}

pub(super) fn has_audio(draft: &NextDraft) -> bool {
    draft
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|grammar| &grammar.variants)
        .any(|variant| variant.voice_profile.is_some() || !variant.audio_assets.is_empty())
}

pub(super) fn strip_audio(grammars: Option<&mut Vec<NextGrammarStructure>>) {
    for variant in grammars
        .into_iter()
        .flatten()
        .flat_map(|grammar| &mut grammar.variants)
    {
        variant.voice_profile = None;
        variant.audio_assets.clear();
    }
}

pub(super) fn preserve_audio(draft: &mut NextDraft, previous: &NextDraft) {
    let old: HashMap<_, _> = previous
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|grammar| &grammar.variants)
        .map(|variant| (variant.id, variant))
        .collect();
    for variant in draft
        .grammar_structures
        .iter_mut()
        .flatten()
        .flat_map(|grammar| &mut grammar.variants)
    {
        if let Some(previous) = old.get(&variant.id) {
            variant.voice_profile = previous.voice_profile.clone();
            if variant.content.text() == previous.content.text() {
                variant.audio_assets = previous.audio_assets.clone();
            } else if let Some(profile) = &mut variant.voice_profile {
                for voice in &mut profile.voices {
                    voice.audio_asset_id = None;
                }
            }
        }
    }
}

pub(super) fn validate_audio_scope(draft: &NextDraft) -> Result<(), AppError> {
    for variant in draft
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|grammar| &grammar.variants)
    {
        let accent = match variant.locale.as_str() {
            "en-GB" => Some(crate::speech::locale::AccentGroup::Uk),
            "en-US" => Some(crate::speech::locale::AccentGroup::Us),
            _ => None,
        };
        if variant
            .audio_assets
            .iter()
            .any(|asset| accent.is_some_and(|accent| asset.locale.accent_group() != accent))
        {
            return Err(language::invalid());
        }
    }
    Ok(())
}
