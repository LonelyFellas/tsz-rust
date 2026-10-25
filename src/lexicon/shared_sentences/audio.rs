use std::collections::{HashMap, HashSet};

use super::*;
use crate::lexicon::{audio_assets::dto::AudioAsset, dto::EnglishTextV3};

pub(super) async fn normalize(
    tx: &mut PgConnection,
    actor: Uuid,
    sentence_id: Uuid,
    text: &mut EnglishTextV3,
) -> Result<(), AppError> {
    let serialized = serde_json::to_value(&*text).map_err(AppError::internal)?;
    crate::speech::generated::validate_bindings(tx, &serialized).await?;
    let generated = crate::speech::generated_source::references(&serialized)?;
    let variants = crate::lexicon::v3_contract::english_text_variants(text);
    let mut ids = generated
        .iter()
        .map(|reference| reference.asset)
        .collect::<HashSet<_>>();
    for variant in variants {
        let mut seen = HashSet::new();
        if variant.audio_assets.len() > 8 || variant.audio_assets.iter().any(|a| !seen.insert(a.id))
        {
            return Err(invalid("每段正文最多引用 8 条不重复的音频"));
        }
        ids.extend(seen);
    }
    if ids.is_empty() {
        return Ok(());
    }
    let ids = ids.into_iter().collect::<Vec<_>>();
    // Re-read ownership with a fresh snapshot after any concurrent claim commits.
    sqlx::query("SELECT id FROM lexicon.audio_assets WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let rows = sqlx::query(
        r#"SELECT asset.id,
            (asset.created_by_admin_id=$3 OR EXISTS (
                SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r
                WHERE r.asset_id=asset.id AND r.sentence_id=$2)) AS accessible,
            (EXISTS (SELECT 1 FROM lexicon.v3_audio_asset_references r WHERE r.asset_id=asset.id)
             OR EXISTS (SELECT 1 FROM lexicon.shared_sentence_audio_asset_references r
                WHERE r.asset_id=asset.id AND r.sentence_id<>$2)) AS taken,
            jsonb_build_object('id',asset.id,'locale',asset.locale,'gender',asset.gender,
                'content_type',asset.content_type,'size_bytes',asset.size_bytes,
                'duration_ms',asset.duration_ms,'original_name',asset.original_name,
                'created_at',asset.created_at) AS metadata
        FROM lexicon.audio_assets asset
        WHERE asset.id=ANY($1) AND asset.reclamation_started_at IS NULL
          AND NOT EXISTS (SELECT 1 FROM lexicon.audio_generations g WHERE g.asset_id=asset.id AND g.state<>'ready')
        "#,
    )
    .bind(&ids)
    .bind(sentence_id)
    .bind(actor)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let mut canonical = HashMap::<Uuid, AudioAsset>::new();
    for row in rows {
        if !row.get::<bool, _>("accessible") || row.get::<bool, _>("taken") {
            return Err(invalid("音频不存在、不可访问或已被其他内容使用"));
        }
        canonical.insert(
            row.get("id"),
            serde_json::from_value(row.get("metadata")).map_err(AppError::internal)?,
        );
    }
    if canonical.len() != ids.len() {
        return Err(invalid("音频不存在、不可访问或正在回收"));
    }
    for variant in variants_mut(text) {
        for asset in &mut variant.audio_assets {
            *asset = canonical[&asset.id].clone();
        }
    }
    Ok(())
}

fn variants_mut(text: &mut EnglishTextV3) -> Vec<&mut crate::lexicon::dto::RichTextVariantV3> {
    use crate::lexicon::dto::DialectVariantRichTextSlotV3;
    match text {
        EnglishTextV3::Unified { common } => vec![common],
        EnglishTextV3::Distinguish { uk, us, .. } => [uk, us]
            .into_iter()
            .filter_map(|slot| match slot {
                DialectVariantRichTextSlotV3::Ready { variant } => Some(variant),
                DialectVariantRichTextSlotV3::Missing => None,
            })
            .collect(),
    }
}

pub(super) async fn replace_draft(
    tx: &mut PgConnection,
    id: Uuid,
    text: &EnglishTextV3,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM lexicon.shared_sentence_audio_asset_references WHERE sentence_id=$1 AND scope='draft'")
        .bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    insert(tx, id, None, text).await
}

pub(super) async fn insert(
    tx: &mut PgConnection,
    id: Uuid,
    publication_id: Option<Uuid>,
    text: &EnglishTextV3,
) -> Result<(), AppError> {
    let mut inserted = HashSet::new();
    for variant in crate::lexicon::v3_contract::english_text_variants(text) {
        let generated = variant
            .voice_profile
            .as_ref()
            .into_iter()
            .flat_map(|p| &p.voices)
            .filter_map(|setting| setting.audio_asset_id);
        for asset_id in variant
            .audio_assets
            .iter()
            .map(|asset| asset.id)
            .chain(generated)
        {
            if !inserted.insert(asset_id) {
                continue;
            }
            sqlx::query("INSERT INTO lexicon.shared_sentence_audio_asset_references(asset_id,sentence_id,scope,publication_id,variant_id) VALUES($1,$2,$3,$4,$5)")
                .bind(asset_id).bind(id)
                .bind(if publication_id.is_some() { "publication" } else { "draft" })
                .bind(publication_id).bind(variant.id)
                .execute(&mut *tx).await.map_err(AppError::internal)?;
        }
    }
    Ok(())
}
