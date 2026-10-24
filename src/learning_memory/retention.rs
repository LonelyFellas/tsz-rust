use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

async fn sweep(pool: &PgPool) -> Result<(), sqlx::Error> {
    let users:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT user_id FROM ((SELECT user_id FROM learning_memory_events WHERE accepted_at<clock_timestamp()-interval '180 days' ORDER BY accepted_at LIMIT 500) UNION ALL (SELECT user_id FROM learning_review_sessions WHERE expires_at<clock_timestamp()-interval '180 days' ORDER BY expires_at LIMIT 100)) candidates LIMIT 20")
        .fetch_all(pool).await?;
    for user in users {
        let mut tx = pool.begin().await?;
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE id=$1 FOR UPDATE SKIP LOCKED")
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?;
        if locked.is_none() {
            continue;
        }
        let cutoff: DateTime<Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()-interval '180 days'")
                .fetch_one(&mut *tx)
                .await?;
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT answer_id FROM learning_memory_events WHERE user_id=$1 AND accepted_at<$2 ORDER BY accepted_at,answer_id LIMIT 500")
            .bind(user).bind(cutoff).fetch_all(&mut *tx).await?;
        sqlx::query("INSERT INTO learning_memory_history_months(memory_id,user_id,month,source_kind,attempt_count,correct_count,spaced_correct_count) SELECT memory_id,user_id,date_trunc('month',accepted_at AT TIME ZONE 'UTC')::date,source_kind,count(*),count(*) FILTER(WHERE is_correct),count(*) FILTER(WHERE effect='review_passed') FROM learning_memory_events WHERE answer_id=ANY($1) AND memory_id IS NOT NULL GROUP BY memory_id,user_id,3,source_kind ON CONFLICT(memory_id,month,source_kind) DO UPDATE SET attempt_count=learning_memory_history_months.attempt_count+EXCLUDED.attempt_count,correct_count=learning_memory_history_months.correct_count+EXCLUDED.correct_count,spaced_correct_count=learning_memory_history_months.spaced_correct_count+EXCLUDED.spaced_correct_count")
            .bind(&ids).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM learning_memory_events WHERE answer_id=ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        // Session identity expires permanently; deleted items cannot accept a replay as new work.
        sqlx::query("DELETE FROM learning_review_sessions WHERE id IN(SELECT s.id FROM learning_review_sessions s WHERE s.user_id=$1 AND s.expires_at<$2 AND NOT EXISTS(SELECT 1 FROM learning_review_attempts a JOIN learning_memory_events e ON e.answer_id=a.id AND e.source_kind='review' WHERE a.session_id=s.id) ORDER BY s.expires_at LIMIT 50)")
            .bind(user).bind(cutoff).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}
pub(crate) fn run_worker(pool: PgPool) {
    crate::safe_log::spawn_worker("learning_memory/retention", async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = sweep(&pool).await {
                tracing::error!(
                    error_kind = crate::safe_log::error_kind(&error),
                    "learning memory retention failed"
                );
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[sqlx::test]
    async fn history_is_aggregated_once_without_deleting_memory_or_recent_answers(pool: PgPool) {
        let user = Uuid::now_v7();
        let memory = Uuid::now_v7();
        sqlx::query("INSERT INTO users(id,email,password_hash,display_name) VALUES($1,$2,'fixture','memory retention')").bind(user).bind(format!("{user}@example.test")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO learning_memories(id,user_id,entry_id,pos_id,sense_id,skill,answer_profile,memory_epoch,content_signature,scheduler_version,stage,interval_step,due_at,last_feedback_at,last_evidence_at,revision) VALUES($1,$2,$3,$4,$5,'spelling_zh_to_en','BrE',1,decode(repeat('aa',32),'hex'),'memory_ladder_v1','learning',0,clock_timestamp(),clock_timestamp(),clock_timestamp(),1)").bind(memory).bind(user).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
        for days in [181, 0] {
            sqlx::query("INSERT INTO learning_memory_events(answer_id,user_id,memory_id,memory_epoch,previous_revision,resulting_revision,content_signature,scheduler_version,effect,is_correct,accepted_at) VALUES($1,$2,$3,1,0,1,decode(repeat('aa',32),'hex'),'memory_ladder_v1','initial_correct',true,clock_timestamp()-$4*interval '1 day')")
                .bind(Uuid::now_v7()).bind(user).bind(memory).bind(days as f64).execute(&pool).await.unwrap();
        }
        sweep(&pool).await.unwrap();
        sweep(&pool).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT sum(attempt_count)::bigint FROM learning_memory_history_months"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memory_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM learning_memories")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}
