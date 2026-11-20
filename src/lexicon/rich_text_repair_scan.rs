use anyhow::{Context, ensure};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

use super::{
    dto::{FormContentV3, FormsStepRepresentationV3},
    normalization::sha256_json,
    rich_text_repair::{RepairCounts, repair_representation, repair_shared_forms},
};
use crate::lexicon_next::model::NextDraft;

#[derive(Debug, Clone, Serialize)]
pub struct RepairScanBlocker {
    pub source: String,
    pub entry_id: Uuid,
    pub node_id: Option<Uuid>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairScanReport {
    pub mode: &'static str,
    pub scanned: usize,
    pub affected: usize,
    pub applied: usize,
    pub counts: RepairCounts,
    pub manifest_digest: String,
    pub blockers: Vec<RepairScanBlocker>,
}

#[derive(Debug)]
struct Candidate {
    source: &'static str,
    entry_id: Uuid,
    node_id: Option<Uuid>,
    old: Value,
    new: Value,
    counts: RepairCounts,
}

fn deserialize_legacy_compatible<T: DeserializeOwned>(
    value: Value,
) -> Result<T, serde_json::Error> {
    serde_json::from_value(super::voice_versions::stored_dubbing(value))
}

fn digest_bytes(value: &impl Serialize) -> anyhow::Result<String> {
    Ok(sha256_json(value)?
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Serialize)]
struct CandidateManifest {
    source: String,
    entry_id: Uuid,
    node_id: Option<Uuid>,
    old_digest: String,
    new_digest: String,
    counts: RepairCounts,
}

fn candidate_manifest(candidates: &[Candidate]) -> anyhow::Result<Vec<CandidateManifest>> {
    candidates
        .iter()
        .map(|candidate| {
            Ok(CandidateManifest {
                source: candidate.source.to_owned(),
                entry_id: candidate.entry_id,
                node_id: candidate.node_id,
                old_digest: digest_bytes(&candidate.old)?,
                new_digest: digest_bytes(&candidate.new)?,
                counts: candidate.counts.clone(),
            })
        })
        .collect()
}

fn audio_asset_ids(value: &Value) -> HashSet<Uuid> {
    fn walk(value: &Value, ids: &mut HashSet<Uuid>) {
        match value {
            Value::Object(fields) => {
                if let Some(id) = fields
                    .get("audio_asset_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok())
                {
                    ids.insert(id);
                }
                if let Some(assets) = fields.get("audio_assets").and_then(Value::as_array) {
                    for id in assets.iter().filter_map(|asset| {
                        asset
                            .get("id")
                            .and_then(Value::as_str)
                            .and_then(|value| value.parse().ok())
                    }) {
                        ids.insert(id);
                    }
                }
                for child in fields.values() {
                    walk(child, ids);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, ids);
                }
            }
            _ => {}
        }
    }
    let mut ids = HashSet::new();
    walk(value, &mut ids);
    ids
}

async fn remove_v3_removed_audio_refs(
    tx: &mut Transaction<'_, Postgres>,
    candidate: &Candidate,
) -> anyhow::Result<()> {
    let meanings: Value = sqlx::query_scalar(
        "SELECT meanings FROM lexicon.entry_editor_projection WHERE entry_id=$1",
    )
    .bind(candidate.entry_id)
    .fetch_one(&mut **tx)
    .await?;
    let old_ids = audio_asset_ids(&serde_json::json!({
        "forms": candidate.old.clone(),
        "meanings": meanings.clone()
    }));
    let new_ids = audio_asset_ids(&serde_json::json!({
        "forms": candidate.new.clone(),
        "meanings": meanings
    }));
    let removed = old_ids.difference(&new_ids).copied().collect::<Vec<_>>();
    if !removed.is_empty() {
        sqlx::query(
            "DELETE FROM lexicon.v3_audio_asset_references WHERE entry_id=$1 AND scope='draft' AND asset_id=ANY($2)",
        )
        .bind(candidate.entry_id)
        .bind(&removed)
        .execute(&mut **tx)
        .await?;
    }
    let content_hash = sha256_json(&candidate.new)?;
    sqlx::query(
        "UPDATE lexicon.entry_step_progress SET content_hash=$2 WHERE entry_id=$1 AND step='forms'",
    )
    .bind(candidate.entry_id)
    .bind(content_hash)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn remove_next_removed_audio_refs(
    tx: &mut Transaction<'_, Postgres>,
    candidate: &Candidate,
) -> anyhow::Result<()> {
    let old_ids = audio_asset_ids(&candidate.old);
    let new_ids = audio_asset_ids(&candidate.new);
    let removed = old_ids.difference(&new_ids).copied().collect::<Vec<_>>();
    if !removed.is_empty() {
        sqlx::query(
            "DELETE FROM lexicon_next.audio_asset_references WHERE entry_id=$1 AND asset_id=ANY($2)",
        )
        .bind(candidate.entry_id)
        .bind(&removed)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn candidates(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<(Vec<Candidate>, Vec<RepairScanBlocker>)> {
    let mut candidates = Vec::new();
    let mut blockers = Vec::new();

    let rows = sqlx::query_as::<_, (Uuid, Uuid, Value)>(
        "SELECT entry_id, id, content FROM lexicon.v3_form_contents ORDER BY entry_id, id",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (entry_id, content_id, old) in rows {
        let mut content: FormContentV3 = match deserialize_legacy_compatible(old.clone()) {
            Ok(content) => content,
            Err(error) => {
                blockers.push(RepairScanBlocker {
                    source: "lexicon.v3_form_contents".into(),
                    entry_id,
                    node_id: Some(content_id),
                    reason: format!("invalid_form_content:{error}"),
                });
                continue;
            }
        };
        let mut representation =
            FormsStepRepresentationV3::Shared(super::dto::SharedFormsStepContentV3 {
                forms_format: super::dto::SharedFormsFormatV3::SharedV1,
                contents: vec![content.clone()],
                pos: Vec::new(),
            });
        let counts = repair_representation(&mut representation);
        let FormsStepRepresentationV3::Shared(mut shared) = representation else {
            unreachable!("content scan always uses shared representation")
        };
        content = shared.contents.pop().expect("content scan item");
        if counts.total() == 0 {
            continue;
        }
        candidates.push(Candidate {
            source: "lexicon.v3_form_contents",
            entry_id,
            node_id: Some(content_id),
            old,
            new: serde_json::to_value(content)?,
            counts,
        });
    }

    let rows = sqlx::query_as::<_, (Uuid, Value)>(
        "SELECT entry_id, forms FROM lexicon.entry_editor_projection ORDER BY entry_id",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (entry_id, old) in rows {
        let mut representation: FormsStepRepresentationV3 =
            match deserialize_legacy_compatible(old.clone()) {
                Ok(value) => value,
                Err(error) => {
                    blockers.push(RepairScanBlocker {
                        source: "lexicon.entry_editor_projection".into(),
                        entry_id,
                        node_id: None,
                        reason: format!("invalid_forms_projection:{error}"),
                    });
                    continue;
                }
            };
        let counts = repair_representation(&mut representation);
        if counts.total() == 0 {
            continue;
        }
        candidates.push(Candidate {
            source: "lexicon.entry_editor_projection",
            entry_id,
            node_id: None,
            old,
            new: serde_json::to_value(representation)?,
            counts,
        });
    }

    let rows = sqlx::query_as::<_, (Uuid, Value)>(
        r#"
        SELECT entries.id, revisions.document
        FROM lexicon_next.entries entries
        JOIN lexicon_next.draft_revisions revisions
          ON revisions.entry_id = entries.id
         AND revisions.revision = entries.revision
        ORDER BY entries.id
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    for (entry_id, old) in rows {
        let mut draft: NextDraft = match deserialize_legacy_compatible(old.clone()) {
            Ok(value) => value,
            Err(error) => {
                blockers.push(RepairScanBlocker {
                    source: "lexicon_next.draft_revisions".into(),
                    entry_id,
                    node_id: None,
                    reason: format!("invalid_next_draft:{error}"),
                });
                continue;
            }
        };
        let Some(editor) = draft.form_editor.as_mut() else {
            continue;
        };
        let counts = repair_shared_forms(editor);
        if counts.total() == 0 {
            continue;
        }
        candidates.push(Candidate {
            source: "lexicon_next.draft_revisions",
            entry_id,
            node_id: None,
            old,
            new: serde_json::to_value(draft)?,
            counts,
        });
    }

    Ok((candidates, blockers))
}

pub async fn dry_run(pool: &PgPool) -> anyhow::Result<RepairScanReport> {
    run(pool, None).await
}

pub async fn apply(
    pool: &PgPool,
    expected_manifest_digest: &str,
) -> anyhow::Result<RepairScanReport> {
    run(pool, Some(expected_manifest_digest)).await
}

async fn run(
    pool: &PgPool,
    expected_manifest_digest: Option<&str>,
) -> anyhow::Result<RepairScanReport> {
    let apply = expected_manifest_digest.is_some();
    let mut tx = pool.begin().await?;
    super::rich_text_repair::lock_transaction(&mut tx).await?;
    let (candidates, blockers) = candidates(&mut tx).await?;
    let manifest = candidate_manifest(&candidates)?;
    let manifest_digest = digest_bytes(&manifest)?;
    if let Some(expected) = expected_manifest_digest {
        ensure!(
            manifest_digest == expected,
            "rich_text_repair_manifest_mismatch:expected={expected}:actual={manifest_digest}"
        );
    }
    if apply && !blockers.is_empty() {
        anyhow::bail!(
            "rich_text_repair_blocked:{}",
            serde_json::to_string(&blockers)?
        );
    }

    let mut counts = RepairCounts::default();
    for candidate in &candidates {
        counts.add_assign(&candidate.counts);
    }
    let mut applied = 0;
    if apply {
        for candidate in &candidates {
            let result = match candidate.source {
                "lexicon.v3_form_contents" => {
                    sqlx::query(
                        "UPDATE lexicon.v3_form_contents SET content=$3 WHERE entry_id=$1 AND id=$2 AND content=$4",
                    )
                    .bind(candidate.entry_id)
                    .bind(candidate.node_id.context("missing content id")?)
                    .bind(&candidate.new)
                    .bind(&candidate.old)
                    .execute(&mut *tx)
                    .await?
                }
                "lexicon.entry_editor_projection" => {
                    sqlx::query(
                        "UPDATE lexicon.entry_editor_projection SET forms=$2, updated_at=clock_timestamp() WHERE entry_id=$1 AND forms=$3",
                    )
                    .bind(candidate.entry_id)
                    .bind(&candidate.new)
                    .bind(&candidate.old)
                    .execute(&mut *tx)
                    .await?
                }
                "lexicon_next.draft_revisions" => {
                    sqlx::query(
                        r#"
                        UPDATE lexicon_next.draft_revisions revisions
                        SET document=$2
                        FROM lexicon_next.entries entries
                        WHERE revisions.entry_id=$1
                          AND revisions.entry_id=entries.id
                          AND revisions.revision=entries.revision
                          AND revisions.document=$3
                        "#,
                    )
                    .bind(candidate.entry_id)
                    .bind(&candidate.new)
                    .bind(&candidate.old)
                    .execute(&mut *tx)
                    .await?
                }
                source => anyhow::bail!("unknown repair source: {source}"),
            };
            ensure!(
                result.rows_affected() == 1,
                "rich_text_repair_candidate_changed:{}:{}",
                candidate.source,
                candidate.entry_id
            );
            match candidate.source {
                "lexicon.entry_editor_projection" => {
                    remove_v3_removed_audio_refs(&mut tx, candidate).await?;
                }
                "lexicon_next.draft_revisions" => {
                    remove_next_removed_audio_refs(&mut tx, candidate).await?;
                }
                _ => {}
            }
            applied += 1;
        }
    }
    tx.commit().await?;
    Ok(RepairScanReport {
        mode: if apply { "apply" } else { "dry_run" },
        scanned: candidates.len() + blockers.len(),
        affected: candidates.len(),
        applied,
        counts,
        manifest_digest,
        blockers,
    })
}

#[cfg(test)]
mod tests {
    use super::deserialize_legacy_compatible;
    use crate::lexicon::dto::{DubbingAreaV3, VoiceProfileModeV3, VoiceProfileV3};
    use serde_json::json;

    #[test]
    fn scanner_accepts_legacy_dubbing_shape_before_repair() {
        let legacy = json!({
            "voices": [],
            "dubbing": {
                "mode": "local",
                "area": "word",
                "recommendation": {"pair_id": "original"}
            }
        });

        let profile: VoiceProfileV3 = deserialize_legacy_compatible(legacy).unwrap();

        assert_eq!(profile.area(), Some(DubbingAreaV3::Word));
        assert_eq!(profile.mode("gb"), Some(VoiceProfileModeV3::Local));
        assert_eq!(profile.legacy_dubbing.as_deref().unwrap()["mode"], "local");
        assert_eq!(profile.legacy_dubbing.as_deref().unwrap()["area"], "word");
    }
}
