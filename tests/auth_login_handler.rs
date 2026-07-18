//! `POST /auth/login` handler 的端到端测试（真库 + `oneshot`）。
//!
//! 验 handler 层独有的翻译：状态码映射、响应形状、**两种失败不可区分**、不泄露 hash。
//! 凭证校验的细节在 `tests/user_authenticate.rs`（service 层）。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

use tsz_rust::state::AppState;
use tsz_rust::user::repository::UserRepository;
use tsz_rust::user::service::{RegisterInput, UserService};

/// 先注册一个用户（密码 "password123"），返回 id。
async fn register_user(pool: &PgPool, phone: &str) -> uuid::Uuid {
    UserService::new(UserRepository::new(pool.clone()))
        .register(RegisterInput {
            phone: Some(phone.to_owned()),
            email: None,
            password: "password123".to_owned(),
        })
        .await
        .expect("注册应成功")
        .id
}

/// POST /auth/login，返回 (状态码, Set-Cookie 头, 响应体 JSON)。
async fn login(pool: PgPool, body: Value) -> (StatusCode, Option<String>, Value) {
    let resp = tsz_rust::router(AppState::for_test(pool))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        set_cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

// ————————————————————— 成功 —————————————————————

/// 正确凭证 → 200 + access 字段齐全、refresh 走 Set-Cookie 不进 body、不泄露 hash、refresh 已落库。
#[sqlx::test]
async fn login_returns_200_with_tokens(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;

    let (status, set_cookie, body) = login(
        pool.clone(),
        json!({ "identifier": "13800138000", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "正确凭证应 200");

    // access 侧字段（登录响应把 token 嵌在 `token` 下，profile 字段在顶层）
    assert!(
        body["token"]["access_token"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "应有非空 access_token"
    );
    // refresh token 改走 httpOnly cookie（契约 0.2），body 里任何位置都不得出现明文
    assert!(
        !body.to_string().contains("refresh_token"),
        "refresh_token 不得出现在响应 body"
    );

    // Set-Cookie 下发 refresh token，属性必须齐全（HttpOnly / SameSite / Path / Max-Age）
    let cookie = set_cookie.as_deref().expect("登录应下发 Set-Cookie");
    let value = cookie
        .strip_prefix("refresh_token=")
        .and_then(|rest| rest.split(';').next())
        .expect("Set-Cookie 应以 refresh_token= 开头");
    assert_eq!(
        value.len(),
        43,
        "refresh token 应为 43 字符 base64url，实际：{value}"
    );
    assert!(cookie.contains("HttpOnly"), "必须 HttpOnly，实际：{cookie}");
    assert!(
        cookie.contains("SameSite=Lax"),
        "必须 SameSite=Lax，实际：{cookie}"
    );
    assert!(
        cookie.contains("Path=/api/v1/auth"),
        "Path 应收窄到 /api/v1/auth，实际：{cookie}"
    );
    assert!(
        cookie.contains("Max-Age=2592000"),
        "Max-Age 应为 30 天（2592000 秒），实际：{cookie}"
    );
    assert!(
        !cookie.contains("Secure"),
        "for_test cookie_secure=false，不应带 Secure，实际：{cookie}"
    );

    assert_eq!(body["token"]["token_type"], "Bearer");
    assert_eq!(
        body["token"]["expires_in"], 900,
        "for_test 的 access TTL=15min=900s"
    );

    // access_token 看着像 JWT（三段）
    assert_eq!(
        body["token"]["access_token"]
            .as_str()
            .unwrap()
            .split('.')
            .count(),
        3,
        "access_token 应是三段式 JWT"
    );

    // role 序列化必须小写，与 JWT 里的 role claim（as_str）一致，别漂成 "Student"
    assert_eq!(
        body["last_active_role"], "student",
        "last_active_role 应小写"
    );
    assert_eq!(body["roles"][0], "student", "roles 元素应小写");

    // 绝不泄露 hash
    assert!(
        !body.to_string().contains("$2b$"),
        "响应不得含 bcrypt hash 片段"
    );

    // refresh 确实落库了（该用户名下恰好一行）
    let count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM refresh_tokens WHERE user_id = $1",
        user_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, Some(1), "登录应发一枚 refresh token 并落库");
}

// ————————————————————— 失败：两种都 401 且一致 —————————————————————

/// 密码错 → 401 invalid credentials。
#[sqlx::test]
async fn login_wrong_password_is_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;

    let (status, set_cookie, body) = login(
        pool,
        json!({ "identifier": "13800138000", "password": "wrong" }),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"].as_str(), Some("invalid credentials"));
    assert!(set_cookie.is_none(), "登录失败不得下发 cookie");
}

/// 未知用户 → 401 invalid credentials —— 和密码错**逐字节一致**（不可区分）。
#[sqlx::test]
async fn login_unknown_user_is_identical_401(pool: PgPool) {
    register_user(&pool, "13800138000").await;

    // 密码错（用户存在）
    let (s_wrong, _, b_wrong) = login(
        pool.clone(),
        json!({ "identifier": "13800138000", "password": "wrong" }),
    )
    .await;
    // 用户不存在
    let (s_unknown, _, b_unknown) = login(
        pool,
        json!({ "identifier": "19999999999", "password": "password123" }),
    )
    .await;

    assert_eq!(s_wrong, StatusCode::UNAUTHORIZED);
    assert_eq!(s_unknown, StatusCode::UNAUTHORIZED);
    assert_eq!(s_wrong, s_unknown, "两种失败状态码必须一致");
    assert_eq!(b_wrong, b_unknown, "两种失败响应体必须一致（不可区分）");
}

// ————————————————————— 禁用账号 —————————————————————

/// 密码正确但账号被禁 → 403。
#[sqlx::test]
async fn login_disabled_account_is_403(pool: PgPool) {
    let user_id = register_user(&pool, "13800138000").await;
    sqlx::query!(
        "UPDATE users SET status = 'disabled' WHERE id = $1",
        user_id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, set_cookie, _) = login(
        pool,
        json!({ "identifier": "13800138000", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "密码对+账号禁用应 403");
    assert!(set_cookie.is_none(), "禁用账号不得下发 cookie");
}

// ————————————————————— 角色列表 —————————————————————

/// 登录响应的 `roles` 应反映**真实角色表**（可多个），而非只回 last_active_role。
/// 注册默认给 student；再追加 teacher → 响应应同时含 student 与 teacher。
/// 这条能区分「查 user_roles 表」与「vec![last_active_role] 假实现」——后者只会回 1 个。
#[sqlx::test]
async fn login_returns_all_roles_from_table(pool: PgPool) {
    let uid = register_user(&pool, "13800138000").await; // 默认 student
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
        .bind(uid)
        .bind("teacher")
        .execute(&pool)
        .await
        .expect("追加 teacher 角色应成功");

    let (status, _, body) = login(
        pool,
        json!({ "identifier": "13800138000", "password": "password123" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let roles: Vec<&str> = body["roles"]
        .as_array()
        .expect("应有 roles 数组")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        roles.contains(&"student"),
        "roles 应含 student，实际 {roles:?}"
    );
    assert!(
        roles.contains(&"teacher"),
        "roles 应含 teacher（证明查的是真实角色表而非 last_active_role），实际 {roles:?}"
    );
    assert_eq!(roles.len(), 2, "应恰好两个角色，实际 {roles:?}");
}
