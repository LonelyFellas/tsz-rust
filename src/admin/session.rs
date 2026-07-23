//! admin 侧 refresh token 会话机制——web 侧 `src/session/` 的平移，独立成份：
//! 表（admin_refresh_tokens）、类型、错误全与 web 分开，两域互不牵连。
//!
//! 与 web 侧的两处**刻意偏离**（admin-design.md，契约钉在 tests/admin_session_*.rs）：
//!   - **Q1 严格单登录**：`issue` 前先 `revoke_all_by_admin_id`——后台账号不允许
//!     多处在线，重新登录即挤掉旧会话。
//!   - **Q8 绝对死线**：`rotate` 的新枚**继承**被消费旧枚的 expires_at、不重算
//!     now+ttl——轮换只换凭证不续命，到期 401 必重走登录。（web 是滑动续期。）
//!
//! 落库契约同 web：存哈希不存明文、每次明文都不同、rotate 原子换新、
//! 重放检测带 20s 宽限窗口（窗口内不连坐不铸币）。

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::TokenError;
use crate::platform::{generate_token_plaintext, hash_token};

// ————————————————————— model —————————————————————

/// admin_refresh_tokens 表一行，query_as! 直接映射。
#[derive(Debug)]
pub struct AdminRefreshToken {
    pub id: Uuid,
    pub admin_id: Uuid,
    pub token_hash: String,
    // 被主动撤销（logout / 单登录清场 / 重放连坐）
    pub revoked_at: Option<DateTime<Utc>>,
    // 被轮换消费，已换成新枚
    pub rotated_at: Option<DateTime<Utc>>,
    // 本次登录的绝对死线（Q8：轮换只继承、不重算）
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct NewAdminRefreshToken {
    pub id: Uuid,
    pub admin_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

// ————————————————————— repository —————————————————————

#[derive(Debug, thiserror::Error)]
pub enum AdminRefreshTokenError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// ⚠️ 边界：repository 只搬 SQL——不做哈希（token_hash 原样存）、不判过期语义
/// （过期行照样插/查，过期只在 consume 的 WHERE 里挡）、不执法单登录
/// （Q1 的 issue 前 revoke_all 是 service 编排的活）。
pub struct AdminRefreshTokenRepository {
    pool: PgPool,
}

impl AdminRefreshTokenRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// issue 用：插入一行。
    pub async fn insert(
        &self,
        row: NewAdminRefreshToken,
    ) -> Result<AdminRefreshToken, AdminRefreshTokenError> {
        let row = sqlx::query_as!(
            AdminRefreshToken,
            r#"
            INSERT INTO admin_refresh_tokens (id, admin_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id, admin_id, token_hash, revoked_at, rotated_at, expires_at, created_at
            "#,
            row.id,
            row.admin_id,
            row.token_hash,
            row.expires_at
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    /// Q1 严格单登录的**原子**签发：同一事务内先对 admins 行加 `FOR UPDATE` 锁，
    /// 串行化同一 admin 的并发 issue，再吊销其全部活跃会话、插入新枚。返回被挤掉的会话数。
    ///
    /// 为什么必须是行锁而非仅包事务：READ COMMITTED 下两个并发事务的 revoke_all 互相
    /// 看不见对方尚未提交的新行，只包事务仍会各插一枚（活跃数=2，破 Q1）。`FOR UPDATE`
    /// 让第二个 issue 阻塞到第一个提交后，其 revoke_all 才能看见并吊销第一枚，
    /// 最终活跃数恒为 1。锁 admins 行只串行化「同一 admin 的并发 issue」——普通读不加锁、
    /// 不同 admin 各锁各行，无额外争用。
    pub async fn revoke_all_and_insert(
        &self,
        row: NewAdminRefreshToken,
    ) -> Result<u64, AdminRefreshTokenError> {
        let mut tx = self.pool.begin().await?;

        // 取锁：同一 admin 的并发 issue 在此排队（admin 行必存在——login 刚认证过；
        // 万一被并发删，后续 INSERT 的 FK 会挡）。只为拿锁，不用结果。
        sqlx::query_scalar!(
            r#"SELECT id FROM admins WHERE id = $1 FOR UPDATE"#,
            row.admin_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        let displaced = sqlx::query!(
            r#"UPDATE admin_refresh_tokens SET revoked_at = NOW()
               WHERE admin_id = $1 AND revoked_at IS NULL"#,
            row.admin_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();

        sqlx::query!(
            r#"INSERT INTO admin_refresh_tokens (id, admin_id, token_hash, expires_at)
               VALUES ($1, $2, $3, $4)"#,
            row.id,
            row.admin_id,
            row.token_hash,
            row.expires_at
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(displaced)
    }

    /// rotate 验尸 / peek 用：按哈希查行（命中唯一索引 admin_refresh_tokens_hash）。
    pub async fn find_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<AdminRefreshToken>, AdminRefreshTokenError> {
        let row = sqlx::query_as!(
            AdminRefreshToken,
            r#"
            SELECT id, admin_id, token_hash, revoked_at, rotated_at, expires_at, created_at
            FROM admin_refresh_tokens WHERE token_hash = $1
            "#,
            token_hash
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// 原子轮换（Q8）：CTE 单条 SQL——CAS 消费旧枚（未轮换/未吊销/未过期才盖
    /// rotated_at）+ 新枚落库**继承**旧枚 expires_at，同生共死；INSERT 撞唯一索引
    /// 时整条语句原子回滚，旧枚不会留在已轮换态。expires_at 不是参数——继承在
    /// DB 层完成，service 想传错都没有入口。
    /// 抢到 → Ok(Some((属主 admin_id, 继承的死线)))；落空 → Ok(None)。
    pub async fn consume_and_insert(
        &self,
        old_hash: &str,
        new_hash: &str,
        new_id: Uuid,
    ) -> Result<Option<(Uuid, DateTime<Utc>)>, AdminRefreshTokenError> {
        let row = sqlx::query!(
            r#"
            WITH consumed AS (
                UPDATE admin_refresh_tokens SET rotated_at = NOW()
                WHERE token_hash = $1 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > NOW()
                RETURNING admin_id, expires_at
            )
            INSERT INTO admin_refresh_tokens (id, admin_id, token_hash, expires_at)
            SELECT $3::uuid, admin_id, $2::text, expires_at FROM consumed
            RETURNING admin_id AS "admin_id!", expires_at AS "expires_at!"
            "#,
            old_hash,
            new_hash,
            new_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| (r.admin_id, r.expires_at)))
    }

    /// logout 用：按哈希吊销，幂等（AND revoked_at IS NULL 守卫，不刷新原吊销时刻），
    /// 返回影响行数。
    pub async fn revoke_by_hash(&self, token_hash: &str) -> Result<u64, AdminRefreshTokenError> {
        let row = sqlx::query!(
            r#"
            UPDATE admin_refresh_tokens SET revoked_at = NOW()
            WHERE token_hash = $1 AND revoked_at IS NULL
            "#,
            token_hash
        )
        .execute(&self.pool)
        .await?;
        Ok(row.rows_affected())
    }

    /// 单登录清场（issue 前）/ 重放连坐用：吊销该 admin 全部未吊销行，返回准确计数。
    pub async fn revoke_all_by_admin_id(
        &self,
        admin_id: &Uuid,
    ) -> Result<u64, AdminRefreshTokenError> {
        let row = sqlx::query!(
            r#"
            UPDATE admin_refresh_tokens SET revoked_at = NOW()
            WHERE admin_id = $1 AND revoked_at IS NULL
            "#,
            admin_id
        )
        .execute(&self.pool)
        .await?;
        Ok(row.rows_affected())
    }
}

// ————————————————————— service —————————————————————

#[derive(Debug, thiserror::Error)]
pub enum AdminSessionError {
    /// 对外统一的不可区分错误：垃圾串/过期/已吊销/重放全走这一个变体和文案，
    /// 不告诉攻击者「这枚曾经有效」。
    #[error("invalid refresh token")]
    InvalidRefreshToken,
    #[error(transparent)]
    Repository(#[from] AdminRefreshTokenError),
    #[error("signing error: {0}")]
    Signing(TokenError),
}

pub struct IssuedAdminRefresh {
    pub plaintext: String,
    pub expires_at: DateTime<Utc>,
}

/// 刻意**不** derive Debug——含新枚明文，防止顺手进日志/expect 输出。
/// 测试侧 expect_err 前一律 `.map(drop)`。
pub struct RotatedAdminRefresh {
    pub admin_id: Uuid,
    pub refresh: IssuedAdminRefresh,
}

/// 重放宽限窗口：rotated_at 距今在窗口内的重放按丢包重试宽待——401 但不连坐、
/// 不铸币。生产值改动要同步 tests/admin_session_reuse_detection.rs 的镜像常量。
const REPLAY_GRACE: Duration = Duration::seconds(20);

pub struct AdminSessionService {
    repository: AdminRefreshTokenRepository,
    refresh_ttl: Duration, // config（ADMIN_REFRESH_TTL_DAYS）传入
}

impl AdminSessionService {
    pub fn new(repository: AdminRefreshTokenRepository, refresh_ttl: Duration) -> Self {
        Self {
            repository,
            refresh_ttl,
        }
    }

    /// login 调用：为 admin 发一枚 refresh。
    /// **Q1 严格单登录**：吊销该 admin 全部既有会话（重新登录即挤掉旧会话）+ 落库新枚，
    /// 由 `revoke_all_and_insert` 在一个事务内带 admins 行锁**原子**完成——并发登录也
    /// 恒余一枚活跃。任意时刻活跃会话数 ≤ 1。
    pub async fn issue(&self, admin_id: &Uuid) -> Result<IssuedAdminRefresh, AdminSessionError> {
        let plaintext = generate_token_plaintext();
        let token_hash = hash_token(&plaintext);
        let expires_at = Utc::now() + self.refresh_ttl;

        let displaced = self
            .repository
            .revoke_all_and_insert(NewAdminRefreshToken {
                id: Uuid::now_v7(),
                admin_id: *admin_id,
                token_hash,
                expires_at,
            })
            .await?;
        if displaced > 0 {
            tracing::info!(admin_id = %admin_id, displaced, "admin re-login displaced prior sessions");
        }

        Ok(IssuedAdminRefresh {
            plaintext,
            expires_at,
        })
    }

    /// /admin/refresh 核心：哈希明文 → 原子 CAS 消费旧枚 + 落库新枚（继承死线，Q8）
    /// → 返回属主 + 新枚。不查账号状态（那是 handler 的活）。
    ///
    /// CAS 落空时验尸区分「无效」与「重放」，判据只有一条：已轮换（rotated_at 非空）
    /// **且未吊销**。其余失败（垃圾串/过期/已吊销——含被单登录挤掉的旧枚，那是
    /// 自己人不是攻击）一律只回 401、不动任何行。窗口外重放 → 该 admin 全量连坐
    /// （单登录下即链上现存的那枚新 token）；窗口内按丢包重试宽待，不连坐不铸币。
    pub async fn rotate(&self, plaintext: &str) -> Result<RotatedAdminRefresh, AdminSessionError> {
        let token_hash = hash_token(plaintext);
        let new_plaintext = generate_token_plaintext();
        let new_hash = hash_token(&new_plaintext);

        if let Some((admin_id, inherited_expires_at)) = self
            .repository
            .consume_and_insert(&token_hash, &new_hash, Uuid::now_v7())
            .await?
        {
            return Ok(RotatedAdminRefresh {
                admin_id,
                refresh: IssuedAdminRefresh {
                    plaintext: new_plaintext,
                    // 用 DB 返回的继承值（微秒精度），不用 Rust 侧重算——轮换不续命
                    expires_at: inherited_expires_at,
                },
            });
        }

        // CAS 未抢到 → 验尸
        if let Some(token) = self.repository.find_by_hash(&token_hash).await?
            && let Some(rotated_at) = token.rotated_at
            && token.revoked_at.is_none()
        {
            if Utc::now() - rotated_at < REPLAY_GRACE {
                // 窗口内：可能是丢包重试——不连坐、不发新枚，对外仍是同一个 401
                return Err(AdminSessionError::InvalidRefreshToken);
            }
            // 窗口外：已轮换且未吊销 = 重放 → 该 admin 全量连坐吊销
            let n = self
                .repository
                .revoke_all_by_admin_id(&token.admin_id)
                .await?;
            tracing::warn!(
                admin_id = %token.admin_id, revoked = n,
                "admin refresh token replay detected; all sessions revoked"
            );
        }

        Err(AdminSessionError::InvalidRefreshToken)
    }

    /// /admin/logout：哈希明文 → revoke_by_hash。幂等，永远 Ok（不泄露 token 是否存在）。
    pub async fn logout(&self, plaintext: &str) -> Result<(), AdminSessionError> {
        let token_hash = hash_token(plaintext);
        self.repository.revoke_by_hash(&token_hash).await?;
        Ok(())
    }

    /// refresh handler「轮换压轴」次序的地基：先只读定位属主 → 账号/状态都验过了
    /// → 最后才 rotate。peek 绝不消费。
    pub async fn peek_admin_id(&self, plaintext: &str) -> Result<Option<Uuid>, AdminSessionError> {
        let token_hash = hash_token(plaintext);
        Ok(self
            .repository
            .find_by_hash(&token_hash)
            .await?
            .map(|t| t.admin_id))
    }
}

// crypto（generate_token_plaintext / hash_token）已上提到 platform，web 与 admin 两个
// 会话域共用一份；纯函数属性测试也随之集中在 src/platform/utils.rs。
