use super::*;
use sha2::{Digest, Sha256};

/// 只复制真实 API 创建的空草稿；内容保存、节点投影和发布仍由受测接口生成。
pub(super) async fn seed_empty_v3_entries(
    pool: &PgPool,
    prototype: Uuid,
    prototype_surface: &str,
    surfaces: &[String],
) -> Vec<Uuid> {
    let mut tx = pool.begin().await.unwrap();
    let template: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('detection', e.detection_snapshot,
            'headwords', v.initial_headwords, 'headword_keys', v.initial_headword_keys,
            'label', p.label, 'matched_surfaces', p.matched_surfaces)
         FROM lexicon.entries e
         JOIN lexicon.v3_entry_state v ON v.entry_id=e.id
         JOIN lexicon.entry_presentation_projection p ON p.entry_id=e.id
         JOIN lexicon.entry_editor_projection editor ON editor.entry_id=e.id
         WHERE e.id=$1 AND e.content_schema_version=3 AND v.origin='native'
           AND e.revision=1 AND e.current_publication_id IS NULL
           AND editor.forms->'pos'='[]'::jsonb AND editor.meanings->'pos'='[]'::jsonb
           AND NOT EXISTS (SELECT 1 FROM lexicon.nodes n WHERE n.entry_id=e.id)",
    )
    .bind(prototype)
    .fetch_one(&mut *tx)
    .await
    .expect("批量 fixture 必须从没有内容节点的真实空草稿开始");
    let ids = surfaces.iter().map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let seeds = surfaces
        .iter()
        .zip(&ids)
        .map(|(surface, id)| {
            // 本用例的词面均为 ASCII；同步改写检测回显、初始词面和展示标签。
            let mut seed: Value = serde_json::from_str(
                &serde_json::to_string(&template)
                    .unwrap()
                    .replace(prototype_surface, surface),
            )
            .unwrap();
            seed["id"] = json!(id);
            seed["label"] = json!(
                seed["label"]
                    .as_str()
                    .unwrap()
                    .replace(&prototype.to_string()[..8], &id.to_string()[..8],)
            );
            let hash = Sha256::digest(
                serde_json::to_vec(&json!({
                    "detection": seed["detection"], "headwords": seed["headwords"],
                }))
                .unwrap(),
            );
            seed["basics_hash"] = json!(
                hash.iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            seed
        })
        .collect::<Vec<_>>();
    let seeds = json!(seeds);
    sqlx::query(
        "INSERT INTO lexicon.entries (id,content_schema_version,language,kind,revision,
            annotation,detection_snapshot,created_by_admin_id,updated_by_admin_id)
         SELECT seed.id,3,e.language,e.kind,1,e.annotation,seed.detection,
            e.created_by_admin_id,e.updated_by_admin_id
         FROM lexicon.entries e CROSS JOIN jsonb_to_recordset($2)
            AS seed(id uuid,detection jsonb) WHERE e.id=$1",
    )
    .bind(prototype)
    .bind(&seeds)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO lexicon.v3_entry_state (entry_id,content_schema_version,origin,
            first_v3_write_revision,initial_headwords,initial_headword_keys)
         SELECT seed.id,v.content_schema_version,v.origin,v.first_v3_write_revision,
            seed.headwords,seed.headword_keys
         FROM lexicon.v3_entry_state v CROSS JOIN jsonb_to_recordset($2)
            AS seed(id uuid,headwords jsonb,headword_keys text[]) WHERE v.entry_id=$1",
    )
    .bind(prototype)
    .bind(&seeds)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO lexicon.entry_editor_projection (entry_id,forms,meanings,rebuilt_revision)
         SELECT seed.id,p.forms,p.meanings,p.rebuilt_revision
         FROM lexicon.entry_editor_projection p CROSS JOIN jsonb_to_recordset($2)
            AS seed(id uuid) WHERE p.entry_id=$1",
    )
    .bind(prototype)
    .bind(&seeds)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO lexicon.entry_step_progress (entry_id,step,completed_revision,content_hash)
         SELECT seed.id,'basics',1,decode(seed.basics_hash,'hex')
         FROM jsonb_to_recordset($1) AS seed(id uuid,basics_hash text)",
    )
    .bind(&seeds)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO lexicon.entry_presentation_projection (entry_id,content_schema_version,
            source_revision,label,matched_surfaces,strategy_version)
         SELECT seed.id,p.content_schema_version,p.source_revision,seed.label,
            seed.matched_surfaces,p.strategy_version
         FROM lexicon.entry_presentation_projection p CROSS JOIN jsonb_to_recordset($2)
            AS seed(id uuid,label text,matched_surfaces text[]) WHERE p.entry_id=$1",
    )
    .bind(prototype)
    .bind(&seeds)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    ids
}
