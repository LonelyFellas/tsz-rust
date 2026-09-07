use std::{sync::Arc, time::Duration};

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::platform::storage::{ObjectKey, ObjectStore};

const INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 零引用资产的宽限期。它保护的是「已 confirm 但还没保存进草稿」的那一段：
/// 管理员选完文件去开会、回来再保存，中间资产一直是零引用。
/// 与 `audio/uploads/` 的生命周期规则同为 7 天，两处一起改。
const GRACE_DAYS: i32 = 7;

/// 一轮最多回收多少条，避免突发的对象存储流量。
const BATCH: usize = 100;

/// 单次对象删除的时限。这一步跨在资产行的排他锁与一个数据库连接上，底座的 OSS operator
/// 没有配 HTTP 超时，不设时限的话一次挂死的 DELETE 会让回收在本进程内**静默**停摆
/// （`reclaim_once` 还没返回，错误分支也不会触发），同时把引用同一条资产的词义保存
/// 顶在 `FOR SHARE` 上无限期等待。
const DELETE_TIMEOUT: Duration = Duration::from_secs(30);

/// 周期性回收失去全部引用的音频资产：先删对象再删行。
///
/// 顺序不能反。先删行的话，一旦对象删除失败，这个对象就再也没有任何记录指向它——
/// `ObjectStore` 没有 `list`（`docs/object-storage-design.md` §4），`assets/` 前缀又不能挂
/// 按年龄回收的生命周期规则（那会误删正式资产），于是它成为永久且不可发现的垃圾。
/// 反过来先删对象、行没删掉，下一轮会重来一次：`delete` 是幂等的，重复删只是一次 no-op。
pub fn run_worker(pool: PgPool, storage: Option<Arc<dyn ObjectStore>>) {
    let Some(storage) = storage else {
        // 没配 audio 空间的环境根本不会产生资产，跑这个 worker 只是白占一个任务。
        return;
    };
    tokio::spawn(async move {
        loop {
            match reclaim_once(&pool, &storage).await {
                Ok(0) => {}
                Ok(deleted) => tracing::info!(deleted, "audio assets reclaimed"),
                Err(error) => tracing::error!(
                    error = %error,
                    error_kind = "audio_asset_cleanup",
                    "audio asset cleanup failed"
                ),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// 每条资产单独一个事务：`FOR UPDATE` 锁住资产行，再删对象、删行、提交。
///
/// 锁是必须的。不加锁的话，「查到零引用」与「删对象」之间有一个窗口，
/// 管理员恰好在这一刻把这条资产存进草稿，就会得到一条引用完好、对象已被删掉的资产——
/// 线上播不出声且不可恢复。保存路径的校验对同一行取 `FOR SHARE`，于是两者必然串行：
/// 要么保存先拿到锁、worker 随后看到引用而跳过，要么 worker 先删完、保存的校验报「资产不存在」。
/// 锁只在一次对象删除期间持有，且只挡住引用同一条资产的保存。
pub async fn reclaim_once(
    pool: &PgPool,
    storage: &Arc<dyn ObjectStore>,
) -> Result<usize, sqlx::Error> {
    let mut deleted = 0;
    // 删不掉的行会被 rollback 放回候选集，不记下来就会在同一轮里被反复选中，
    // 一个坏对象足以把这一轮的名额全部耗光、挡住其余资产的回收。
    let mut skipped: Vec<Uuid> = Vec::new();
    for _ in 0..BATCH {
        let mut tx = pool.begin().await?;
        let row = sqlx::query(
            r#"
            SELECT asset.id, asset.object_key
            FROM lexicon.audio_assets asset
            WHERE asset.created_at < now() - make_interval(days => $1)
              AND asset.id <> ALL($2)
              AND NOT EXISTS (
                  SELECT 1 FROM lexicon.v3_audio_asset_references reference
                  WHERE reference.asset_id = asset.id
              )
            ORDER BY asset.created_at
            LIMIT 1
            FOR UPDATE OF asset SKIP LOCKED
            "#,
        )
        .bind(GRACE_DAYS)
        .bind(&skipped)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            break;
        };

        let id: Uuid = row.get("id");
        let object_key: String = row.get("object_key");
        let Ok(key) = ObjectKey::parse(object_key) else {
            // 库里的键不合法说明写入侧出过 bug，删不了也报不出去；留着行以便排查。
            tracing::error!(
                asset_id = %id,
                error_kind = "audio_asset_key",
                "stored audio asset key is not a valid object key; skipping reclamation"
            );
            tx.rollback().await?;
            skipped.push(id);
            continue;
        };
        let deleted_object = match tokio::time::timeout(DELETE_TIMEOUT, storage.delete(&key)).await
        {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err(format!("delete timed out after {DELETE_TIMEOUT:?}")),
        };
        if let Err(error) = deleted_object {
            tracing::warn!(
                asset_id = %id,
                object_key = %key,
                error = %error,
                error_kind = "storage_delete",
                "audio asset object delete failed; row kept for the next round"
            );
            tx.rollback().await?;
            skipped.push(id);
            continue;
        }
        sqlx::query("DELETE FROM lexicon.audio_assets WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        deleted += 1;
    }
    Ok(deleted)
}
