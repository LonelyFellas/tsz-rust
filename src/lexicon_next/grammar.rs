use super::{language, model::*};
use crate::error::{AppError, ErrorCode};
use sqlx::{PgConnection, Row};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(super) fn validate(draft: &NextDraft, ids: &mut HashSet<Uuid>) -> Result<(), AppError> {
    let grammars = draft.grammar_structures.as_deref().unwrap_or_default();
    for grammar in grammars {
        language::identity(ids, grammar.id)?;
        if !language::valid_pos(&grammar.part_of_speech) {
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
    kind: &str,
) -> Result<(), AppError> {
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT code::text FROM catalog.parts_of_speech WHERE kind=$1 FOR SHARE",
    )
    .bind(kind)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
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
        if pos.is_some_and(|code| !codes.iter().any(|saved| saved == code)) {
            return Err(AppError::unprocessable(
                ErrorCode::InvalidRequestBody,
                "基本词性不属于当前词条类型，请重新选择",
            ));
        }
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
            add(sense.id, "sub_pos", "请选择词义的细分词性");
        }
        for definition in &sense.definitions {
            if definition.grammar_unbound == Some(true) {
                continue;
            }
            if definition
                .grammar_structure_id
                .and_then(|id| by_id.get(&id))
                .is_none_or(|g| !matches_sense(g, sense, parents))
            {
                add(
                    definition.id,
                    "grammar_structure_id",
                    "请选择与词义细分词性匹配的语法结构",
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
    let mut audio_grammars = draft.grammar_structures.clone();
    for variant in audio_grammars
        .iter_mut()
        .flatten()
        .flat_map(|grammar| &mut grammar.variants)
    {
        if let Some(links) = &mut variant.form_links {
            links.retain(|link| {
                !matches!(
                    link.target_source,
                    Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
                )
            });
        }
    }
    let mut audio = serde_json::to_value(&audio_grammars).map_err(AppError::internal)?;
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
    crate::speech::generated::validate_bindings(
        conn,
        &serde_json::json!({ "grammar_structures": audio }),
    )
    .await?;
    for grammar in draft.grammar_structures.iter().flatten() {
        super::repository::node(conn, grammar.id, entry, "grammar", None).await?;
        for variant in &grammar.variants {
            let kind = match variant.locale.as_str() {
                "en" => "grammar_en",
                "en-GB" => "grammar_en_gb",
                _ => "grammar_en_us",
            };
            super::repository::node(conn, variant.id, entry, kind, Some(grammar.id)).await?;
            if let Some(links) = &variant.form_links {
                if !crate::lexicon::service::grammar_form_links::valid_reading_ranges(
                    variant.content.text(),
                    links,
                ) {
                    return Err(language::invalid());
                }
                sqlx::query(
                    "DELETE FROM lexicon_next.grammar_form_links WHERE source_variant_id=$1",
                )
                .bind(variant.id)
                .execute(&mut *conn)
                .await
                .map_err(AppError::internal)?;
                for link in links {
                    if link.source_segments.len() != 1
                        || link.target_word_id == entry
                        || link.target_dialect.as_str().is_empty()
                    {
                        return Err(language::invalid());
                    }
                    if matches!(
                        link.target_source,
                        Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
                    ) && let Some(target_run) =
                        crate::test_resources::entry_run(conn, link.target_word_id).await?
                    {
                        let run = crate::test_resources::entry_run(conn, entry).await?;
                        crate::test_resources::require_same_run(run.as_deref(), Some(&target_run))?;
                    }
                    validate_form_target(
                        conn,
                        link.target_source.as_ref(),
                        link.target_word_id,
                        link.target_publication_id,
                        link.target_pos_id,
                        link.target_form_id,
                        link.target_variant_id,
                    )
                    .await?;
                    let segment = &link.source_segments[0];
                    sqlx::query(
                        "INSERT INTO lexicon_next.grammar_form_links (id,entry_id,source_variant_id,source_start,source_end,source_surface,target_source,target_word_id,target_publication_id,target_pos_id,target_form_id,target_variant_id,target_dialect,reading_reference) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
                    )
                    .bind(link.id)
                    .bind(entry)
                    .bind(variant.id)
                    .bind(segment.start as i32)
                    .bind(segment.end as i32)
                    .bind(&segment.surface)
                    .bind(
                        link
                            .target_source
                            .as_ref()
                            .map(|source| match source {
                                crate::lexicon::dto::AssociationTargetSource::V3 => "v3",
                                crate::lexicon::dto::AssociationTargetSource::LexiconNext => {
                                    "lexicon_next"
                                }
                            })
                            .unwrap_or("v3"),
                    )
                    .bind(link.target_word_id)
                    .bind(link.target_publication_id)
                    .bind(link.target_pos_id)
                    .bind(link.target_form_id)
                    .bind(link.target_variant_id)
                    .bind(link.target_dialect.as_str())
                    .bind(link.reading_reference.as_ref().map(serde_json::to_value).transpose().map_err(AppError::internal)?)
                    .execute(&mut *conn)
                    .await
                    .map_err(AppError::internal)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn hydrate(
    conn: &mut PgConnection,
    entry: Uuid,
    draft: &mut NextDraft,
) -> Result<(), AppError> {
    let rows = sqlx::query(
        "SELECT source_variant_id,id,source_start,source_end,source_surface,target_source,target_word_id,target_publication_id,target_pos_id,target_form_id,target_variant_id,target_dialect,reading_reference FROM lexicon_next.grammar_form_links WHERE entry_id=$1 ORDER BY source_variant_id,source_start",
    )
    .bind(entry)
    .fetch_all(&mut *conn)
    .await
    .map_err(AppError::internal)?;
    for variant in draft
        .grammar_structures
        .iter_mut()
        .flatten()
        .flat_map(|grammar| &mut grammar.variants)
    {
        let links = rows
            .iter()
            .filter(|row| row.get::<Uuid, _>("source_variant_id") == variant.id)
            .map(|row| {
                let target_dialect = crate::lexicon::dto::Dialect::parse(
                    row.get::<String, _>("target_dialect").as_str(),
                )
                .ok_or_else(language::invalid)?;
                let reading_reference = row
                    .get::<Option<serde_json::Value>, _>("reading_reference")
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(AppError::internal)?;
                Ok(crate::lexicon::dto::GrammarFormLinkV3 {
                    id: row.get("id"),
                    source_segments: vec![crate::lexicon::dto::SentenceSourceRangeV1 {
                        start: row.get::<i32, _>("source_start") as usize,
                        end: row.get::<i32, _>("source_end") as usize,
                        surface: row.get("source_surface"),
                    }],
                    target_word_id: row.get("target_word_id"),
                    target_source: match row.get::<String, _>("target_source").as_str() {
                        "v3" => None,
                        "lexicon_next" => {
                            Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
                        }
                        _ => return Err(language::invalid()),
                    },
                    target_publication_id: row.get("target_publication_id"),
                    target_pos_id: row.get("target_pos_id"),
                    target_form_id: row.get("target_form_id"),
                    target_variant_id: row.get("target_variant_id"),
                    target_dialect,
                    reading_reference,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        if !links.is_empty() {
            variant.form_links = Some(links);
        }
    }
    Ok(())
}

async fn validate_form_target(
    conn: &mut PgConnection,
    source: Option<&crate::lexicon::dto::AssociationTargetSource>,
    entry: Uuid,
    publication: Option<Uuid>,
    pos: Uuid,
    form: Uuid,
    variant: Uuid,
) -> Result<(), AppError> {
    let next = matches!(
        source,
        Some(crate::lexicon::dto::AssociationTargetSource::LexiconNext)
    );
    let valid: bool = if let Some(publication) = publication.filter(|_| !next) {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1
                FROM lexicon.entries e
                JOIN lexicon.entry_publications p
                  ON p.id=$2 AND p.entry_id=e.id
                JOIN lexicon.entry_publication_nodes pos_pub
                  ON pos_pub.publication_id=p.id AND pos_pub.entry_id=e.id AND pos_pub.node_id=$3 AND pos_pub.node_type='pos'
                JOIN lexicon.nodes pos
                  ON pos.id=pos_pub.node_id AND pos.entry_id=e.id AND pos.node_type='pos'
                JOIN lexicon.entry_publication_nodes form_pub
                  ON form_pub.publication_id=p.id AND form_pub.entry_id=e.id AND form_pub.node_id=$4 AND form_pub.node_type='concrete_form'
                JOIN lexicon.nodes form
                  ON form.id=form_pub.node_id AND form.entry_id=e.id AND form.node_type='concrete_form'
                LEFT JOIN lexicon.nodes form_parent
                  ON form_parent.id=form.parent_node_id AND form_parent.entry_id=e.id
                JOIN lexicon.entry_publication_nodes variant_pub
                  ON variant_pub.publication_id=p.id AND variant_pub.entry_id=e.id AND variant_pub.node_id=$5 AND variant_pub.node_type='form_variant'
                JOIN lexicon.nodes variant
                  ON variant.id=variant_pub.node_id AND variant.entry_id=e.id AND variant.node_type='form_variant' AND variant.parent_node_id=form.id
                WHERE e.id=$1 AND e.kind='word'
                  AND (form.parent_node_id=pos.id OR (form_parent.node_type='form_group' AND form_parent.parent_node_id=pos.id))
            )",
        )
            .bind(entry)
            .bind(publication)
            .bind(pos)
            .bind(form)
            .bind(variant)
            .fetch_one(&mut *conn)
            .await
            .map_err(AppError::internal)?
    } else if next {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM lexicon_next.entries e WHERE e.id=$1 AND e.kind='word') AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$2 AND entry_id=$1 AND role='pos') AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$3 AND entry_id=$1 AND role='form' AND parent_id=$2) AND EXISTS(SELECT 1 FROM lexicon_next.form_editor_nodes WHERE id=$4 AND entry_id=$1 AND role='variant' AND parent_id=$3)",
        )
        .bind(entry)
        .bind(pos)
        .bind(form)
        .bind(variant)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
    } else {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM lexicon.entries e WHERE e.id=$1 AND e.kind='word') AND EXISTS(SELECT 1 FROM lexicon.nodes WHERE id=$2 AND entry_id=$1 AND node_type='pos') AND EXISTS(SELECT 1 FROM lexicon.v3_concrete_forms form WHERE form.id=$3 AND form.entry_id=$1 AND form.entry_pos_id=$2) AND EXISTS(SELECT 1 FROM lexicon.v3_form_variants variant WHERE variant.id=$4 AND variant.entry_id=$1 AND variant.form_id=$3)",
        )
        .bind(entry)
        .bind(pos)
        .bind(form)
        .bind(variant)
        .fetch_one(&mut *conn)
        .await
        .map_err(AppError::internal)?
    };
    if valid {
        Ok(())
    } else {
        Err(AppError::unprocessable(
            ErrorCode::InvalidRequestBody,
            "语法关联目标词形不存在，请重新选择",
        ))
    }
}

pub(super) fn has_audio(draft: &NextDraft) -> bool {
    draft
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|grammar| &grammar.variants)
        .any(|variant| variant.voice_profile.is_some() || !variant.audio_assets.is_empty())
}

pub(super) fn has_form_links(draft: &NextDraft) -> bool {
    draft
        .grammar_structures
        .iter()
        .flatten()
        .flat_map(|grammar| &grammar.variants)
        .any(|variant| variant.form_links.is_some())
}

pub(super) fn preserve_form_links(draft: &mut NextDraft, previous: &NextDraft) {
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
        if variant.form_links.is_none()
            && let Some(previous) = old.get(&variant.id)
        {
            variant.form_links = previous.form_links.clone();
        }
    }
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
            if variant.content == previous.content {
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
