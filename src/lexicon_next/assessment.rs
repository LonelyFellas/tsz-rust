use super::{
    assessment_document, dialect_pairs, language, publication_model::NextPublicationSnapshot,
    repository,
};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath},
    error::{AppError, ErrorCode},
    lexicon::{dto::WordEntryKindV3, normalization::normalize_headword},
    platform::text::key_v1,
    state::AppState,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextAssessmentState {
    #[schema(required)]
    pub publication_id: Option<Uuid>,
    pub lifecycle_revision: i64,
    pub version: i64,
    pub approved: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewNextPublication {
    pub publication_id: Uuid,
    pub lifecycle_revision: i64,
    pub expected_version: i64,
    pub approved: bool,
}

async fn status(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<NextAssessmentState, AppError> {
    let row:Option<(Option<Uuid>,i64,i64,bool)>=sqlx::query_as(
        "SELECT p.id,e.lifecycle_revision,coalesce(r.version,0),coalesce(e.status='published' AND r.approved AND r.publication_id=p.id AND r.lifecycle_revision=e.lifecycle_revision,false)
         FROM lexicon_next.entries e LEFT JOIN LATERAL (SELECT id FROM lexicon_next.publications WHERE entry_id=e.id ORDER BY lifecycle_revision DESC LIMIT 1)p ON true
         LEFT JOIN LATERAL (SELECT version,approved,publication_id,lifecycle_revision FROM learning_source.reviews WHERE entry_id=e.id ORDER BY version DESC LIMIT 1)r ON true WHERE e.id=$1"
    ).bind(id).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    let (publication_id, lifecycle_revision, version, approved) =
        row.ok_or_else(|| AppError::not_found("词条不存在"))?;
    Ok(NextAssessmentState {
        publication_id,
        lifecycle_revision,
        version,
        approved,
    })
}

#[utoipa::path(get,path="/api/v1/admin/lexicon-next/entries/{id}/assessment",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),responses((status=200,body=NextAssessmentState),(status=403),(status=404)))]
pub async fn read(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NextAssessmentState>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("words.access")?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    Ok(Json(status(&mut tx, id).await?))
}

#[utoipa::path(put,path="/api/v1/admin/lexicon-next/entries/{id}/assessment",tag="admin-lexicon-next",security(("bearer_auth"=[])),params(("id"=Uuid,Path)),request_body=ReviewNextPublication,responses((status=200,body=NextAssessmentState),(status=403),(status=404),(status=409),(status=422)))]
pub async fn decide(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ReviewNextPublication>,
) -> Result<Json<NextAssessmentState>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "words.review_assessment").await?;
    repository::lock_list_writers(&mut tx).await?;
    let (published,kind,medium):(bool,String,String)=sqlx::query_as("SELECT status='published',kind,medium_locale FROM lexicon_next.entries WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(||AppError::not_found("词条不存在"))?;
    let current = status(&mut tx, id).await?;
    if !published
        || current.publication_id != Some(input.publication_id)
        || current.lifecycle_revision != input.lifecycle_revision
        || current.version != input.expected_version
    {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "审核依据已变化，请重新读取已发布版本",
        ));
    }
    if current.approved == input.approved {
        return Ok(Json(current));
    }
    if input.approved {
        let (revision, mut raw): (i64, serde_json::Value) = sqlx::query_as(
            "SELECT revision,snapshot FROM lexicon_next.publications WHERE id=$1 AND entry_id=$2",
        )
        .bind(input.publication_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if let Some(draft) = raw.get_mut("draft") {
            super::model::strip_legacy_creation_detection(draft);
        }
        let snapshot: NextPublicationSnapshot =
            serde_json::from_value(raw).map_err(AppError::internal)?;
        let pairs = dialect_pairs::project(&mut tx, id, revision, true).await?;
        let doc = assessment_document::project(
            id,
            match kind.as_str() {
                "word" => WordEntryKindV3::Word,
                "phrase" => WordEntryKindV3::Phrase,
                _ => return Err(language::invalid()),
            },
            &medium,
            snapshot,
            &pairs,
        )?;
        let mut surfaces = std::collections::BTreeSet::new();
        for variant in doc
            .forms
            .pos
            .iter()
            .flat_map(|p| &p.forms)
            .flat_map(|f| f.regional_variants.variants())
        {
            if let Ok(form) = normalize_headword(variant.spelling) {
                surfaces.insert(form.key);
            }
        }
        sqlx::query("INSERT INTO learning_source.identities(id,next_id) VALUES ($1,$1) ON CONFLICT(next_id) DO NOTHING").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO learning_source.reviewed_publications(id,entry_id,snapshot) VALUES ($1,$2,$3) ON CONFLICT(id) DO NOTHING")
            .bind(input.publication_id).bind(id).bind(serde_json::to_value(doc).map_err(AppError::internal)?).execute(&mut *tx).await.map_err(AppError::internal)?;
        sqlx::query("INSERT INTO learning_source.next_surfaces SELECT $1,unnest($2::text[]) ON CONFLICT DO NOTHING").bind(input.publication_id).bind(surfaces.into_iter().collect::<Vec<_>>()).execute(&mut *tx).await.map_err(AppError::internal)?;
    }
    sqlx::query("INSERT INTO learning_source.reviews(entry_id,publication_id,lifecycle_revision,approved,actor_id) VALUES($1,$2,$3,$4,$5)")
        .bind(id).bind(input.publication_id).bind(current.lifecycle_revision).bind(input.approved).bind(auth.subject).execute(&mut *tx).await.map_err(AppError::internal)?;
    if input.approved {
        index_candidates(&mut tx, id, input.publication_id).await?;
    }
    let result = status(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(result))
}

async fn index_candidates(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    publication: Uuid,
) -> Result<(), AppError> {
    for profile in ["BrE", "AmE"] {
        let candidates = crate::question_bank::source::candidates(tx, &[id], "C2", profile).await?;
        for chunk in candidates.chunks(256) {
            let rows:Vec<_>=chunk.iter().map(|c|serde_json::json!({
                "pos_id":c.source.pos_id,"sense_id":c.source.sense_id,"definition_id":c.source.definition_id,"form_id":c.source.form_id,
                "level":c.level,"part_of_speech":c.part_of_speech,"spelling":c.spelling,"definition":c.definition,
                "accepted_keys":c.accepted_spellings.iter().map(|s|key_v1(s)).collect::<Vec<_>>(),
                "definition_keys":c.other_definitions.iter().chain(std::iter::once(&c.definition)).map(|s|key_v1(s)).collect::<Vec<_>>()
            })).collect();
            sqlx::query("INSERT INTO learning_source.next_choice_candidates(publication_id,entry_id,pos_id,sense_id,definition_id,form_id,answer_profile,level,part_of_speech,spelling,definition,accepted_keys,definition_keys)
                SELECT $1,$2,pos_id,sense_id,definition_id,form_id,$3,level,part_of_speech,spelling,definition,accepted_keys,definition_keys FROM jsonb_to_recordset($4) AS r(pos_id uuid,sense_id uuid,definition_id uuid,form_id uuid,level text,part_of_speech text,spelling text,definition text,accepted_keys text[],definition_keys text[]) ON CONFLICT DO NOTHING")
                .bind(publication).bind(id).bind(profile).bind(sqlx::types::Json(rows)).execute(&mut **tx).await.map_err(AppError::internal)?;
        }
    }
    sqlx::query("SELECT dictionary.refresh_choice_semantics(dataset_id,source_key) FROM (SELECT DISTINCT d.dataset_id,d.source_key FROM learning_source.next_choice_candidates c JOIN dictionary.entry_contents d ON d.normalized_term=lexicon.question_candidate_key_v1(c.spelling) WHERE c.publication_id=$1) s")
        .bind(publication).execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
