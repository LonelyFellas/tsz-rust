use super::*;

// --- helpers ---

pub(super) fn map_entry_write_error(error: sqlx::Error) -> LexiconRepositoryError {
    if [
        "lexicon_relations_target_fkey",
        "lexicon_relations_prebound_target_fkey",
        "lexicon_sentence_links_target_fkey",
        "lexicon_publication_sense_refs_target_fkey",
        "lexicon_publication_sense_refs_target_node_fkey",
        "lexicon_sentence_associations_target_fkey",
        "lexicon_sentence_associations_target_slot_fkey",
        "lexicon_v3_phrase_components_base_fkey",
        "lexicon_v3_phrase_components_form_fkey",
        "lexicon_v3_phrase_components_pos_fkey",
        "lexicon_v3_phrase_components_node_fkey",
    ]
    .iter()
    .any(|constraint| is_foreign_key_violation(&error, constraint))
    {
        return LexiconRepositoryError::ReferenceTargetChanged;
    }
    LexiconRepositoryError::Database(error)
}

pub(super) fn map_target_publication_lock_error(error: sqlx::Error) -> LexiconRepositoryError {
    if matches!(&error, sqlx::Error::Database(database)
        if database.code().as_deref() == Some("55P03"))
    {
        LexiconRepositoryError::TargetPublicationBusy
    } else {
        LexiconRepositoryError::Database(error)
    }
}

pub(super) fn escape_like_literal(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(super) fn kind_string(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Word => "word",
        EntryKind::Phrase => "phrase",
    }
}

pub(super) fn dialect_string(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Common => "common",
        Dialect::Uk => "uk",
        Dialect::Us => "us",
    }
}

pub(super) fn origin_string(origin: TextOrigin) -> &'static str {
    match origin {
        TextOrigin::Dictionary => "dictionary",
        TextOrigin::Converted => "converted",
        TextOrigin::Manual => "manual",
    }
}

// --- entry commands ---

impl LexiconRepository {
    pub(crate) async fn forms_surface_acknowledgement(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
    ) -> Result<Option<FormsSurfaceAcknowledgementRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, FormsSurfaceAcknowledgementRecord>(
            r#"
            SELECT entry_id, forms_revision, forms_content_digest, match_ids,
                   match_digest, acknowledged_by_admin_id, acknowledged_at,
                   policy_name, policy_epoch, normalization_version
            FROM lexicon.entry_forms_surface_acknowledgements
            WHERE entry_id = $1
            FOR UPDATE
            "#,
        )
        .bind(entry_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn forms_surface_acknowledgement_by_entry(
        &self,
        entry_id: Uuid,
    ) -> Result<Option<FormsSurfaceAcknowledgementRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, FormsSurfaceAcknowledgementRecord>(
            r#"
            SELECT entry_id, forms_revision, forms_content_digest, match_ids,
                   match_digest, acknowledged_by_admin_id, acknowledged_at,
                   policy_name, policy_epoch, normalization_version
            FROM lexicon.entry_forms_surface_acknowledgements
            WHERE entry_id = $1
            "#,
        )
        .bind(entry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn upsert_forms_surface_acknowledgement(
        tx: &mut Transaction<'_, Postgres>,
        evidence: &FormsSurfaceAcknowledgementRecord,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_forms_surface_acknowledgements (
                entry_id, forms_revision, forms_content_digest, match_ids,
                match_digest, acknowledged_by_admin_id, acknowledged_at,
                policy_name, policy_epoch, normalization_version
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (entry_id) DO UPDATE SET
                forms_revision = EXCLUDED.forms_revision,
                forms_content_digest = EXCLUDED.forms_content_digest,
                match_ids = EXCLUDED.match_ids,
                match_digest = EXCLUDED.match_digest,
                acknowledged_by_admin_id = EXCLUDED.acknowledged_by_admin_id,
                acknowledged_at = EXCLUDED.acknowledged_at,
                policy_name = EXCLUDED.policy_name,
                policy_epoch = EXCLUDED.policy_epoch,
                normalization_version = EXCLUDED.normalization_version
            "#,
        )
        .bind(evidence.entry_id)
        .bind(evidence.forms_revision)
        .bind(&evidence.forms_content_digest)
        .bind(&evidence.match_ids)
        .bind(&evidence.match_digest)
        .bind(evidence.acknowledged_by_admin_id)
        .bind(evidence.acknowledged_at)
        .bind(&evidence.policy_name)
        .bind(evidence.policy_epoch)
        .bind(evidence.normalization_version)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(())
    }

    pub(crate) async fn delete_forms_surface_acknowledgement(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query("DELETE FROM lexicon.entry_forms_surface_acknowledgements WHERE entry_id = $1")
            .bind(entry_id)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
        Ok(())
    }
}

// --- entry lookup ---

impl LexiconRepository {
    pub(crate) async fn entry_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<EntryRecord>, LexiconRepositoryError> {
        sqlx::query_as::<_, EntryRecord>(
            r#"
            SELECT entry.id,
                   entry.content_schema_version,
                   entry.language,
                   entry.kind,
                   entry.revision,
                   entry.lifecycle_revision, entry.annotation, entry.annotation_revision,
                   entry.detection_snapshot,
                   entry.current_publication_id,
                   publication.source_revision AS current_publication_source_revision,
                   publication.published_at AS current_published_at,
                   projection.forms,
                   projection.meanings,
                   COALESCE((
                       SELECT array_agg(progress.step ORDER BY CASE progress.step
                           WHEN 'basics' THEN 1 WHEN 'forms' THEN 2 WHEN 'meanings' THEN 3 END)
                       FROM lexicon.entry_step_progress progress
                       WHERE progress.entry_id = entry.id
                   ), ARRAY[]::text[]) AS completed_steps,
                   entry.created_by_admin_id,
                   entry.created_at,
                   entry.updated_at
                   ,entry.archived_at
                   ,entry.archived_by_admin_id
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn entry_by_id_for_update(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<EntryRecord>, LexiconRepositoryError> {
        // 先锁聚合根；后续投影和步骤读取都处于同一事务快照。
        let locked = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if locked.is_none() {
            return Ok(None);
        }
        sqlx::query_as::<_, EntryRecord>(
            r#"
            SELECT entry.id,
                   entry.content_schema_version,
                   entry.language,
                   entry.kind,
                   entry.revision,
                   entry.lifecycle_revision, entry.annotation, entry.annotation_revision,
                   entry.detection_snapshot,
                   entry.current_publication_id,
                   publication.source_revision AS current_publication_source_revision,
                   publication.published_at AS current_published_at,
                   projection.forms,
                   projection.meanings,
                   COALESCE((
                       SELECT array_agg(progress.step ORDER BY CASE progress.step
                           WHEN 'basics' THEN 1 WHEN 'forms' THEN 2 WHEN 'meanings' THEN 3 END)
                       FROM lexicon.entry_step_progress progress
                       WHERE progress.entry_id = entry.id
                   ), ARRAY[]::text[]) AS completed_steps,
                   entry.created_by_admin_id,
                   entry.created_at,
                   entry.updated_at
                   ,entry.archived_at
                   ,entry.archived_by_admin_id
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            LEFT JOIN lexicon.entry_publications publication
              ON publication.id = entry.current_publication_id
             AND publication.entry_id = entry.id
            WHERE entry.id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }

    /// 清除**批内成员之间**的引用行（只动这些行，不碰其余内容）。
    ///
    /// 批量删除逐条执行「校验 + 删除」，先删掉的那条若仍被批内另一条引用，
    /// 会撞上 relations / sentence_links 等的 ON DELETE RESTRICT 外键——
    /// 于是能否删除取决于 id 排序而非业务规则。这些引用行本就会随引用方一起
    /// 被删掉，提前清掉它们即可让结果与顺序无关。
    ///
    /// 不含 entry_publication_sense_refs：那要求引用方已发布，
    /// 而已发布词条在更早的 publication 判定处就被拒绝了。
    pub(crate) async fn clear_intra_batch_references(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<(), LexiconRepositoryError> {
        if entry_ids.len() < 2 {
            return Ok(());
        }
        for statement in [
            r#"
            DELETE FROM lexicon.relations
             WHERE entry_id = ANY($1)
               AND (target_entry_id = ANY($1) OR prebound_target_entry_id = ANY($1))
            "#,
            r#"
            DELETE FROM lexicon.sentence_links
             WHERE entry_id = ANY($1) AND target_entry_id = ANY($1)
            "#,
            r#"
            DELETE FROM lexicon.sentence_associations
             WHERE entry_id = ANY($1) AND target_entry_id = ANY($1)
            "#,
            r#"
            DELETE FROM lexicon.v3_phrase_variant_component_usages
             WHERE entry_id = ANY($1) AND target_entry_id = ANY($1)
            "#,
            r#"
            DELETE FROM lexicon.v3_phrase_sense_component_usages
             WHERE entry_id = ANY($1) AND target_entry_id = ANY($1)
            "#,
        ] {
            sqlx::query(statement)
                .bind(entry_ids)
                .execute(&mut **tx)
                .await
                .map_err(map_entry_write_error)?;
        }
        Ok(())
    }

    /// `batch_members` 是与本条同批次、将在同一事务内一并删除的词条。
    /// 它们之间的互相引用不应互相阻塞——都要删掉，谁先谁后不该改变结果。
    /// 不排除的话，批内互引能否删除就取决于执行顺序（即 id 排序），而非业务规则。
    pub(crate) async fn delete_never_published_entry(
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        id: Uuid,
        revision: i64,
        batch_members: &[Uuid],
    ) -> Result<bool, LexiconRepositoryError> {
        let has_inbound_references = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM lexicon.relations relation
                JOIN lexicon.nodes target ON target.id = relation.target_sense_id
                WHERE target.entry_id = $1 AND relation.entry_id <> $1
                  AND relation.entry_id <> ALL($2)
                UNION ALL
                SELECT 1
                FROM lexicon.relations relation
                WHERE relation.prebound_target_entry_id = $1
                  AND relation.entry_id <> $1
                  AND relation.entry_id <> ALL($2)
                UNION ALL
                SELECT 1
                FROM lexicon.sentence_links link
                JOIN lexicon.nodes target ON target.id = link.target_sense_id
                WHERE target.entry_id = $1 AND link.entry_id <> $1
                  AND link.entry_id <> ALL($2)
                UNION ALL
                SELECT 1
                FROM lexicon.entry_publication_sense_refs sense_ref
                WHERE sense_ref.target_entry_id = $1
                  AND sense_ref.entry_id <> $1
                  AND sense_ref.entry_id <> ALL($2)
                UNION ALL
                -- 待认领的例句关联：目标已指向本词条，只是尚未物化成 sentence_links。
                SELECT 1
                FROM lexicon.sentence_associations association
                WHERE association.target_entry_id = $1
                  AND association.entry_id <> $1
                  AND association.entry_id <> ALL($2)
                UNION ALL
                -- V3 短语把本词条当作成分（短语 ↔ 单词）：B1 期间变体级与释义级双源并存。
                SELECT 1
                FROM lexicon.v3_phrase_variant_component_usages usage
                WHERE usage.target_entry_id = $1
                  AND usage.entry_id <> $1
                  AND usage.entry_id <> ALL($2)
                UNION ALL
                SELECT 1
                FROM lexicon.v3_phrase_sense_component_usages usage
                WHERE usage.state = 'resolved'
                  AND usage.target_entry_id = $1
                  AND usage.entry_id <> $1
                  AND usage.entry_id <> ALL($2)
            )
            "#,
        )
        .bind(id)
        .bind(batch_members)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        if has_inbound_references {
            return Ok(false);
        }
        delete_current_content(tx, id).await?;
        let deleted = sqlx::query_scalar::<_, Uuid>(
            r#"
            DELETE FROM lexicon.entries entry
            WHERE entry.id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM lexicon.entry_publications publication
                  WHERE publication.entry_id = entry.id
              )
            RETURNING entry.id
            "#,
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_entry_write_error)?;
        if deleted.is_some() {
            insert_audit_action(
                tx,
                actor_id,
                "lexicon.entry.delete_draft",
                id,
                revision,
                request_id,
                serde_json::json!({"never_published": true}),
            )
            .await?;
        }
        Ok(deleted.is_some())
    }

    pub(crate) async fn has_inbound_prebound_relations(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<bool, LexiconRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM lexicon.relations
                WHERE prebound_target_entry_id = $1
                  AND entry_id <> $1
            )
            "#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)
    }
}

// --- persistence ---

pub(super) async fn insert_node(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
    stable_slot: bool,
) -> Result<(), LexiconRepositoryError> {
    let result = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (id) DO UPDATE
        SET removed_from_draft_at = NULL
        WHERE lexicon.nodes.entry_id = EXCLUDED.entry_id
          AND lexicon.nodes.node_type = EXCLUDED.node_type
          AND lexicon.nodes.node_role <> 'legacy'
          AND lexicon.nodes.parent_node_id IS NOT DISTINCT FROM EXCLUDED.parent_node_id
          AND lexicon.nodes.node_role = EXCLUDED.node_role
          AND lexicon.nodes.stable_slot = EXCLUDED.stable_slot
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await
    .map_err(map_entry_write_error)?;
    if result.rows_affected() != 1 {
        return Err(LexiconRepositoryError::Invariant(
            "node id belongs to another entry, type, parent, or slot",
        ));
    }
    Ok(())
}

pub(super) async fn delete_current_content(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<(), LexiconRepositoryError> {
    for statement in [
        "DELETE FROM lexicon.relations WHERE entry_id = $1",
        "DELETE FROM lexicon.sentence_links WHERE entry_id = $1",
        "DELETE FROM lexicon.text_variants WHERE entry_id = $1",
        "DELETE FROM lexicon.definitions WHERE entry_id = $1",
        "DELETE FROM lexicon.sentences WHERE entry_id = $1",
        "DELETE FROM lexicon.senses WHERE entry_id = $1",
        "DELETE FROM lexicon.grammar_structures WHERE entry_id = $1",
        "DELETE FROM lexicon.sense_groups WHERE entry_id = $1",
        "DELETE FROM lexicon.entry_pos WHERE entry_id = $1",
    ] {
        sqlx::query(statement)
            .bind(entry_id)
            .execute(&mut **tx)
            .await
            .map_err(map_entry_write_error)?;
    }
    Ok(())
}

pub(super) async fn insert_audit_action(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
    action: &str,
    resource_id: Uuid,
    resource_revision: i64,
    request_id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), LexiconRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO audit.admin_actions (
            id, actor_admin_id, action, resource_type, resource_id,
            resource_revision, request_id, metadata
        ) VALUES ($1, $2, $3, 'lexicon.entry', $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(resource_id)
    .bind(resource_revision)
    .bind(request_id)
    .bind(metadata)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(LexiconRepositoryError::Database)
}

impl LexiconRepository {
    pub(crate) async fn insert_command_surface_confirmation_audits(
        tx: &mut Transaction<'_, Postgres>,
        actor_id: Uuid,
        request_id: Uuid,
        resource_id: Uuid,
        resource_revision: i64,
        confirmation: &crate::lexicon::surface_snapshot::VerifiedSurfaceConfirmation,
    ) -> Result<(), LexiconRepositoryError> {
        let reasons = confirmation.owner_bundle["confirmation_reasons"]
            .as_array()
            .ok_or(LexiconRepositoryError::Invariant(
                "surface confirmation owner bundle has no reasons",
            ))?;
        for reason in reasons {
            let action = match reason.as_str() {
                Some("unacknowledged_surface_matches") => {
                    "lexicon.surface_warning.acknowledge_command"
                }
                Some("visibility_activation") => "lexicon.visibility_activation.acknowledge",
                _ => {
                    return Err(LexiconRepositoryError::Invariant(
                        "surface confirmation owner bundle has an unknown reason",
                    ));
                }
            };
            insert_audit_action(
                tx,
                actor_id,
                action,
                resource_id,
                resource_revision,
                request_id,
                serde_json::json!({
                    "snapshot_id": confirmation.snapshot_id,
                    "command": confirmation.binding.command,
                    "policy_name": confirmation.binding.policy_name,
                    "policy_epoch": confirmation.binding.policy_epoch,
                    "match_digest": confirmation.match_digest,
                    "match_ids": confirmation.match_ids,
                    "owner_bundle": confirmation.owner_bundle,
                    "confirmation_reason": reason,
                }),
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn replace_meanings_content(
        tx: &mut Transaction<'_, Postgres>,
        entry_id: Uuid,
        meanings: &DraftMeaningsStepContent,
        sub_parts: &HashMap<String, Uuid>,
    ) -> Result<(), LexiconRepositoryError> {
        // 成分节点只能按 node_role 退役：`phrase_component_usage` 这个 node_type 在 B1
        // 期间 forms 侧还在产出变体级节点，按 type 退役会连它们一起误退，
        // 随后 insert_v3_publication_nodes 漏行、insert_publication_sense_refs 外键违约。
        sqlx::query(
            r#"
            UPDATE lexicon.nodes
            SET removed_from_draft_at = now()
            WHERE entry_id = $1
              AND removed_from_draft_at IS NULL
              AND (node_type = ANY($2) OR node_role = $3)
            "#,
        )
        .bind(entry_id)
        .bind([
            "sense_group",
            "grammar_structure",
            "sense",
            "definition",
            "sentence",
            "text_variant",
            "relation",
        ])
        .bind(crate::lexicon::node_identity::PHRASE_COMPONENT_USAGE_ROLE)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;

        for statement in [
            "DELETE FROM lexicon.relations WHERE entry_id = $1",
            "DELETE FROM lexicon.sentence_links WHERE entry_id = $1",
            "DELETE FROM lexicon.text_variants WHERE entry_id = $1",
            "DELETE FROM lexicon.definitions WHERE entry_id = $1",
            "DELETE FROM lexicon.sentences WHERE entry_id = $1",
            "DELETE FROM lexicon.senses WHERE entry_id = $1",
            "DELETE FROM lexicon.grammar_structures WHERE entry_id = $1",
            "DELETE FROM lexicon.sense_groups WHERE entry_id = $1",
        ] {
            sqlx::query(statement)
                .bind(entry_id)
                .execute(&mut **tx)
                .await
                .map_err(map_entry_write_error)?;
        }
        insert_meanings(tx, entry_id, meanings, sub_parts).await
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_idempotency_value(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    actor_id: Uuid,
    idempotency_key: Uuid,
    request_hash: &[u8],
    resource_id: Option<Uuid>,
    response_body: serde_json::Value,
    response_status: i16,
) -> Result<(), LexiconRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO platform.idempotency_records (
            scope, idempotency_key, actor_id, request_hash, resource_id,
            response_status, response_body, expires_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '24 hours')
        "#,
    )
    .bind(scope)
    .bind(idempotency_key)
    .bind(actor_id)
    .bind(request_hash)
    .bind(resource_id)
    .bind(response_status)
    .bind(response_body)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(LexiconRepositoryError::Database)
}

#[cfg(test)]
mod tests {
    use super::escape_like_literal;

    #[test]
    fn escape_like_literal_neutralizes_wildcards_and_the_escape_character() {
        // 关键字里的 % 与 _ 必须当字面量：搜 "100%" 只该命中真的带百分号的词面。
        assert_eq!(escape_like_literal("100%"), r"100\%");
        assert_eq!(escape_like_literal("give_up"), r"give\_up");
        // 反斜杠先自转义，否则用户输入的 "\%" 会重新变回通配符。
        assert_eq!(escape_like_literal(r"a\%b"), r"a\\\%b");
        assert_eq!(escape_like_literal("give"), "give");
    }
}
