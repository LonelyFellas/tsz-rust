//! Management of language drafts without projecting them into the V1 content schema.
use super::{language, list_model::*};
use crate::{
    admin::{AdminAuth, permissions},
    error::{AppError, ErrorCode},
};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

const SAME_NAME: &str = r#"e.status<>'archived' AND other.status<>'archived' AND other.target_language=e.target_language AND other.medium_locale=e.medium_locale AND other.kind=e.kind AND other.id<>e.id
 AND EXISTS (
 WITH current_forms AS NOT MATERIALIZED (
   SELECT n.entry_id,f.locale,f.text FROM lexicon_next.nodes n JOIN lexicon_next.forms f ON f.id=n.id
 )
 SELECT 1 FROM lexicon_next.entries target
 LEFT JOIN current_forms f ON f.entry_id=target.id
 LEFT JOIN current_forms oform ON oform.entry_id=other.id
 WHERE target.id=e.id
   AND lower(COALESCE(f.text,target.headword))=lower(COALESCE(oform.text,other.headword))
   AND (f.locale IS NULL OR oform.locale IS NULL OR oform.locale=f.locale OR oform.locale='en' OR f.locale='en'))"#;

pub(super) fn select(medium_locale: Option<&str>) -> QueryBuilder<Postgres> {
    let mut sql = QueryBuilder::new(format!(
        r#"SELECT e.id,e.target_language,e.kind,e.headword,e.owner_id,e.revision,e.lifecycle_revision,e.status,
    e.created_at,e.updated_at,creator.display_name AS created_by_name,editor.display_name AS updated_by_name,
    e.annotation,e.annotation_revision,
    EXISTS(SELECT 1 FROM lexicon_next.entries other WHERE {SAME_NAME}) AS annotation_visible,
    ARRAY(SELECT DISTINCT s.part_of_speech FROM lexicon_next.senses s JOIN lexicon_next.nodes n ON n.id=s.id
      WHERE n.entry_id=e.id AND s.part_of_speech IS NOT NULL
      UNION SELECT p->>'pos' FROM jsonb_array_elements(COALESCE(current.document->'form_editor'->'pos','[]'::jsonb)) p ORDER BY 1) AS part_of_speech,
    COALESCE((SELECT d.text FROM lexicon_next.definitions d JOIN lexicon_next.nodes n ON n.id=d.id WHERE n.entry_id=e.id
      AND ("#
    ));
    sql.push_bind(medium_locale).push("::text IS NULL OR d.locale=").push_bind(medium_locale).push(r#")
      ORDER BY CASE d.locale WHEN 'zh-Hans' THEN 0 WHEN 'zh-Hant' THEN 1 ELSE 2 END,d.sort_order,d.id LIMIT 1),
    (SELECT q.text FROM lexicon_next.equivalents q JOIN lexicon_next.nodes n ON n.id=q.id WHERE n.entry_id=e.id
      AND ("#).push_bind(medium_locale).push("::text IS NULL OR q.locale=").push_bind(medium_locale).push(r#")
      ORDER BY CASE q.locale WHEN 'zh-Hans' THEN 0 WHEN 'zh-Hant' THEN 1 ELSE 2 END,q.sort_order,q.id LIMIT 1)) AS gloss,
    COALESCE((SELECT jsonb_agg(jsonb_build_object('id',f.id,'locale',f.locale,'text',f.text) ORDER BY f.sort_order,f.id)
      FROM lexicon_next.forms f JOIN lexicon_next.nodes n ON n.id=f.id WHERE n.entry_id=e.id),'[]'::jsonb) AS forms
    FROM lexicon_next.entries e JOIN admins creator ON creator.id=e.owner_id
    JOIN lexicon_next.draft_revisions current ON current.entry_id=e.id AND current.revision=e.revision
    JOIN admins editor ON editor.id=current.actor_id"#);
    sql
}
pub(super) fn filter(sql: &mut QueryBuilder<Postgres>, query: &NextWorkspaceQuery) {
    sql.push(" WHERE ");
    if let Some(status) = &query.status {
        sql.push("e.status=").push_bind(status);
    } else {
        sql.push("e.status<>'archived'");
    }
    if let Some(language) = &query.target_language {
        sql.push(" AND e.target_language=").push_bind(language);
    }
    if let Some(locale) = &query.medium_locale {
        sql.push(" AND e.medium_locale=").push_bind(locale);
    }
    if let Some(word) = query
        .headword
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sql.push(" AND (position(lower(").push_bind(word).push(") in lower(e.headword))>0 OR EXISTS(SELECT 1 FROM lexicon_next.forms f JOIN lexicon_next.nodes n ON n.id=f.id WHERE n.entry_id=e.id AND position(lower(").push_bind(word).push(") in lower(f.text))>0))");
    }
    if let Some(gloss) = query
        .gloss
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sql.push(" AND (EXISTS(SELECT 1 FROM lexicon_next.definitions d JOIN lexicon_next.nodes n ON n.id=d.id WHERE n.entry_id=e.id");
        if let Some(locale) = &query.medium_locale {
            sql.push(" AND d.locale=").push_bind(locale);
        }
        sql.push(" AND position(lower(").push_bind(gloss).push(") in lower(d.text))>0) OR EXISTS(SELECT 1 FROM lexicon_next.equivalents q JOIN lexicon_next.nodes n ON n.id=q.id WHERE n.entry_id=e.id");
        if let Some(locale) = &query.medium_locale {
            sql.push(" AND q.locale=").push_bind(locale);
        }
        sql.push(" AND position(lower(")
            .push_bind(gloss)
            .push(") in lower(q.text))>0))");
    }
    if let Some(kind) = &query.kind {
        sql.push(" AND e.kind=").push_bind(kind);
    }
    if let Some(pos) = &query.pos {
        sql.push(" AND (EXISTS(SELECT 1 FROM lexicon_next.senses s JOIN lexicon_next.nodes n ON n.id=s.id WHERE n.entry_id=e.id AND s.part_of_speech=").push_bind(pos).push(") OR EXISTS(SELECT 1 FROM lexicon_next.draft_revisions r, jsonb_array_elements(COALESCE(r.document->'form_editor'->'pos','[]'::jsonb)) p WHERE r.entry_id=e.id AND r.revision=e.revision AND p->>'pos'=").push_bind(pos).push("))");
    }
    // Language foundation drafts have no CEFR classification yet. Never infer one.
    if query.level.is_some() {
        sql.push(" AND FALSE");
    }
    if let Some(owner) = query.created_by {
        sql.push(" AND e.owner_id=").push_bind(owner);
    }
    if let Some(from) = query.created_from {
        sql.push(" AND e.created_at>=").push_bind(from);
    }
    if let Some(to) = query.created_to {
        sql.push(" AND e.created_at<").push_bind(to);
    }
}
pub(super) fn validate(query: &NextWorkspaceQuery) -> Result<(), AppError> {
    if let Some(target) = &query.target_language {
        // Query historical languages too, without enabling new-language draft creation.
        if !(2..=3).contains(&target.len()) || !target.bytes().all(|c| c.is_ascii_lowercase()) {
            return Err(language::invalid());
        }
    }
    if query
        .medium_locale
        .as_deref()
        .is_some_and(|locale| !["zh-Hans", "zh-Hant"].contains(&locale))
    {
        return Err(language::unavailable());
    }
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    if page == 0
        || !(1..=100).contains(&size)
        || query
            .headword
            .as_ref()
            .is_some_and(|v| v.chars().count() > 200)
        || query
            .gloss
            .as_ref()
            .is_some_and(|v| v.chars().count() > 5000)
        || query
            .kind
            .as_deref()
            .is_some_and(|v| !["word", "phrase"].contains(&v))
        || query
            .status
            .as_deref()
            .is_some_and(|v| !["draft", "published", "archived"].contains(&v))
        || query
            .pos
            .as_deref()
            .is_some_and(|v| !language::POS.contains(&v))
        || query
            .level
            .as_deref()
            .is_some_and(|v| !["A1", "A2", "B1", "B2", "C1", "C2"].contains(&v))
        || query
            .created_from
            .zip(query.created_to)
            .is_some_and(|(a, b)| a >= b)
    {
        return Err(language::invalid());
    }
    Ok(())
}
pub async fn list(pool: &PgPool, query: NextWorkspaceQuery) -> Result<NextWorkspaceList, AppError> {
    validate(&query)?;
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut count = QueryBuilder::new("SELECT count(*) FROM lexicon_next.entries e");
    filter(&mut count, &query);
    let total = count
        .build_query_scalar()
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let mut rows = select(query.medium_locale.as_deref());
    filter(&mut rows, &query);
    rows.push(" ORDER BY e.created_at DESC,e.id DESC LIMIT ")
        .push_bind(i64::from(size))
        .push(" OFFSET ")
        .push_bind(i64::from(page - 1) * i64::from(size));
    let items = rows
        .build_query_as()
        .fetch_all(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextWorkspaceList {
        items,
        pagination: crate::api::page_meta(page, size, total),
    })
}
pub async fn get(pool: &PgPool, id: Uuid) -> Result<NextWorkspaceEntry, AppError> {
    select(None)
        .push(" WHERE e.id=")
        .push_bind(id)
        .build_query_as()
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("lexicon-next entry not found"))
}
pub async fn stats(
    pool: &PgPool,
    query: NextWorkspaceQuery,
) -> Result<NextWorkspaceStats, AppError> {
    validate(&query)?;
    let mut sql = QueryBuilder::new(
        r#"SELECT count(*) AS total,
        count(*) FILTER(WHERE (e.created_at AT TIME ZONE 'Asia/Shanghai')::date=(now() AT TIME ZONE 'Asia/Shanghai')::date) AS today,
        count(*) FILTER(WHERE date_trunc('month',e.created_at AT TIME ZONE 'Asia/Shanghai')=date_trunc('month',now() AT TIME ZONE 'Asia/Shanghai')) AS month
        FROM lexicon_next.entries e"#,
    );
    filter(&mut sql, &query);
    sql.build_query_as()
        .fetch_one(pool)
        .await
        .map_err(AppError::internal)
}
pub async fn creators(
    pool: &PgPool,
    query: NextWorkspaceQuery,
) -> Result<Vec<NextWorkspaceCreator>, AppError> {
    validate(&query)?;
    let mut sql = QueryBuilder::new(
        "SELECT DISTINCT a.id,a.display_name FROM admins a JOIN lexicon_next.entries e ON e.owner_id=a.id",
    );
    filter(&mut sql, &query);
    sql.push(" ORDER BY a.display_name,a.id")
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)
}
pub async fn annotation(
    pool: &PgPool,
    auth: &AdminAuth,
    id: Uuid,
    input: NextAnnotationInput,
) -> Result<NextAnnotationResult, AppError> {
    let annotation = input
        .annotation
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if input.base_annotation_revision < 1
        || annotation.is_some_and(|v| v.len() > 20 || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(language::invalid());
    }
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, "words.edit").await?;
    // Same lock as create/save: membership and annotations cannot change while checked.
    super::repository::lock_list_writers(&mut tx).await?;
    let (owner, version, status): (Uuid,i64,String) = sqlx::query_as("SELECT owner_id,annotation_revision,status FROM lexicon_next.entries WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(|| AppError::not_found("entry not found"))?;
    permissions::lock(&mut tx, auth.subject)
        .await?
        .require_owned_action("words.edit", owner)?;
    if status == "archived" {
        return Err(AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "归档词条不能修改标注",
        ));
    }
    if version != input.base_annotation_revision {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_annotation_revision"),
            "标注已变化，请重新打开后确认",
        ));
    }
    let others: Vec<Option<String>> = QueryBuilder::<Postgres>::new(
        "SELECT other.annotation FROM lexicon_next.entries e JOIN lexicon_next.entries other ON ",
    )
    .push(SAME_NAME)
    .push(" WHERE e.id=")
    .push_bind(id)
    .build_query_scalar()
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if !others.is_empty()
        && (annotation.is_none() || others.iter().any(|v| v.as_deref() == annotation))
    {
        return Err(AppError::conflict(
            ErrorCode::AnnotationConflict,
            Some("annotation"),
            "同名词条的标注必须填写，且不能重复",
        ));
    }
    let annotation_revision = sqlx::query_scalar("UPDATE lexicon_next.entries SET annotation=$2,annotation_revision=annotation_revision+CASE WHEN annotation IS DISTINCT FROM $2 THEN 1 ELSE 0 END WHERE id=$1 RETURNING annotation_revision")
        .bind(id).bind(annotation).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(NextAnnotationResult {
        entry_id: id,
        annotation: annotation.map(str::to_owned),
        annotation_revision,
    })
}
pub async fn command(
    pool: &PgPool,
    auth: &AdminAuth,
    mut input: NextListCommand,
) -> Result<NextListCommandResult, AppError> {
    if input.items.is_empty()
        || input.items.len() > 50
        || input.request_id.is_nil()
        || input
            .items
            .iter()
            .any(|v| v.base_revision < 1 || v.base_lifecycle_revision < 1)
    {
        return Err(language::invalid());
    }
    input.items.sort_by_key(|v| v.entry_id);
    if input
        .items
        .windows(2)
        .any(|v| v[0].entry_id == v[1].entry_id)
    {
        return Err(language::invalid());
    }
    let permission = match input.action {
        NextListAction::Archive => "words.archive",
        NextListAction::Restore => "words.restore",
        NextListAction::Delete => "words.access",
    };
    let request = serde_json::to_value(&input).map_err(AppError::internal)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    permissions::lock_session(&mut tx, auth, permission).await?;
    let authorization = permissions::lock(&mut tx, auth.subject).await?;
    if matches!(input.action, NextListAction::Delete) {
        authorization.require_super_admin()?;
    }
    super::repository::lock_list_writers(&mut tx).await?;
    let existing: Option<(serde_json::Value, serde_json::Value)> = sqlx::query_as(
        "SELECT request,result FROM lexicon_next.list_commands WHERE actor_id=$1 AND request_id=$2",
    )
    .bind(auth.subject)
    .bind(input.request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if let Some((old, result)) = existing {
        if old != request {
            return Err(AppError::conflict(
                ErrorCode::RevisionConflict,
                None,
                "请求标识已用于其他操作",
            ));
        }
        // Delete is super-only; all other ownership checks still apply on replay.
        if !matches!(input.action, NextListAction::Delete) {
            for target in &input.items {
                let owner: Uuid =
                    sqlx::query_scalar("SELECT owner_id FROM lexicon_next.entries WHERE id=$1")
                        .bind(target.entry_id)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(AppError::internal)?
                        .ok_or_else(|| AppError::not_found("entry not found"))?;
                authorization.require_owned_action(permission, owner)?;
            }
        }
        return serde_json::from_value(result).map_err(AppError::internal);
    }
    for target in &input.items {
        let (owner,revision,lifecycle,status): (Uuid,i64,i64,String) = sqlx::query_as("SELECT owner_id,revision,lifecycle_revision,status FROM lexicon_next.entries WHERE id=$1 FOR UPDATE")
            .bind(target.entry_id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(|| AppError::not_found("entry not found"))?;
        if !matches!(input.action, NextListAction::Delete) {
            authorization.require_owned_action(permission, owner)?;
        }
        if revision != target.base_revision || lifecycle != target.base_lifecycle_revision {
            return Err(AppError::conflict(
                ErrorCode::RevisionConflict,
                None,
                "词条已变化，请刷新列表并重新选择",
            ));
        }
        let required = if matches!(input.action, NextListAction::Archive) {
            "draft"
        } else {
            "archived"
        };
        if status != required {
            return Err(AppError::conflict(
                ErrorCode::RevisionConflict,
                None,
                "词条状态已变化，请刷新列表",
            ));
        }
        match input.action {
            NextListAction::Archive | NextListAction::Restore => {
                let next = if matches!(input.action, NextListAction::Archive) {
                    "archived"
                } else {
                    "draft"
                };
                sqlx::query("UPDATE lexicon_next.entries SET status=$2,lifecycle_revision=lifecycle_revision+1 WHERE id=$1")
                    .bind(target.entry_id).bind(next).execute(&mut *tx).await.map_err(AppError::internal)?;
            }
            NextListAction::Delete => {
                // Only new-language drafts exist here; FK checks remain enabled.
                for sql in [
                    "DELETE FROM lexicon_next.equivalents WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
                    "DELETE FROM lexicon_next.definitions WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
                    "DELETE FROM lexicon_next.senses WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
                    "DELETE FROM lexicon_next.forms WHERE id IN (SELECT id FROM lexicon_next.nodes WHERE entry_id=$1)",
                ] {
                    sqlx::query(sql)
                        .bind(target.entry_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(AppError::internal)?;
                }
                sqlx::query("DELETE FROM lexicon_next.nodes WHERE entry_id=$1")
                    .bind(target.entry_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
                sqlx::query("DELETE FROM lexicon_next.draft_revisions WHERE entry_id=$1")
                    .bind(target.entry_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
                sqlx::query("DELETE FROM lexicon_next.entries WHERE id=$1")
                    .bind(target.entry_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(AppError::internal)?;
            }
        }
    }
    let result = NextListCommandResult {
        entry_ids: input.items.iter().map(|v| v.entry_id).collect(),
    };
    sqlx::query("INSERT INTO lexicon_next.list_commands(actor_id,request_id,request,result) VALUES($1,$2,$3,$4)")
        .bind(auth.subject).bind(input.request_id).bind(request).bind(serde_json::to_value(&result).map_err(AppError::internal)?)
        .execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
