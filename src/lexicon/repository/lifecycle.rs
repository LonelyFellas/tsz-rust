use super::*;

impl LexiconRepository {
    pub(crate) async fn transition_lifecycle(
        tx: &mut Transaction<'_, Postgres>,
        word: &AdminWordV2,
        actor_id: Uuid,
        request_id: Uuid,
    ) -> Result<(), LexiconRepositoryError> {
        let archived = matches!(word.status, crate::lexicon::dto::AdminWordStatus::Archived);
        let updated = sqlx::query(
            r#"
            UPDATE lexicon.entries
            SET lifecycle_revision = $2,
                archived_at = CASE WHEN $3 THEN $4 ELSE NULL END,
                archived_by_admin_id = CASE WHEN $3 THEN $5 ELSE NULL END,
                updated_by_admin_id = $5,
                updated_at = $4
            WHERE id = $1 AND lifecycle_revision = $6 AND revision = $7
            "#,
        )
        .bind(word.id)
        .bind(word.lifecycle_revision)
        .bind(archived)
        .bind(word.updated_at)
        .bind(actor_id)
        .bind(word.lifecycle_revision - 1)
        .bind(word.revision)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(LexiconRepositoryError::Invariant(
                "locked entry lifecycle revision changed during transition",
            ));
        }

        let event_type = if archived {
            "lexicon.entry_archived"
        } else {
            "lexicon.entry_restored"
        };
        sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision,
                event_type, payload, occurred_at, available_at
            ) VALUES ($1, 'lexicon.entry.lifecycle', $2, $3, $4, $5, $6, $6)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(word.id)
        .bind(word.lifecycle_revision)
        .bind(event_type)
        .bind(serde_json::json!({
            "entry_id": word.id,
            "lifecycle_revision": word.lifecycle_revision,
            "archived": archived,
            "has_current_publication": word.published_revision.is_some(),
        }))
        .bind(word.updated_at)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        insert_audit_action(
            tx,
            actor_id,
            if archived {
                "lexicon.entry.archive"
            } else {
                "lexicon.entry.restore"
            },
            word.id,
            word.revision,
            request_id,
            serde_json::json!({
                "lifecycle_revision": word.lifecycle_revision,
                "current_publication_preserved": word.published_revision.is_some(),
            }),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn insert_idempotent_response<T: serde::Serialize>(
        tx: &mut Transaction<'_, Postgres>,
        scope: &str,
        actor_id: Uuid,
        idempotency_key: Uuid,
        request_hash: &[u8],
        resource_id: Option<Uuid>,
        response: &T,
        response_status: i16,
    ) -> Result<(), LexiconRepositoryError> {
        insert_idempotency_value(
            tx,
            scope,
            actor_id,
            idempotency_key,
            request_hash,
            resource_id,
            serde_json::to_value(response)?,
            response_status,
        )
        .await
    }
}
