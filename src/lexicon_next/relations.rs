use std::collections::HashSet;

use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    language,
    model::{NextDraft, NextEntry},
};
use crate::error::{AppError, ErrorCode};

fn invalid(message: &str) -> AppError {
    AppError::unprocessable(ErrorCode::InvalidRequestBody, message)
}

pub(super) fn validate(draft: &NextDraft, ids: &mut HashSet<Uuid>) -> Result<(), AppError> {
    for sense in &draft.senses {
        let mut targets = HashSet::new();
        for row in sense.relations.iter().flatten() {
            language::identity(ids, row.id)?;
            if !matches!(row.relation.as_str(), "derivative" | "synonym" | "antonym")
                || !crate::lexicon::validation::valid_percent(&row.score)
                || row.target_word_id.is_some() != row.target_sense_id.is_some()
            {
                return Err(invalid("拓展词类型、分值或关联词义无效"));
            }
            if let Some(word) = row.target_word_id {
                if word.is_nil()
                    || row.target_sense_id.is_some_and(|id| id.is_nil())
                    || row.pending_target_headword.is_some()
                    || row.pending_target_gloss.is_some()
                {
                    return Err(invalid("已关联拓展词不能携带待关联文本"));
                }
                let sense_key = (row.relation == "derivative").then_some(row.target_sense_id);
                if !targets.insert((row.relation.clone(), word.to_string(), sense_key)) {
                    return Err(invalid("同类拓展词不能重复关联同一目标"));
                }
            } else {
                let word = row.pending_target_headword.as_deref().unwrap_or("");
                let normalized = crate::lexicon::normalization::NormalizedHeadword::parse(word)
                    .map_err(|_| invalid("请输入有效的拓展词词面"))?;
                if let Some(gloss) = &row.pending_target_gloss {
                    language::text(gloss, 5000)?;
                }
                if row.relation != "derivative"
                    && !targets.insert((row.relation.clone(), normalized.key, None))
                {
                    return Err(invalid("同类拓展词词面不能重复"));
                }
            }
        }
    }
    Ok(())
}

// All entry saves and lifecycle writes already hold lock_list_writers. Reading
// current snapshots under that lock also protects against concurrent unlink/delete.
pub(super) async fn ensure_not_referenced(
    conn: &mut PgConnection,
    entry: Uuid,
    retained_senses: &[Uuid],
) -> Result<(), AppError> {
    let used: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM lexicon_next.entries e
         JOIN lexicon_next.draft_revisions r ON r.entry_id=e.id AND r.revision=e.revision
         CROSS JOIN LATERAL jsonb_array_elements(r.document->'senses') s
         CROSS JOIN LATERAL jsonb_array_elements(COALESCE(s->'relations','[]'::jsonb)) rel
         WHERE e.id<>$1 AND rel->>'target_word_id'=$1::text
         AND NOT ((rel->>'target_sense_id')::uuid=ANY($2)))",
    )
    .bind(entry)
    .bind(retained_senses)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if used {
        return Err(AppError::conflict(
            ErrorCode::RevisionConflict,
            None,
            "词条或词义仍被其他词条的拓展词引用，请先解除关联",
        ));
    }
    Ok(())
}

pub(super) async fn prepare(
    conn: &mut PgConnection,
    entry: Uuid,
    draft: &mut NextDraft,
    previous: &NextEntry,
    negotiated: bool,
) -> Result<(), AppError> {
    if !negotiated {
        for sense in &mut draft.senses {
            sense.relations = previous
                .draft
                .senses
                .iter()
                .find(|saved| saved.id == sense.id)
                .and_then(|saved| saved.relations.clone());
        }
    }
    ensure_not_referenced(
        conn,
        entry,
        &draft.senses.iter().map(|s| s.id).collect::<Vec<_>>(),
    )
    .await?;
    for sense in &draft.senses {
        for row in sense.relations.iter().flatten() {
            let Some(target) = row.target_word_id else {
                continue;
            };
            if target == entry {
                return Err(invalid("拓展词不能关联当前词条自身"));
            }
            let valid: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM lexicon_next.entries e
                 JOIN lexicon_next.senses s ON s.id=$2
                 JOIN lexicon_next.nodes n ON n.id=s.id AND n.entry_id=e.id
                 WHERE e.id=$1 AND e.status<>'archived' AND e.target_language=$3 AND e.medium_locale=$4)",
            ).bind(target).bind(row.target_sense_id).bind(&previous.target_language)
                .bind(&previous.medium_locale).fetch_one(&mut *conn).await.map_err(AppError::internal)?;
            if !valid {
                return Err(invalid(
                    "关联词条或词义已不可用，或不属于当前目标语言与媒介语言",
                ));
            }
        }
    }
    Ok(())
}
