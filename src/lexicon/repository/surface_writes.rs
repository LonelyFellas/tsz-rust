use std::collections::BTreeSet;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SurfaceLockKey {
    pub language: String,
    pub dialect_scope: String,
    pub normalized_surface: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SurfaceProjectionSource {
    pub entry_id: Uuid,
    pub source_id: String,
    pub source_kind: &'static str,
    pub source_node_id: Option<Uuid>,
    pub language: String,
    pub entry_kind: &'static str,
    pub dialect: &'static str,
    pub dialect_scope: &'static str,
    pub surface: String,
    pub normalized_surface: String,
    pub normalization_version: i16,
    pub pos_id: Option<Uuid>,
    pub pos: Option<String>,
    pub form_type: Option<String>,
}

pub(crate) fn surface_lock_keys<'a>(
    source_sets: impl IntoIterator<Item = &'a [SurfaceProjectionSource]>,
) -> Vec<SurfaceLockKey> {
    source_sets
        .into_iter()
        .flat_map(|sources| sources.iter())
        .map(|source| SurfaceLockKey {
            language: source.language.clone(),
            dialect_scope: source.dialect_scope.to_owned(),
            normalized_surface: source.normalized_surface.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 单次加锁的等待上限。
///
/// 要大于「连接归还线程池并把排队的 ROLLBACK 冲出去」这个窗口（正常亚毫秒级，
/// CI 这类负载高的环境会被拉长），又要小得让真正的并发写者快速失败。
const SURFACE_CONTEXT_LOCK_TIMEOUT: &str = "750ms";

impl LexiconRepository {
    /// 只锁一次改动或 token 复核真正碰到的命中词条上下文。
    ///
    /// 用**有界等待**而不是 try-lock。try-lock 的前提是「抢不到 = 真有并发写者」，
    /// 而这个前提不成立：sqlx 的 `Transaction::drop` 只把 ROLLBACK **入队**，等那条
    /// 连接下一次被异步使用时才真正发出（见 sqlx-core `transaction.rs` 的 Drop 实现）。
    /// 于是一个刚因校验失败而返回的请求，它的 advisory xact lock 会继续挂在那条尚未
    /// 回滚干净的连接上；紧接着的下一个请求从连接池拿到别的连接，对同一个 entry
    /// try-lock 就会失败，用户拿到一个凭空的 409 `reference_conflict`——他明明只是
    /// 改完刚才那个校验错误又存了一次。
    ///
    /// 等待上限保证两件事都成立：自己没回滚干净的锁在毫秒级就能等到，真正的并发写者
    /// 仍然会在上限内快速失败，不会长时间占着连接池。
    pub async fn lock_surface_contexts(
        tx: &mut Transaction<'_, Postgres>,
        entry_ids: &[Uuid],
    ) -> Result<(), LexiconRepositoryError> {
        let entry_ids = entry_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if entry_ids.is_empty() {
            return Ok(());
        }
        // SET LOCAL 的作用域是整个事务，所以取完锁必须立刻还原，否则后续语句会
        // 一并背上这个超时。
        // set_config 的第三个参数 true = 事务内生效，等价于 SET LOCAL；
        // 走它是因为 SET 不接受占位符。
        sqlx::query("SELECT set_config('lock_timeout', $1, true)")
            .bind(SURFACE_CONTEXT_LOCK_TIMEOUT)
            .execute(&mut **tx)
            .await
            .map_err(LexiconRepositoryError::Database)?;
        let acquired = sqlx::query(
            r#"
            SELECT pg_advisory_xact_lock(hashtextextended(
                'lexicon.surface-context:' || requested.entry_id::text,
                0
            ))
            FROM unnest($1::uuid[]) AS requested(entry_id)
            ORDER BY requested.entry_id
            "#,
        )
        .bind(entry_ids)
        .execute(&mut **tx)
        .await;
        match acquired {
            Ok(_) => {
                // 只有成功路径才还原：超时会让事务进入 aborted 状态，那里再发任何语句
                // 都只会拿到 25P02，把真正的 55P03 盖掉。失败时调用方一定会回滚，
                // SET LOCAL 本来也随之作废。
                sqlx::query("SET LOCAL lock_timeout = DEFAULT")
                    .execute(&mut **tx)
                    .await
                    .map_err(LexiconRepositoryError::Database)?;
                Ok(())
            }
            // 两种「没抢到」都要收敛成同一个可重试的占用信号，否则会以 500 冒出去：
            //   55P03 lock_not_available  等满上限
            //   40P01 deadlock_detected   被死锁检测器选中回滚
            // 死锁是换成阻塞锁之后才可能出现的：save_meanings 在同一事务里先锁自身
            // 再锁关联词目标（editing.rs 两处调用），两个互相引用的词条同时保存就会
            // ABBA。try-lock 时代第二次直接返回 false，不会走到这里。
            Err(sqlx::Error::Database(error))
                if matches!(error.code().as_deref(), Some("55P03") | Some("40P01")) =>
            {
                Err(LexiconRepositoryError::SurfaceContextBusy)
            }
            Err(error) => Err(LexiconRepositoryError::Database(error)),
        }
    }

    /// Joins the surface-policy writer barrier for the lifetime of `transaction`.
    ///
    /// This is public so database contract tests can prove that a policy disable
    /// waits for every in-flight create which entered under the previous epoch.
    pub async fn lock_surface_policy_writer(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), LexiconRepositoryError> {
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('lexicon.surface-policy-writer', 0))",
        )
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(LexiconRepositoryError::Database)
    }

    pub(crate) async fn lock_surface_keys(
        tx: &mut Transaction<'_, Postgres>,
        keys: &[SurfaceLockKey],
    ) -> Result<(), LexiconRepositoryError> {
        if keys.is_empty() {
            return Ok(());
        }
        let languages = keys
            .iter()
            .map(|key| key.language.as_str())
            .collect::<Vec<_>>();
        let dialect_scopes = keys
            .iter()
            .map(|key| key.dialect_scope.as_str())
            .collect::<Vec<_>>();
        let normalized_surfaces = keys
            .iter()
            .map(|key| key.normalized_surface.as_str())
            .collect::<Vec<_>>();
        sqlx::query(
            r#"
            WITH requested AS MATERIALIZED (
                SELECT language, dialect_scope, normalized_surface
                FROM unnest($1::text[], $2::text[], $3::text[])
                    AS value(language, dialect_scope, normalized_surface)
                ORDER BY language, dialect_scope, normalized_surface
            )
            SELECT pg_advisory_xact_lock(hashtextextended(
                'lexicon.surface:' || requested.language || ':' ||
                requested.dialect_scope || ':' || requested.normalized_surface,
                0
            ))
            FROM requested
            "#,
        )
        .bind(languages)
        .bind(dialect_scopes)
        .bind(normalized_surfaces)
        .execute(&mut **tx)
        .await
        .map_err(LexiconRepositoryError::Database)?;
        Ok(())
    }
}
