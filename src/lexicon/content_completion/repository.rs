use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::lexicon::{
    content_completion::dto::*,
    dto::{
        DraftFormsStepContent, DraftMeaningsStepContent, SenseGroupV2, WordHeadwordsV2,
        WordPosMeaningsV2,
    },
    normalization::{normalize_headword, sha256_json},
};

#[derive(Debug, thiserror::Error)]
pub enum ContentCompletionRepositoryError {
    #[error("word not found")]
    WordNotFound,
    #[error("job not found")]
    JobNotFound,
    #[error("word is archived")]
    EntryArchived,
    #[error("revision conflict")]
    RevisionConflict(i64),
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("invalid retry partitions")]
    InvalidRetry,
    #[error("serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSourceSnapshot {
    pub entry_id: Uuid,
    pub headword: String,
    pub headwords: WordHeadwordsV2,
    pub forms: DraftFormsStepContent,
    pub dictionary_provider: String,
    pub dictionary_version: String,
    pub source_record_keys: Vec<String>,
    pub dictionary_evidence_by_pos: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionResult {
    pub sense_groups: Vec<SenseGroupV2>,
    pub pos: WordPosMeaningsV2,
}

#[derive(Debug, sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    entry_id: Uuid,
    base_revision: i64,
    requested_scope: Vec<String>,
    fill_policy: String,
    source_snapshot: Value,
    status: String,
    result: Option<Value>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct PartitionRow {
    pos_id: Uuid,
    pos: String,
    status: String,
    attempt: i32,
    error_code: Option<String>,
    error_detail: Option<String>,
    provenance: Option<Value>,
}

#[derive(Debug)]
pub struct ClaimedPartition {
    pub job_id: Uuid,
    pub pos_id: Uuid,
    pub pos: String,
    pub attempt: i32,
    pub source: CompletionSourceSnapshot,
}

#[derive(Clone)]
pub struct ContentCompletionRepository {
    pool: PgPool,
}

impl ContentCompletionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        actor_id: Uuid,
        idempotency_key: Uuid,
        entry_id: Uuid,
        input: &CreateContentCompletionJobInput,
    ) -> Result<ContentCompletionJobEnvelope, ContentCompletionRepositoryError> {
        let request_hash = sha256_json(&(entry_id, input))
            .map_err(ContentCompletionRepositoryError::Serialization)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "lexicon.content-completion:{actor_id}:{idempotency_key}"
            ))
            .execute(&mut *tx)
            .await?;
        if let Some(existing) = sqlx::query_as::<_, (Uuid, Vec<u8>)>(
            "SELECT id, request_hash FROM lexicon.content_completion_jobs WHERE actor_id = $1 AND idempotency_key = $2 FOR UPDATE",
        )
        .bind(actor_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if existing.1 != request_hash {
                return Err(ContentCompletionRepositoryError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self.get(actor_id, entry_id, existing.0).await;
        }

        let row = sqlx::query_as::<_, (i64, Option<DateTime<Utc>>, String, String, Option<String>, Option<String>, Value, Value)>(
            r#"
            SELECT entry.revision, entry.archived_at,
                   entry.headword_mode, COALESCE(entry.source_dialect, ''),
                   (SELECT headword FROM lexicon.entry_headwords WHERE entry_id = entry.id AND dialect = 'common'),
                   (SELECT headword FROM lexicon.entry_headwords WHERE entry_id = entry.id AND dialect = COALESCE(entry.source_dialect, 'common') LIMIT 1),
                   projection.forms, entry.detection_snapshot
            FROM lexicon.entries entry
            JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
            WHERE entry.id = $1
            FOR UPDATE OF entry
            "#,
        )
        .bind(entry_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ContentCompletionRepositoryError::WordNotFound)?;
        if row.1.is_some() {
            return Err(ContentCompletionRepositoryError::EntryArchived);
        }
        if row.0 != input.base_revision {
            return Err(ContentCompletionRepositoryError::RevisionConflict(row.0));
        }
        let forms: DraftFormsStepContent = serde_json::from_value(row.6)?;
        if forms.pos.is_empty() {
            return Err(ContentCompletionRepositoryError::InvalidRetry);
        }
        let headwords = if row.2 == "unified" {
            WordHeadwordsV2::Unified {
                common: row
                    .4
                    .clone()
                    .or(row.5.clone())
                    .ok_or(ContentCompletionRepositoryError::WordNotFound)?,
            }
        } else {
            let uk: String = sqlx::query_scalar("SELECT headword FROM lexicon.entry_headwords WHERE entry_id = $1 AND dialect = 'uk'")
                .bind(entry_id).fetch_one(&mut *tx).await?;
            let us: String = sqlx::query_scalar("SELECT headword FROM lexicon.entry_headwords WHERE entry_id = $1 AND dialect = 'us'")
                .bind(entry_id).fetch_one(&mut *tx).await?;
            WordHeadwordsV2::Distinguish {
                uk,
                us,
                source_dialect: if row.3 == "uk" {
                    crate::lexicon::dto::SourceDialect::Uk
                } else {
                    crate::lexicon::dto::SourceDialect::Us
                },
            }
        };
        let headword = match &headwords {
            WordHeadwordsV2::Unified { common } => common.clone(),
            WordHeadwordsV2::Distinguish {
                uk,
                us,
                source_dialect,
            } => {
                if matches!(source_dialect, crate::lexicon::dto::SourceDialect::Uk) {
                    uk.clone()
                } else {
                    us.clone()
                }
            }
        };
        let normalized = normalize_headword(&headword)
            .map_err(|_| ContentCompletionRepositoryError::WordNotFound)?
            .key;
        let dictionary = sqlx::query_as::<_, (String, String, String, Vec<String>)>(
            r#"SELECT datasets.source_name, datasets.version,
                      terms.normalized_term, terms.pos
               FROM dictionary.active_terms terms
               JOIN dictionary.datasets datasets ON datasets.id = terms.dataset_id
               WHERE terms.normalized_term = $1"#,
        )
        .bind(&normalized)
        .fetch_optional(&mut *tx)
        .await?;
        let content_rows = sqlx::query_as::<_, (String, String, String, String, Value, String)>(
            r#"SELECT datasets.source_name, datasets.version, content.source_key,
                      content.pos, content.senses, content.source_locator
               FROM dictionary.entry_contents content
               JOIN dictionary.datasets datasets ON datasets.id = content.dataset_id
               WHERE datasets.status = 'active' AND content.normalized_term = $1
               ORDER BY content.source_key"#,
        )
        .bind(&normalized)
        .fetch_all(&mut *tx)
        .await?;
        let mut source_record_keys = Vec::new();
        let mut dictionary_evidence_by_pos = std::collections::HashMap::<String, Vec<Value>>::new();
        for (_, _, source_key, pos, senses, source_locator) in &content_rows {
            source_record_keys.push(source_key.clone());
            dictionary_evidence_by_pos
                .entry(pos.clone())
                .or_default()
                .push(serde_json::json!({
                    "source_key": source_key,
                    "source_locator": source_locator,
                    "senses": senses
                }));
        }
        let (dictionary_provider, dictionary_version) =
            if let Some((provider, version, source_term, parts_of_speech)) = dictionary {
                source_record_keys.insert(0, format!("dictionary.active_terms:{source_term}"));
                dictionary_evidence_by_pos
                    .entry("_term".to_owned())
                    .or_default()
                    .push(serde_json::json!({
                        "normalized_term": source_term,
                        "parts_of_speech": parts_of_speech
                    }));
                (provider, version)
            } else if let Some((provider, version, ..)) = content_rows.first() {
                (provider.clone(), version.clone())
            } else {
                source_record_keys.push(format!("lexicon.entries:{entry_id}"));
                ("entry_input".to_owned(), "unmatched".to_owned())
            };
        let dictionary_evidence_by_pos = dictionary_evidence_by_pos
            .into_iter()
            .map(|(pos, records)| (pos, Value::Array(records)))
            .collect();
        let source = CompletionSourceSnapshot {
            entry_id,
            headword,
            headwords,
            forms: forms.clone(),
            dictionary_provider,
            dictionary_version,
            source_record_keys,
            dictionary_evidence_by_pos,
        };
        let job_id = Uuid::now_v7();
        let requested_scope = input.scope.iter().map(scope_string).collect::<Vec<_>>();
        sqlx::query(
            r#"INSERT INTO lexicon.content_completion_jobs
               (id, entry_id, actor_id, idempotency_key, request_hash, base_revision,
                requested_scope, fill_policy, source_snapshot, status)
               VALUES ($1,$2,$3,$4,$5,$6,$7,'missing_only',$8,'pending')"#,
        )
        .bind(job_id)
        .bind(entry_id)
        .bind(actor_id)
        .bind(idempotency_key)
        .bind(request_hash)
        .bind(input.base_revision)
        .bind(&requested_scope)
        .bind(serde_json::to_value(&source)?)
        .execute(&mut *tx)
        .await?;
        for pos in &forms.pos {
            sqlx::query("INSERT INTO lexicon.content_completion_partitions (job_id,pos_id,pos,status) VALUES ($1,$2,$3,'pending')")
                .bind(job_id).bind(pos.pos_id).bind(&pos.pos).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.get(actor_id, entry_id, job_id).await
    }

    pub async fn get(
        &self,
        actor_id: Uuid,
        entry_id: Uuid,
        job_id: Uuid,
    ) -> Result<ContentCompletionJobEnvelope, ContentCompletionRepositoryError> {
        let job = sqlx::query_as::<_, JobRow>(
            r#"SELECT id, entry_id, base_revision, requested_scope, fill_policy,
                      source_snapshot, status, result, created_at, updated_at
               FROM lexicon.content_completion_jobs
               WHERE id=$1 AND entry_id=$2 AND actor_id=$3"#,
        )
        .bind(job_id)
        .bind(entry_id)
        .bind(actor_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ContentCompletionRepositoryError::JobNotFound)?;
        let partitions = sqlx::query_as::<_, PartitionRow>(
            r#"SELECT pos_id,pos,status,attempt,error_code,error_detail,provenance
               FROM lexicon.content_completion_partitions WHERE job_id=$1 ORDER BY pos"#,
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ContentCompletionJobEnvelope {
            job: map_job(job, partitions)?,
        })
    }

    pub async fn retry(
        &self,
        actor_id: Uuid,
        idempotency_key: Uuid,
        entry_id: Uuid,
        job_id: Uuid,
        input: &RetryContentCompletionJobInput,
    ) -> Result<ContentCompletionJobEnvelope, ContentCompletionRepositoryError> {
        if input.pos_ids.is_empty() || input.pos_ids.len() > 32 {
            return Err(ContentCompletionRepositoryError::InvalidRetry);
        }
        let mut tx = self.pool.begin().await?;
        let owns: Option<Uuid> = sqlx::query_scalar("SELECT id FROM lexicon.content_completion_jobs WHERE id=$1 AND entry_id=$2 AND actor_id=$3 FOR UPDATE")
            .bind(job_id).bind(entry_id).bind(actor_id).fetch_optional(&mut *tx).await?;
        if owns.is_none() {
            return Err(ContentCompletionRepositoryError::JobNotFound);
        }
        let retry_hash =
            sha256_json(input).map_err(ContentCompletionRepositoryError::Serialization)?;
        let retry_scope = format!("content_completion_retry:{job_id}");
        if let Some(hash) = sqlx::query_scalar::<_, Vec<u8>>("SELECT request_hash FROM platform.idempotency_records WHERE scope=$1 AND actor_id=$2 AND idempotency_key=$3")
            .bind(&retry_scope).bind(actor_id).bind(idempotency_key).fetch_optional(&mut *tx).await? {
            if hash != retry_hash { return Err(ContentCompletionRepositoryError::IdempotencyConflict); }
            tx.commit().await?;
            return self.get(actor_id, entry_id, job_id).await;
        }
        let updated = sqlx::query(
            r#"UPDATE lexicon.content_completion_partitions
               SET status='pending', error_code=NULL, error_detail=NULL, lease_expires_at=NULL, updated_at=now()
               WHERE job_id=$1 AND pos_id=ANY($2) AND status IN ('failed','missing')"#,
        ).bind(job_id).bind(&input.pos_ids).execute(&mut *tx).await?.rows_affected();
        if updated != input.pos_ids.len() as u64 {
            return Err(ContentCompletionRepositoryError::InvalidRetry);
        }
        sqlx::query("UPDATE lexicon.content_completion_jobs SET status='pending', result=NULL, updated_at=now() WHERE id=$1")
            .bind(job_id).execute(&mut *tx).await?;
        sqlx::query(
            r#"INSERT INTO platform.idempotency_records
               (scope,actor_id,idempotency_key,request_hash,resource_id,response_status,response_body,expires_at)
               VALUES ($1,$2,$3,$4,$5,202,'{}'::jsonb,now()+interval '24 hours')"#,
        ).bind(retry_scope).bind(actor_id).bind(idempotency_key).bind(retry_hash).bind(job_id)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        self.get(actor_id, entry_id, job_id).await
    }

    pub async fn claim(
        &self,
    ) -> Result<Option<ClaimedPartition>, ContentCompletionRepositoryError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (Uuid, Uuid, String, Value)>(
            r#"SELECT partition.job_id, partition.pos_id, partition.pos, job.source_snapshot
               FROM lexicon.content_completion_partitions partition
               JOIN lexicon.content_completion_jobs job ON job.id=partition.job_id
               WHERE partition.status='pending'
                  OR (partition.status='running' AND partition.lease_expires_at < now())
               ORDER BY partition.updated_at, partition.job_id, partition.pos_id
               FOR UPDATE OF partition SKIP LOCKED LIMIT 1"#,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((job_id, pos_id, pos, source)) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let attempt: i32 = sqlx::query_scalar("UPDATE lexicon.content_completion_partitions SET status='running',attempt=attempt+1,lease_expires_at=now()+interval '3 minutes',updated_at=now() WHERE job_id=$1 AND pos_id=$2 RETURNING attempt")
            .bind(job_id).bind(pos_id).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE lexicon.content_completion_jobs SET status='running',updated_at=now() WHERE id=$1")
            .bind(job_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(ClaimedPartition {
            job_id,
            pos_id,
            pos,
            attempt,
            source: serde_json::from_value(source)?,
        }))
    }

    pub async fn complete_partition(
        &self,
        job_id: Uuid,
        pos_id: Uuid,
        attempt: i32,
        result: &PartitionResult,
        provenance: &ContentCompletionProvenance,
    ) -> Result<(), ContentCompletionRepositoryError> {
        let updated = sqlx::query("UPDATE lexicon.content_completion_partitions SET status='completed',result=$4,provenance=$5,error_code=NULL,error_detail=NULL,lease_expires_at=NULL,updated_at=now() WHERE job_id=$1 AND pos_id=$2 AND status='running' AND attempt=$3")
            .bind(job_id).bind(pos_id).bind(attempt).bind(serde_json::to_value(result)?).bind(serde_json::to_value(provenance)?).execute(&self.pool).await?.rows_affected();
        if updated == 0 {
            return Ok(());
        }
        self.refresh_job(job_id).await
    }

    pub async fn fail_partition(
        &self,
        job_id: Uuid,
        pos_id: Uuid,
        attempt: i32,
        code: &str,
        detail: &str,
    ) -> Result<(), ContentCompletionRepositoryError> {
        let updated = sqlx::query("UPDATE lexicon.content_completion_partitions SET status='failed',error_code=$4,error_detail=$5,lease_expires_at=NULL,updated_at=now() WHERE job_id=$1 AND pos_id=$2 AND status='running' AND attempt=$3")
            .bind(job_id).bind(pos_id).bind(attempt).bind(code).bind(detail).execute(&self.pool).await?.rows_affected();
        if updated == 0 {
            return Ok(());
        }
        self.refresh_job(job_id).await
    }

    pub async fn mark_partition_missing(
        &self,
        job_id: Uuid,
        pos_id: Uuid,
        attempt: i32,
        code: &str,
        detail: &str,
    ) -> Result<(), ContentCompletionRepositoryError> {
        let updated = sqlx::query("UPDATE lexicon.content_completion_partitions SET status='missing',error_code=$4,error_detail=$5,lease_expires_at=NULL,updated_at=now() WHERE job_id=$1 AND pos_id=$2 AND status='running' AND attempt=$3")
            .bind(job_id).bind(pos_id).bind(attempt).bind(code).bind(detail).execute(&self.pool).await?.rows_affected();
        if updated == 0 {
            return Ok(());
        }
        self.refresh_job(job_id).await
    }

    async fn refresh_job(&self, job_id: Uuid) -> Result<(), ContentCompletionRepositoryError> {
        let rows = sqlx::query_as::<_, (String, Option<Value>)>("SELECT status,result FROM lexicon.content_completion_partitions WHERE job_id=$1 ORDER BY pos")
            .bind(job_id).fetch_all(&self.pool).await?;
        if rows
            .iter()
            .any(|(status, _)| status == "pending" || status == "running")
        {
            return Ok(());
        }
        let completed = rows
            .iter()
            .filter(|(status, _)| status == "completed")
            .count();
        let status = if completed == rows.len() {
            "completed"
        } else if completed > 0 {
            "partial"
        } else {
            "failed"
        };
        let mut result = DraftMeaningsStepContent::default();
        for value in rows
            .into_iter()
            .filter_map(|(status, value)| (status == "completed").then_some(value).flatten())
        {
            let partition: PartitionResult = serde_json::from_value(value)?;
            result.sense_groups.extend(partition.sense_groups);
            result.pos.push(partition.pos);
        }
        sqlx::query("UPDATE lexicon.content_completion_jobs SET status=$2,result=$3,updated_at=now() WHERE id=$1")
            .bind(job_id).bind(status).bind(serde_json::to_value(result)?).execute(&self.pool).await?;
        Ok(())
    }
}

fn scope_string(value: &ContentCompletionScope) -> String {
    match value {
        ContentCompletionScope::GrammarStructures => "grammar_structures",
        ContentCompletionScope::Meanings => "meanings",
        ContentCompletionScope::Examples => "examples",
    }
    .to_owned()
}

fn map_job(
    job: JobRow,
    partitions: Vec<PartitionRow>,
) -> Result<ContentCompletionJob, ContentCompletionRepositoryError> {
    let _source_snapshot = job.source_snapshot;
    Ok(ContentCompletionJob {
        id: job.id,
        entry_id: job.entry_id,
        base_revision: job.base_revision,
        status: match job.status.as_str() {
            "pending" => ContentCompletionJobStatus::Pending,
            "running" => ContentCompletionJobStatus::Running,
            "completed" => ContentCompletionJobStatus::Completed,
            "partial" => ContentCompletionJobStatus::Partial,
            _ => ContentCompletionJobStatus::Failed,
        },
        requested_scope: job
            .requested_scope
            .iter()
            .filter_map(|value| match value.as_str() {
                "grammar_structures" => Some(ContentCompletionScope::GrammarStructures),
                "meanings" => Some(ContentCompletionScope::Meanings),
                "examples" => Some(ContentCompletionScope::Examples),
                _ => None,
            })
            .collect(),
        fill_policy: {
            let _ = job.fill_policy;
            ContentCompletionFillPolicy::MissingOnly
        },
        partitions: partitions
            .into_iter()
            .map(|row| {
                Ok(ContentCompletionPartition {
                    pos_id: row.pos_id,
                    pos: row.pos,
                    status: match row.status.as_str() {
                        "pending" => ContentCompletionPartitionStatus::Pending,
                        "running" => ContentCompletionPartitionStatus::Running,
                        "completed" => ContentCompletionPartitionStatus::Completed,
                        "missing" => ContentCompletionPartitionStatus::Missing,
                        _ => ContentCompletionPartitionStatus::Failed,
                    },
                    attempt: row.attempt,
                    error_code: row.error_code,
                    error_detail: row.error_detail,
                    provenance: row.provenance.map(serde_json::from_value).transpose()?,
                })
            })
            .collect::<Result<_, ContentCompletionRepositoryError>>()?,
        result: job.result.map(serde_json::from_value).transpose()?,
        created_at: job.created_at,
        updated_at: job.updated_at,
    })
}
