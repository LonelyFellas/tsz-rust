use axum::{extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    admin::{AdminAuth, permissions},
    api::ApiJson,
    error::{AppError, ErrorCode},
    lexicon_next::{language, repository},
    state::AppState,
    test_resources,
};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DialectPairFixtureRequest {
    #[schema(
        min_length = 8,
        max_length = 80,
        pattern = "^[A-Za-z0-9][A-Za-z0-9-]*$"
    )]
    pub headword_prefix: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DialectPairFixtureEntry {
    pub entry_id: Uuid,
    pub sense_id: Uuid,
    pub headword: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DialectPairFixtureResponse {
    pub run: String,
    pub british: DialectPairFixtureEntry,
    pub american: DialectPairFixtureEntry,
}

fn fixture_draft(headword: &str, sense: Uuid) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "headword": headword,
        "forms": [],
        "senses": [{
            "id": sense,
            "part_of_speech": "noun",
            "definitions": [],
            "equivalents": []
        }]
    })
}

#[utoipa::path(
    post,
    path="/api/v1/admin/lexicon-next/test-fixtures/dialect-pair",
    tag="admin-lexicon-next-test-fixtures",
    security(("bearer_auth"=[])),
    params(("X-TSZ-Test-Run"=String, Header, description="仅 tshb-test 白名单账号可用的独占测试批次")),
    request_body=DialectPairFixtureRequest,
    responses((status=200, body=DialectPairFixtureResponse), (status=400), (status=401), (status=403), (status=503))
)]
pub async fn create(
    State(state): State<AppState>,
    auth: AdminAuth,
    headers: HeaderMap,
    ApiJson(input): ApiJson<DialectPairFixtureRequest>,
) -> Result<axum::Json<DialectPairFixtureResponse>, AppError> {
    let (state, run) = test_resources::scoped_state(&state, &headers, auth.subject, true)?;
    let run = run.ok_or_else(|| {
        AppError::forbidden(ErrorCode::Forbidden, "test resource run is required")
    })?;
    if input.headword_prefix.len() < 8
        || input.headword_prefix.len() > 80
        || !input
            .headword_prefix
            .bytes()
            .enumerate()
            .all(|(index, value)| value.is_ascii_alphanumeric() || (index > 0 && value == b'-'))
    {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "invalid dialect fixture headword prefix",
        ));
    }
    let british_headword = format!("{}-gb", input.headword_prefix);
    let american_headword = format!("{}-us", input.headword_prefix);
    language::text(&british_headword, 200)?;
    language::text(&american_headword, 200)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, &auth, "words.create").await?;
    repository::lock_list_writers(&mut tx).await?;
    test_resources::lock_run(&mut tx, auth.subject, &run).await?;
    let british_entry = Uuid::now_v7();
    let american_entry = Uuid::now_v7();
    let british_sense = Uuid::now_v7();
    let american_sense = Uuid::now_v7();
    for (entry, sense, headword) in [
        (british_entry, british_sense, &british_headword),
        (american_entry, american_sense, &american_headword),
    ] {
        sqlx::query("INSERT INTO lexicon_next.entries(id,target_language,medium_locale,kind,headword,owner_id,revision) VALUES($1,'en','zh-Hans','word',$2,$3,1)")
            .bind(entry)
            .bind(headword)
            .bind(auth.subject)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
        test_resources::register_entry(&mut tx, &run, auth.subject, entry).await?;
        sqlx::query(
            "INSERT INTO lexicon_next.nodes(id,entry_id,kind,parent_id) VALUES($1,$2,'sense',NULL)",
        )
        .bind(sense)
        .bind(entry)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        sqlx::query(
            "INSERT INTO lexicon_next.senses(id,part_of_speech,sort_order) VALUES($1,'noun',0)",
        )
        .bind(sense)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let draft =
            serde_json::from_value(fixture_draft(headword, sense)).map_err(AppError::internal)?;
        repository::snapshot(&mut tx, entry, 1, auth.subject, &draft, &[]).await?;
    }
    sqlx::query("INSERT INTO lexicon_next.dialect_pairs(id,gb_entry,gb_sense,us_entry,us_sense) VALUES($1,$2,$3,$4,$5)")
        .bind(Uuid::now_v7())
        .bind(british_entry)
        .bind(british_sense)
        .bind(american_entry)
        .bind(american_sense)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    for (entry, sense, usage) in [
        (british_entry, british_sense, "en-GB"),
        (american_entry, american_sense, "en-US"),
    ] {
        let pair_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM lexicon_next.dialect_pairs WHERE gb_entry=$1 OR us_entry=$1",
        )
        .bind(entry)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        let bindings = serde_json::json!([{
            "sense_id": sense,
            "usage": usage,
            "no_equivalent": false,
            "note": "",
            "pair_id": pair_id
        }]);
        sqlx::query("UPDATE lexicon_next.draft_revisions SET dialect_senses=$3 WHERE entry_id=$1 AND revision=1")
            .bind(entry)
            .bind(1_i64)
            .bind(bindings)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(axum::Json(DialectPairFixtureResponse {
        run,
        british: DialectPairFixtureEntry {
            entry_id: british_entry,
            sense_id: british_sense,
            headword: british_headword,
        },
        american: DialectPairFixtureEntry {
            entry_id: american_entry,
            sense_id: american_sense,
            headword: american_headword,
        },
    }))
}
