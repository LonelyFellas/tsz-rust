//! 同一次刷新在响应丢失后续接，数据库不保存凭据明文。
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    platform::hash_token,
};

pub fn request_id(headers: &HeaderMap) -> Result<Option<Uuid>, AppError> {
    let Some(value) = headers.get("x-refresh-request-id") else {
        return Ok(None);
    };
    let id = value
        .to_str()
        .ok()
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|id| id.get_version_num() == 4)
        .ok_or_else(|| {
            AppError::bad_request(ErrorCode::ValidationFailed, "invalid refresh request id")
        })?;
    Ok(Some(id))
}

/// 旧页面没有原刷新键，不能把不同键的旧 Cookie 当成恶意重放而连坐撤销。
pub fn current_only(headers: &HeaderMap, request_id: Option<Uuid>) -> Result<bool, AppError> {
    match headers.get("x-refresh-current-only") {
        None => Ok(false),
        Some(value) if value == "v1" && request_id.is_some() => Ok(true),
        Some(_) => Err(AppError::bad_request(
            ErrorCode::ValidationFailed,
            "invalid current-only refresh request",
        )),
    }
}

pub enum Recovery {
    Ready {
        owner: Uuid,
        plaintext: String,
        expires_at: DateTime<Utc>,
    },
    Invalid,
    Replay,
}

#[derive(Clone, Copy)]
pub enum Realm {
    Web,
    Admin,
}

impl Realm {
    fn tables(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Web => ("refresh_tokens", "users", "user_id"),
            Self::Admin => ("admin_refresh_tokens", "admins", "admin_id"),
        }
    }
}

pub async fn rotate(
    pool: &PgPool,
    realm: Realm,
    plaintext: &str,
    request_id: Uuid,
    ttl: Duration,
) -> Result<Recovery, sqlx::Error> {
    // SQL 标识符仅来自封闭 Realm 枚举；所有请求数据均通过 bind 传入。
    let (table, accounts, owner_column) = realm.tables();
    let token_hash = hash_token(plaintext);
    let request_hash = hash_token(&request_id.to_string());
    let mut tx = pool.begin().await?;
    // 与退出、改密、禁用统一先锁账号；之后使用实际数据库时钟核验 token。
    let owner = sqlx::query_as::<_, (Uuid, bool)>(AssertSqlSafe(format!(
        "SELECT a.id, a.status = 'active' FROM {accounts} a JOIN {table} r ON r.{owner_column}=a.id WHERE r.token_hash=$1 FOR UPDATE OF a"
    ))).bind(&token_hash).fetch_optional(&mut *tx).await?;
    let Some((owner, true)) = owner else {
        return Ok(Recovery::Invalid);
    };
    if matches!(realm, Realm::Web)
        && crate::account_deletion::is_effective_in(&mut tx, owner).await?
    {
        return Ok(Recovery::Invalid);
    }
    let (rotated, revoked, expires_at, now, prior_request, replacement) = sqlx::query_as::<_, (
        bool, bool, DateTime<Utc>, DateTime<Utc>, Option<String>, Option<String>
    )>(AssertSqlSafe(format!(
        "SELECT rotated_at IS NOT NULL, revoked_at IS NOT NULL, expires_at, clock_timestamp(), rotation_request_hash, replacement_token_hash FROM {table} WHERE token_hash=$1"
    ))).bind(&token_hash).fetch_one(&mut *tx).await?;
    if revoked || expires_at <= now {
        return Ok(Recovery::Invalid);
    }

    let ready = |plaintext, expires_at| Recovery::Ready {
        owner,
        plaintext,
        expires_at,
    };
    if rotated {
        if prior_request.as_deref() != Some(request_hash.as_str()) {
            return Ok(Recovery::Replay);
        }
        let new_plaintext = derive(plaintext, request_id, table);
        if replacement.as_deref() != Some(hash_token(&new_plaintext).as_str()) {
            return Ok(Recovery::Invalid);
        }
        let expiry = sqlx::query_scalar::<_, DateTime<Utc>>(AssertSqlSafe(format!(
            "SELECT expires_at FROM {table} WHERE token_hash=$1 AND {owner_column}=$2 AND rotated_at IS NULL AND revoked_at IS NULL AND expires_at > clock_timestamp()"
        ))).bind(&replacement).bind(owner).fetch_optional(&mut *tx).await?;
        return Ok(expiry.map_or(Recovery::Invalid, |expiry| ready(new_plaintext, expiry)));
    }

    // Set-Cookie 已到达但 JSON 丢失：当前凭据就是同次请求的后继，不能再轮换。
    let already_issued: bool = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT EXISTS(SELECT 1 FROM {table} WHERE replacement_token_hash=$1 AND rotation_request_hash=$2 AND {owner_column}=$3 AND revoked_at IS NULL AND expires_at > clock_timestamp())"
    ))).bind(&token_hash).bind(&request_hash).bind(owner).fetch_one(&mut *tx).await?;
    if already_issued {
        return Ok(ready(plaintext.to_owned(), expires_at));
    }

    let new_plaintext = derive(plaintext, request_id, table);
    let new_hash = hash_token(&new_plaintext);
    let expiry = match realm {
        Realm::Admin => expires_at,
        Realm::Web => now + ttl,
    };
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE {table} SET rotated_at=clock_timestamp(), rotation_request_hash=$2, replacement_token_hash=$3 WHERE token_hash=$1"
    ))).bind(&token_hash).bind(request_hash).bind(&new_hash).execute(&mut *tx).await?;
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {table} (id, {owner_column}, token_hash, expires_at) VALUES ($1,$2,$3,$4)"
    )))
    .bind(Uuid::now_v7())
    .bind(owner)
    .bind(new_hash)
    .bind(expiry)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ready(new_plaintext, expiry))
}

fn derive(plaintext: &str, request_id: Uuid, realm: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(plaintext.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"tsz:refresh-recovery:v1:");
    mac.update(realm.as_bytes());
    mac.update(request_id.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_only_requires_the_supported_mode_and_a_valid_request_key() {
        let mut headers = HeaderMap::new();
        let key = Uuid::parse_str("283e8512-4e3c-4c59-a02b-2f383a97e9ae").unwrap();
        assert!(!current_only(&headers, None).unwrap());
        headers.insert("x-refresh-current-only", "v1".parse().unwrap());
        assert!(current_only(&headers, None).is_err());
        assert!(current_only(&headers, Some(key)).unwrap());
        headers.insert("x-refresh-current-only", "unknown".parse().unwrap());
        assert!(current_only(&headers, Some(key)).is_err());
    }
}
