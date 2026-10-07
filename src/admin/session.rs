//! admin 侧 refresh token 会话机制——web 侧 `src/session/` 的平移，独立成份：
//! 表（admin_refresh_tokens）、类型、错误全与 web 分开，两域互不牵连。
//!
//! 同一管理员支持多端登录，各会话独立轮换。与 web 不同，admin 保留
//! **Q8 绝对死线**：轮换继承最初的 expires_at，不延长登录有效期。
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
    // 被主动撤销（logout / 全端撤销 / 重放连坐）
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
    #[error("authentication changed")]
    AuthenticationChanged,
}

/// repository 负责会话 SQL 与账号行锁；哈希及重放窗口由 service 处理。
pub struct AdminRefreshTokenRepository {
    pool: PgPool,
}

impl AdminRefreshTokenRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 无状态校验的原始插入；登录签发应使用 `insert_for_active_admin`。
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

    /// 锁定账号并校验认证时读取的版本和状态，再签发独立会话。
    /// 与改密、禁用和全端退出使用同一账号行锁，避免迟到登录绕过安全变更。
    pub async fn insert_for_active_admin(
        &self,
        row: NewAdminRefreshToken,
        security_version: i64,
    ) -> Result<(), AdminRefreshTokenError> {
        let mut tx = self.pool.begin().await?;

        let current = sqlx::query_as::<_, (i64, bool)>(
            "SELECT security_version, status = 'active' FROM admins WHERE id = $1 FOR UPDATE",
        )
        .bind(row.admin_id)
        .fetch_optional(&mut *tx)
        .await?;
        if current != Some((security_version, true)) {
            return Err(AdminRefreshTokenError::AuthenticationChanged);
        }

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
        Ok(())
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
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT a.id FROM admins a JOIN admin_refresh_tokens r ON r.admin_id = a.id WHERE r.token_hash = $1 FOR UPDATE OF a")
            .bind(old_hash).fetch_optional(&mut *tx).await?;
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
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
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

    /// UPDATE 取得账号行锁后，版本递增与 refresh 吊销在同一事务提交。
    pub async fn revoke_all_for_version(
        &self,
        admin_id: &Uuid,
        security_version: i64,
    ) -> Result<(), AdminRefreshTokenError> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE admins SET security_version = security_version + 1, updated_at = NOW() \
             WHERE id = $1 AND security_version = $2 AND status = 'active'",
        )
        .bind(admin_id)
        .bind(security_version)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(AdminRefreshTokenError::AuthenticationChanged);
        }
        sqlx::query("UPDATE admin_refresh_tokens SET revoked_at = NOW() WHERE admin_id = $1 AND revoked_at IS NULL")
            .bind(admin_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 重放连坐用：吊销该 admin 全部未吊销行，返回准确计数。
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

    /// 为已认证的管理员签发独立 refresh 会话，保留其他设备的会话。
    pub async fn issue(
        &self,
        admin_id: &Uuid,
        security_version: i64,
    ) -> Result<IssuedAdminRefresh, AdminSessionError> {
        let plaintext = generate_token_plaintext();
        let token_hash = hash_token(&plaintext);
        let expires_at = Utc::now() + self.refresh_ttl;

        self.repository
            .insert_for_active_admin(
                NewAdminRefreshToken {
                    id: Uuid::now_v7(),
                    admin_id: *admin_id,
                    token_hash,
                    expires_at,
                },
                security_version,
            )
            .await?;
        Ok(IssuedAdminRefresh {
            plaintext,
            expires_at,
        })
    }

    /// /admin/refresh 核心：哈希明文 → 原子 CAS 消费旧枚 + 落库新枚（继承死线，Q8）
    /// → 返回属主 + 新枚。不查账号状态（那是 handler 的活）。
    ///
    /// CAS 落空时验尸区分「无效」与「重放」，判据只有一条：已轮换（rotated_at 非空）
    /// **且未吊销**。其余失败（垃圾串/过期/已吊销）一律只回 401、不动任何行。窗口外重放 → 该 admin 全量连坐
    /// （包括其他设备的独立会话）；窗口内按丢包重试宽待，不连坐不铸币。
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

    /// 同事务撤销全部 access / refresh。迟到的旧版本请求不能撤销新登录。
    pub async fn logout_all(
        &self,
        admin_id: &Uuid,
        security_version: i64,
    ) -> Result<(), AdminSessionError> {
        self.repository
            .revoke_all_for_version(admin_id, security_version)
            .await?;
        Ok(())
    }

    /// refresh handler「轮换压轴」次序的地基：先只读定位属主 → 账号/状态都验过了
    /// → 最后才 rotate。peek 绝不消费；已撤销凭证先返回无效，避免旧登录被误报为账号故障。
    pub async fn peek_admin_id(&self, plaintext: &str) -> Result<Option<Uuid>, AdminSessionError> {
        let token_hash = hash_token(plaintext);
        Ok(self
            .repository
            .find_by_hash(&token_hash)
            .await?
            .filter(|t| t.revoked_at.is_none())
            .map(|t| t.admin_id))
    }
}

// crypto（generate_token_plaintext / hash_token）已上提到 platform，web 与 admin 两个
// 会话域共用一份；纯函数属性测试也随之集中在 src/platform/utils.rs。
