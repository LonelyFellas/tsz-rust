//! Shared asset ownership and canonical metadata for forms and grammar.
use super::{language, model::NextDraft};
use crate::error::AppError;
use serde_json::Value;
use sqlx::{PgConnection, Row};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) async fn persist(
    conn: &mut PgConnection,
    actor: Uuid,
    entry: Uuid,
    draft: &mut NextDraft,
) -> Result<(), AppError> {
    let mut serialized = serde_json::to_value(&*draft).map_err(AppError::internal)?;
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
    *draft = serde_json::from_value(serialized).map_err(AppError::internal)?;
    super::grammar::validate_audio_scope(draft)?;
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
