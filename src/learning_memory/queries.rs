use super::{dto::*, *};
use crate::{
    auth::extract::AuthUser,
    learning_tasks::repository::{conflict, invalid, learner},
    state::AppState,
};
use sqlx::PgPool;
type MemoryListRow = (Uuid, Option<String>, String, DateTime<Utc>, bool, bool, i64);
const CONTENT_VISIBLE: &str = "(m.content_available OR m.checked_publication_id IS DISTINCT FROM src.publication_id OR m.checked_cefr_level IS DISTINCT FROM $6)";
const READY_AT: &str = "GREATEST(m.due_at,COALESCE(m.last_feedback_at+interval '10 minutes',m.due_at),CASE WHEN m.stage='relearning' AND m.retest_day=((clock_timestamp() AT TIME ZONE 'Asia/Shanghai')-interval '4 hours')::date AND m.retest_issued>=2 THEN ((m.retest_day+1)::timestamp+interval '4 hours') AT TIME ZONE 'Asia/Shanghai' ELSE m.due_at END)";

// A memory is not a grant. Every query checks a registered, still-current source.
pub(super) const SOURCE: &str = r#"
SELECT s.wordlist_id,s.membership_id,s.public_generation,s.entry_archive_generation,
       e.current_publication_id AS publication_id,p.snapshot
FROM learning_memory_sources s
JOIN wordlists w ON w.id=s.wordlist_id
JOIN wordlist_items i ON i.wordlist_id=w.id AND i.entry_id=m.entry_id AND i.learning_membership_id=s.membership_id
JOIN lexicon.entries e ON e.id=m.entry_id AND e.archived_at IS NULL
JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id AND p.content_schema_version=3
LEFT JOIN users owner ON owner.id=w.owner_user_id
WHERE s.memory_id=m.id AND s.user_id=m.user_id AND w.deleted_at IS NULL
  AND e.wordlist_archive_generation=s.entry_archive_generation
  AND (w.owner_admin_id IS NOT NULL OR (owner.status='active' AND NOT EXISTS(
    SELECT 1 FROM account_deletion_requests d WHERE d.user_id=owner.id AND d.status='pending' AND d.effective_at<=clock_timestamp())))
  AND (w.owner_user_id=m.user_id OR (w.state='published' AND w.learning_public_generation=s.public_generation))
ORDER BY s.wordlist_id LIMIT 1
"#;

pub(super) async fn begin(
    pool: &PgPool,
    auth: &AuthUser,
) -> Result<(Tx<'static>, LearningContext), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(auth.subject)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let context = learner(&mut tx, auth).await?;
    Ok((tx, context))
}
pub(super) async fn enabled(tx: &mut Tx<'_>, user: Uuid) -> Result<bool, AppError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM learning_memory_enrollments WHERE user_id=$1 AND enabled AND enabled_at<=clock_timestamp())")
        .bind(user).fetch_one(&mut **tx).await.map_err(AppError::internal)
}
pub(super) fn require_feature(state: &AppState) -> Result<(), AppError> {
    if !state.learning_memory_enabled {
        return Err(AppError::not_found("复习功能尚未开放"));
    }
    Ok(())
}

pub(super) async fn overview(
    state: &AppState,
    auth: &AuthUser,
) -> Result<MemoryOverview, AppError> {
    let (mut tx, context) = begin(&state.pool, auth).await?;
    let at = crate::learning_tasks::repository::now(&mut tx).await?;
    let active = state.learning_memory_enabled && enabled(&mut tx, auth.subject).await?;
    let counts:(i64,i64,i64,i64,i64)=sqlx::query_as(sqlx::AssertSqlSafe(format!(r#"
        SELECT count(*) FILTER(WHERE NOT paused AND visible AND ready AND stage<>'relearning'),
               count(*) FILTER(WHERE NOT paused AND visible AND ready AND stage='relearning'),
               count(*) FILTER(WHERE NOT paused AND visible AND NOT ready),
               count(*) FILTER(WHERE paused),count(*) FILTER(WHERE NOT paused AND NOT visible)
        FROM (SELECT m.*,EXISTS(SELECT 1 FROM ({SOURCE}) src WHERE m.content_available OR m.checked_publication_id IS DISTINCT FROM src.publication_id OR m.checked_cefr_level IS DISTINCT FROM $5) AS visible,
          GREATEST(m.due_at,COALESCE(m.last_feedback_at+interval '10 minutes',m.due_at))<=$3
            AND (m.stage<>'relearning' OR m.retest_day IS DISTINCT FROM $4 OR m.retest_issued<2) AS ready
          FROM learning_memories m WHERE user_id=$1 AND answer_profile=$2) x
    "#))).bind(auth.subject).bind(&context.english_variant).bind(at)
        .bind(crate::learning_tasks::question::business_window(at).0).bind(&context.cefr_level).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    let session:Option<Uuid>=sqlx::query_scalar("SELECT s.id FROM learning_review_sessions s WHERE s.user_id=$1 AND s.expires_at>$2 AND s.settings_snapshot->>'english_variant'=$3 AND EXISTS(SELECT 1 FROM learning_review_items i JOIN learning_memories m ON m.id=i.memory_id JOIN learning_memory_sources ms ON ms.memory_id=m.id AND ms.wordlist_id=i.source_wordlist_id AND ms.membership_id=i.source_membership_id JOIN wordlists w ON w.id=i.source_wordlist_id AND w.deleted_at IS NULL JOIN wordlist_items wi ON wi.wordlist_id=w.id AND wi.entry_id=i.entry_id AND wi.learning_membership_id=i.source_membership_id JOIN lexicon.entries e ON e.id=i.entry_id AND e.archived_at IS NULL AND e.wordlist_archive_generation=i.entry_archive_generation JOIN lexicon.entry_publications p ON p.id=e.current_publication_id AND p.entry_id=e.id AND p.content_schema_version=3 LEFT JOIN users owner ON owner.id=w.owner_user_id WHERE i.session_id=s.id AND i.active AND i.expires_at>$2 AND NOT m.paused AND (w.owner_admin_id IS NOT NULL OR (owner.status='active' AND NOT EXISTS(SELECT 1 FROM account_deletion_requests d WHERE d.user_id=owner.id AND d.status='pending' AND d.effective_at<=clock_timestamp()))) AND (w.owner_user_id=$1 OR (w.state='published' AND w.learning_public_generation=i.source_public_generation)) AND (i.kind<>'retest' OR (m.revision=i.memory_revision AND m.memory_epoch=i.memory_epoch))) ORDER BY s.created_at DESC LIMIT 1")
        .bind(auth.subject).bind(at).bind(&context.english_variant).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(MemoryOverview {
        available: state.learning_memory_enabled,
        enabled: active,
        answer_profile: context.english_variant,
        due_count: counts.0,
        relearning_count: counts.1,
        later_count: counts.2,
        paused_count: counts.3,
        unavailable_count: counts.4,
        server_time: at,
        next_day_at: crate::learning_tasks::question::business_window(at).2,
        current_session_id: session,
    })
}
pub(super) async fn enrollment(
    state: &AppState,
    auth: &AuthUser,
    input: MemoryEnrollmentInput,
) -> Result<MemoryOverview, AppError> {
    require_feature(state)?;
    let (mut tx, _) = begin(&state.pool, auth).await?;
    sqlx::query("INSERT INTO learning_memory_enrollments(user_id,enabled) VALUES($1,$2) ON CONFLICT(user_id) DO UPDATE SET enabled=EXCLUDED.enabled,enabled_at=CASE WHEN NOT learning_memory_enrollments.enabled AND EXCLUDED.enabled THEN clock_timestamp() ELSE learning_memory_enrollments.enabled_at END")
        .bind(auth.subject).bind(input.enabled).execute(&mut *tx).await.map_err(AppError::internal)?;
    if !input.enabled {
        sqlx::query("UPDATE learning_review_items SET active=false WHERE user_id=$1 AND active")
            .bind(auth.subject)
            .execute(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    }
    tx.commit().await.map_err(AppError::internal)?;
    overview(state, auth).await
}
pub(super) async fn items(
    state: &AppState,
    auth: &AuthUser,
    q: MemoryQuery,
) -> Result<MemoryPage, AppError> {
    require_feature(state)?;
    let limit = q.limit.unwrap_or(20);
    let filter = q.filter.as_deref().unwrap_or("all");
    if !(1..=100).contains(&limit) || !matches!(filter, "all" | "due" | "paused" | "relearning") {
        return Err(invalid("记忆筛选或分页无效"));
    }
    let (mut tx, context) = begin(&state.pool, auth).await?;
    let at = crate::learning_tasks::repository::now(&mut tx).await?;
    let rows:Vec<MemoryListRow>=sqlx::query_as(sqlx::AssertSqlSafe(format!(r#"
      SELECT m.id,src.snapshot#>>'{{presentation,label}}',m.stage,
        {READY_AT},m.paused,
        src.publication_id IS NOT NULL AND {CONTENT_VISIBLE},m.revision
      FROM learning_memories m LEFT JOIN LATERAL ({SOURCE}) src ON true
      WHERE m.user_id=$1 AND m.answer_profile=$2 AND ($3::uuid IS NULL OR m.id>$3)
        AND ($4='all' OR ($4='paused' AND m.paused) OR ($4='relearning' AND m.stage='relearning')
          OR ($4='due' AND NOT m.paused AND src.publication_id IS NOT NULL AND {CONTENT_VISIBLE} AND {READY_AT}<=$5))
      ORDER BY m.id LIMIT $7
    "#))).bind(auth.subject).bind(&context.english_variant).bind(q.after).bind(filter).bind(at).bind(&context.cefr_level).bind(limit+1)
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    let more = rows.len() > limit as usize;
    let items: Vec<_> = rows
        .into_iter()
        .take(limit as usize)
        .map(
            |(id, label, stage, due_at, paused, content_available, revision)| MemoryItem {
                id,
                label: if content_available { label } else { None },
                stage,
                due_at,
                paused,
                content_available,
                revision,
            },
        )
        .collect();
    let next_cursor = more.then(|| items.last().expect("nonempty page").id);
    tx.commit().await.map_err(AppError::internal)?;
    Ok(MemoryPage {
        items,
        next_cursor,
        server_time: at,
    })
}
pub(super) async fn pause(
    state: &AppState,
    auth: &AuthUser,
    id: Uuid,
    input: MemoryPauseInput,
) -> Result<MemoryItem, AppError> {
    require_feature(state)?;
    let (mut tx, context) = begin(&state.pool, auth).await?;
    let result:Option<(String,DateTime<Utc>,i64)>=sqlx::query_as("UPDATE learning_memories SET paused=$3,revision=revision+1 WHERE id=$1 AND user_id=$2 AND revision=$4 RETURNING stage,due_at,revision")
        .bind(id).bind(auth.subject).bind(input.paused).bind(input.expected_revision).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let Some((stage, due_at, revision)) = result else {
        return Err(conflict("记忆已变化或不可访问，请刷新"));
    };
    sqlx::query("UPDATE learning_review_items SET active=false WHERE memory_id=$1 AND active")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let label:Option<String>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT src.snapshot#>>'{{presentation,label}}' FROM learning_memories m JOIN LATERAL ({SOURCE}) src ON true WHERE m.id=$1 AND (m.content_available OR m.checked_publication_id IS DISTINCT FROM src.publication_id OR m.checked_cefr_level IS DISTINCT FROM $2)"))).bind(id).bind(&context.cefr_level).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(MemoryItem {
        id,
        content_available: label.is_some(),
        label,
        stage,
        due_at,
        paused: input.paused,
        revision,
    })
}

#[derive(sqlx::FromRow)]
pub(super) struct Selected {
    pub id: Uuid,
    pub memory_epoch: i64,
    pub revision: i64,
    pub content_signature: Vec<u8>,
    pub stage: Stage,
    pub entry_id: Uuid,
    pub pos_id: Uuid,
    pub sense_id: Uuid,
    pub wordlist_id: Uuid,
    pub membership_id: Uuid,
    pub public_generation: Option<i64>,
    pub entry_archive_generation: i64,
    pub publication_id: Uuid,
}
impl Selected {
    pub fn question(&self) -> QuestionRow {
        QuestionRow {
            custom_item_id: None,
            cycle_unit_id: None,
            id: self.id,
            position: 0,
            source_wordlist_id: self.wordlist_id,
            source_membership_id: self.membership_id,
            source_public_generation: self.public_generation,
            entry_id: Some(self.entry_id),
            entry_archive_generation: Some(self.entry_archive_generation),
            pos_id: Some(self.pos_id),
            sense_id: Some(self.sense_id),
            prompt_snapshot: serde_json::Value::Null,
            answer_snapshot: serde_json::Value::Null,
        }
    }
    pub fn candidate(
        &self,
        context: &LearningContext,
        access: &SourceAccess,
    ) -> Result<Option<crate::learning_tasks::question::Candidate>, AppError> {
        let Some(publication) = access.entries.get(&self.entry_id) else {
            return Ok(None);
        };
        let source = LearningSource {
            wordlist_id: self.wordlist_id,
            revision: 0,
            membership_id: self.membership_id,
            public_generation: self.public_generation,
            entry_id: self.entry_id,
            archive_generation: self.entry_archive_generation,
            publication_id: publication.publication_id,
            content: publication.content_v3()?,
        };
        Ok(build_candidates(&[source], context)?
            .candidates
            .into_iter()
            .find(|c| c.pos_id == Some(self.pos_id) && c.sense_id == Some(self.sense_id)))
    }
}
pub(super) async fn selected(
    tx: &mut Tx<'_>,
    user: Uuid,
    context: &LearningContext,
    mode: &str,
    at: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Selected>, AppError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(r#"
      SELECT m.id,m.memory_epoch,m.revision,m.content_signature,m.stage,m.interval_step,m.due_at,m.last_feedback_at,
        m.entry_id,m.pos_id,m.sense_id,src.wordlist_id,src.membership_id,src.public_generation,src.entry_archive_generation,src.publication_id
      FROM learning_memories m JOIN LATERAL ({SOURCE}) src ON true
      WHERE m.user_id=$1 AND m.answer_profile=$2 AND NOT m.paused
        AND (m.content_available OR m.checked_publication_id IS DISTINCT FROM src.publication_id OR m.checked_cefr_level IS DISTINCT FROM $7)
        AND (($3='due' AND m.stage<>'relearning') OR ($3='retest' AND m.stage='relearning'))
        AND m.due_at<=$4
        AND GREATEST(m.due_at,COALESCE(m.last_feedback_at+interval '10 minutes',m.due_at))<=$4
        AND (m.stage<>'relearning' OR m.retest_day IS DISTINCT FROM $5 OR m.retest_issued<2)
      ORDER BY m.due_at,m.id LIMIT $6
    "#))).bind(user).bind(&context.english_variant).bind(mode).bind(at)
        .bind(crate::learning_tasks::question::business_window(at).0).bind(limit).bind(&context.cefr_level)
        .fetch_all(&mut **tx).await.map_err(AppError::internal)
}
