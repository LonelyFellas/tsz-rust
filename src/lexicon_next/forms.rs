//! Reuse the V1 authoring contract, never its entry IDs or storage.
use std::collections::HashSet;

use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{language, model::*};
use crate::{
    error::{AppError, ErrorCode},
    lexicon::dto::*,
};

pub(super) fn validate(
    editor: &SharedFormsStepContentV3,
    ids: &mut HashSet<Uuid>,
) -> Result<(), AppError> {
    let forms = editor
        .clone()
        .resolve()
        .map_err(|error| error.into_app_error())?;
    let issues = crate::lexicon::v3_contract::validate_forms(&forms, StepSaveIntent::Save);
    if !issues.is_empty() {
        return Err(crate::lexicon::v3_contract::contract_validation_error(
            &issues,
        ));
    }
    for (id, _, _) in nodes(editor) {
        language::identity(ids, id)?;
    }
    Ok(())
}

fn nodes(editor: &SharedFormsStepContentV3) -> Vec<(Uuid, &'static str, Option<Uuid>)> {
    let mut nodes = Vec::new();
    for content in &editor.contents {
        nodes.push((content.id, "content", None));
        for (_, variant) in content.regional_variants.variants() {
            nodes.push((variant.id, "content_variant", Some(content.id)));
            for pronunciation in &variant.pronunciations {
                nodes.push((pronunciation.id, "content_pronunciation", Some(variant.id)));
            }
        }
    }
    for pos in &editor.pos {
        nodes.push((pos.pos_id, "pos", None));
        for group in &pos.form_groups {
            nodes.push((group.id, "group", Some(pos.pos_id)));
            for member in &group.members {
                nodes.push((member.id, "membership", Some(group.id)));
            }
        }
        for form in &pos.forms {
            nodes.push((form.id, "form", Some(pos.pos_id)));
            for (_, variant) in form.regional_bindings.variants() {
                nodes.push((variant.id, "variant", Some(form.id)));
                for pronunciation in &variant.pronunciations {
                    nodes.push((pronunciation.id, "pronunciation", Some(variant.id)));
                }
            }
        }
    }
    nodes
}

pub(super) fn projection(draft: &NextDraft) -> Result<Vec<NextForm>, AppError> {
    let mut rows = draft.forms.clone();
    if let Some(editor) = &draft.form_editor {
        for content in &editor.contents {
            for (region, variant) in content.regional_variants.variants() {
                if !variant.spelling.trim().is_empty() {
                    rows.push(NextForm {
                        id: variant.id,
                        locale: match region {
                            "uk" => "en-GB",
                            "us" => "en-US",
                            _ => "en",
                        }
                        .into(),
                        text: variant.spelling.clone(),
                    });
                }
            }
        }
    }
    let mut ids = HashSet::new();
    for row in &rows {
        language::identity(&mut ids, row.id)?;
    }
    Ok(rows)
}

async fn identity(
    conn: &mut PgConnection,
    entry: Uuid,
    id: Uuid,
    role: &str,
    parent: Option<Uuid>,
) -> Result<(), AppError> {
    if id.is_nil() {
        return Err(language::invalid());
    }
    sqlx::query("INSERT INTO lexicon_next.form_editor_nodes(id,entry_id,role,parent_id) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(id).bind(entry).bind(role).bind(parent).execute(&mut *conn).await.map_err(AppError::internal)?;
    let current: (Uuid, String, Option<Uuid>) = sqlx::query_as(
        "SELECT entry_id,role,parent_id FROM lexicon_next.form_editor_nodes WHERE id=$1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if current != (entry, role.into(), parent) {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "form editor node identity changed",
        ));
    }
    Ok(())
}

pub(super) async fn persist(
    conn: &mut PgConnection,
    actor: Uuid,
    entry: Uuid,
    draft: &mut NextDraft,
) -> Result<(), AppError> {
    let Some(editor) = &mut draft.form_editor else {
        return Ok(());
    };
    let resolved = editor
        .clone()
        .resolve()
        .map_err(|error| error.into_app_error())?;
    let expanded = serde_json::json!({"pos": resolved.pos});
    crate::speech::generated::validate_bindings_with_forms(
        conn,
        &expanded,
        Some((entry, &resolved)),
        None,
    )
    .await?;
    let editor_nodes = nodes(editor);
    let node_ids = editor_nodes
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<Vec<_>>();
    let grammar_claimed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lexicon_next.nodes WHERE id=ANY($1) AND kind IN ('grammar','grammar_en','grammar_en_gb','grammar_en_us'))")
        .bind(&node_ids).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
    if grammar_claimed {
        return Err(AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "forms identity already belongs to grammar",
        ));
    }
    for (id, role, parent) in editor_nodes {
        identity(conn, entry, id, role, parent).await?;
    }
    let mut serialized = serde_json::to_value(&*editor).map_err(AppError::internal)?;
    let mut ids = crate::speech::generated_references::asset_references(&serialized)
        .into_iter()
        .flat_map(|(_, ids)| ids)
        .collect::<HashSet<_>>();
    fn recording_ids(value: &Value, ids: &mut HashSet<Uuid>) -> Result<(), AppError> {
        if let Some(assets) = value.get("audio_assets") {
            let assets: Vec<crate::lexicon::audio_assets::dto::AudioAsset> =
                serde_json::from_value(assets.clone()).map_err(|_| language::invalid())?;
            ids.extend(assets.iter().map(|asset| asset.id));
        }
        match value {
            Value::Object(fields) => {
                for child in fields.values() {
                    recording_ids(child, ids)?;
                }
            }
            Value::Array(items) => {
                for child in items {
                    recording_ids(child, ids)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    recording_ids(&serialized, &mut ids)?;
    let ids = ids.into_iter().collect::<Vec<_>>();
    // Lock before checking references: both reclamation and other claimants serialize here.
    sqlx::query("SELECT id FROM lexicon.audio_assets WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    let rows = sqlx::query(r#"SELECT a.id,
      (a.created_by_admin_id=$2 OR EXISTS(SELECT 1 FROM lexicon_next.audio_asset_references r WHERE r.asset_id=a.id AND r.entry_id=$3)) AS accessible,
      (EXISTS(SELECT 1 FROM lexicon_next.audio_asset_references r WHERE r.asset_id=a.id AND r.entry_id<>$3)
       OR EXISTS(SELECT 1 FROM lexicon.v3_audio_asset_references r WHERE r.asset_id=a.id)
       OR EXISTS(SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r WHERE r.asset_id=a.id)) AS taken,
      jsonb_build_object('id',a.id,'locale',a.locale,'gender',a.gender,'content_type',a.content_type,'size_bytes',a.size_bytes,'duration_ms',a.duration_ms,'original_name',a.original_name,'created_at',a.created_at) AS metadata
      FROM lexicon.audio_assets a WHERE a.id=ANY($1) AND a.reclamation_started_at IS NULL
      AND NOT EXISTS(SELECT 1 FROM lexicon.audio_generations g WHERE g.asset_id=a.id AND g.state<>'ready')"#)
        .bind(&ids).bind(actor).bind(entry).fetch_all(&mut *conn).await.map_err(AppError::internal)?;
    let mut canonical = std::collections::HashMap::new();
    for row in rows {
        if !row.get::<bool, _>("accessible") || row.get::<bool, _>("taken") {
            return Err(language::invalid());
        }
        canonical.insert(
            row.get::<Uuid, _>("id"),
            serde_json::from_value(row.get::<Value, _>("metadata")).map_err(AppError::internal)?,
        );
    }
    if canonical.len() != ids.len() {
        return Err(language::invalid());
    }
    fn hydrate(
        value: &mut Value,
        canonical: &std::collections::HashMap<Uuid, crate::lexicon::audio_assets::dto::AudioAsset>,
    ) -> Result<(), AppError> {
        if let Some(assets) = value.get_mut("audio_assets") {
            let mut rows = serde_json::from_value::<
                Vec<crate::lexicon::audio_assets::dto::AudioAsset>,
            >(assets.clone())
            .map_err(AppError::internal)?;
            crate::lexicon::audio_assets::hydrate_recordings(&mut rows, canonical)?;
            *assets = serde_json::to_value(rows).map_err(AppError::internal)?;
        }
        match value {
            Value::Object(fields) => {
                for child in fields.values_mut() {
                    hydrate(child, canonical)?;
                }
            }
            Value::Array(items) => {
                for child in items {
                    hydrate(child, canonical)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    hydrate(&mut serialized, &canonical)?;
    *editor = serde_json::from_value(serialized).map_err(AppError::internal)?;
    sqlx::query("DELETE FROM lexicon_next.audio_asset_references WHERE entry_id=$1")
        .bind(entry)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    for asset in ids {
        sqlx::query(
            "INSERT INTO lexicon_next.audio_asset_references(entry_id,asset_id) VALUES($1,$2)",
        )
        .bind(entry)
        .bind(asset)
        .execute(&mut *conn)
        .await
        .map_err(AppError::internal)?;
    }
    Ok(())
}
