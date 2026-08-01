//! `GET /api/v1/admin/profile`（admin 身份探针）端到端测试。
//!
//! 本端点是 admin 域第一个 **Bearer 鉴权**端点（此前全走 cookie/公开），驱动两块新地基：
//! `AdminAuth` 提取器（`src/admin/extract.rs`，用 `state.admin_token_manager.parse`，
//! realm 隔离由 aud 校验兑现）+ `ADMIN_MOUNT`（`/api/v1/admin`）新挂载组。
//!
//! 编排契约（本文件钉死，admin-design §7/§12）：
//!
//! ```text
//! ① 缺头 / 垃圾 / 过期 / web realm token ⇒ 401（提取器层，不碰库）
//! ② token 有效但 admin 行已消失        ⇒ 401（视为过期会话，非 500）
//! ③ disabled                           ⇒ 403（对齐 admin refresh 的拍板）
//! ④ must_change_password = true        ⇒ 403 + code（前端跳改密页的硬契约；
//!    守卫内联在 handler——唯一守卫组端点，middleware 等多端点批次再抽）
//! ⑤ 成功 ⇒ 200 {id, phone, display_name, role, permissions}
//!    permissions 恒返全量 12 个菜单 key 死数据（Q4/Q10），顺序即侧栏顺序；
//!    **只在 profile 下发、login 不带**（2026-07-26 拍板：F5 会话恢复走
//!    refresh→profile，login 一次性下发撑不住该链路；login 侧有防回潮钉子）
//! ```
//!
//! 刻意**不查** locked_until（有正向测试钉死）：锁定语义 = 挡新登录/refresh 轮换
//! （防爆破），不打断已认证的短命 access token——否则攻击者可用错码轰炸把在线
//! 管理员打下线（DoS）。若日后拍板改变，此注释、该测试与 handler 三处同步改。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Duration;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::admin::{AdminRepository, AdminRole, NewAdmin};
use tsz_rust::auth::{Realm, TokenManager};
use tsz_rust::state::AppState;

/// 侧栏菜单 key 的镜像常量（与 `admin/handler.rs` 的 `MENU_PERMISSIONS` 独立抄写，
/// **刻意不 import**——import 会让断言变成恒真式。顺序即侧栏顺序，是契约的一部分；
/// 改实现常量必须同步改这里（对照前端 @tsz/types MenuPermission 联合类型）。
const EXPECTED_MENU_KEYS: [&str; 12] = [
    "users.access",
    "classes.access",
    "words.access",
    "customdict.access",
    "sentences.access",
    "wordlists.access",
    "customwordlist.access",
    "tasks.access",
    "reviews.access",
    "teacherapply.access",
    "comments.access",
    "coins.access",
];

/// 造一个 active 管理员，返回 id。
async fn seed_admin(pool: &PgPool, role: AdminRole, must_change: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role,
            must_change_password: must_change,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

/// 用 AppState 里的 admin 签名器给某 admin 铸一枚有效 access token。
fn admin_token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签 admin token 应成功")
}

/// GET /api/v1/admin/profile；`bearer` 为 None 时不带 Authorization 头。
/// 返回 (状态码, body 文本)。
async fn get_profile(state: &AppState, bearer: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("GET")
        .uri("/api/v1/admin/profile");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let resp = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

// ============================== ⑤ 成功路径 ==============================

#[sqlx::test]
async fn valid_token_returns_full_profile(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let token = admin_token(&state, id, AdminRole::SuperAdmin);

    let (status, body) = get_profile(&state, Some(&token)).await;

    assert_eq!(status, StatusCode::OK, "有效 token 应 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["id"].as_str(), Some(id.to_string().as_str()));
    assert_eq!(json["phone"].as_str(), Some(id.to_string().as_str()));
    assert_eq!(json["display_name"].as_str(), Some("测试管理员"));
    assert_eq!(
        json["role"].as_str(),
        Some("super_admin"),
        "字段名统一 role（Q11），值为 snake_case"
    );

    // permissions：恰 12 个、顺序逐位对（顺序即侧栏顺序，是契约的一部分）。
    let perms: Vec<&str> = json["permissions"]
        .as_array()
        .expect("permissions 应恒为数组")
        .iter()
        .map(|v| v.as_str().expect("permission key 应为字符串"))
        .collect();
    assert_eq!(
        perms, EXPECTED_MENU_KEYS,
        "permissions 应为全量 12 个菜单 key 且顺序逐位一致"
    );
}

#[sqlx::test]
async fn plain_admin_gets_same_full_permissions(pool: PgPool) {
    // Q10 无 RBAC：permissions 是死数据，不随身份变化——admin 与 super_admin 拿到同一份。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let token = admin_token(&state, id, AdminRole::Admin);

    let (status, body) = get_profile(&state, Some(&token)).await;

    assert_eq!(status, StatusCode::OK, "普通 admin 应同样 200：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["role"].as_str(), Some("admin"));
    assert_eq!(
        json["permissions"].as_array().map(Vec::len),
        Some(EXPECTED_MENU_KEYS.len()),
        "普通 admin 的 permissions 也应是全量死数据"
    );
}

#[sqlx::test]
async fn response_leaks_no_sensitive_or_extra_fields(pool: PgPool) {
    // 防「序列化整个 Admin 本体」：响应必须是手挑的 5 个字段，一个不多。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let token = admin_token(&state, id, AdminRole::Admin);

    let (_, body) = get_profile(&state, Some(&token)).await;
    let json: Value = serde_json::from_str(&body).unwrap();
    let obj = json.as_object().expect("响应应为 JSON 对象");

    for forbidden in [
        "password_hash",
        "status",
        "must_change_password",
        "failed_login_count",
        "locked_until",
        "created_at",
        "updated_at",
    ] {
        assert!(
            !obj.contains_key(forbidden),
            "响应不得含 {forbidden} 字段：{body}"
        );
    }
    assert_eq!(obj.len(), 5, "响应应恰为 5 个字段：{body}");
}

#[sqlx::test]
async fn locked_admin_still_gets_200(pool: PgPool) {
    // 锁定不打断已认证会话：锁定语义只挡新登录/refresh 轮换（防爆破）。
    // 若 profile 也 423，攻击者可对 login 狂输错码把正在后台干活的管理员打下线（DoS）。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let token = admin_token(&state, id, AdminRole::Admin);
    sqlx::query!(
        "UPDATE admins SET locked_until = NOW() + interval '10 minutes' WHERE id = $1",
        id
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = get_profile(&state, Some(&token)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "锁定中的 admin 持有效 access token 应仍可读 profile：{body}"
    );
}

// ============================== ① 提取器层 401 ==============================

#[sqlx::test]
async fn missing_authorization_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let (status, _) = get_profile(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "缺 Authorization 应 401");
}

#[sqlx::test]
async fn garbage_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let (status, _) = get_profile(&state, Some("not-a-jwt")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "垃圾 token 应 401");
}

#[sqlx::test]
async fn expired_token_is_401(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    // 同 secret、同 realm、负 TTL：签出一枚已过期的合法签名 token。
    let expired = TokenManager::new("test-admin-secret", Realm::Admin, Duration::seconds(-3600))
        .generate(id, AdminRole::Admin.as_str())
        .unwrap();

    let (status, _) = get_profile(&state, Some(&expired)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "过期 token 应 401");
}

#[sqlx::test]
async fn web_realm_token_is_401(pool: PgPool) {
    // 跨 realm 隔离端到端（admin-design §14 测试策略点名）：web 签名器发的 token
    // 打 admin 端点必须 401——两把 secret 不同 + aud 不同，双防线任一都该拦住。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let web_token = state
        .token_manager
        .generate(id, "student")
        .expect("签 web token 应成功");

    let (status, _) = get_profile(&state, Some(&web_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "web realm token 应 401");
}

#[sqlx::test]
async fn unknown_role_claim_is_401(pool: PgPool) {
    // role claim 认不出 = token 伪造或版本漂移，提取器 fail-closed 成 401（不放行、不 500）。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let bad_role_token = state
        .admin_token_manager
        .generate(id, "intern")
        .expect("签 token 应成功");

    let (status, _) = get_profile(&state, Some(&bad_role_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未知 role claim 应 401");
}

// ============================== ② 行已消失 ==============================

#[sqlx::test]
async fn token_valid_but_admin_row_gone_is_401(pool: PgPool) {
    // 会话期内账号被删：视为过期会话（401），绝不能 500。
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let token = admin_token(&state, id, AdminRole::Admin);
    sqlx::query!("DELETE FROM admins WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, _) = get_profile(&state, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "admin 行已删应 401");
}

// ============================== ③ disabled ==============================

#[sqlx::test]
async fn disabled_admin_is_403(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, false).await;
    let token = admin_token(&state, id, AdminRole::Admin);
    sqlx::query!("UPDATE admins SET status = 'disabled' WHERE id = $1", id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, body) = get_profile(&state, Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "禁用账号应 403：{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(
        json.as_object().is_some_and(|o| !o.contains_key("code")),
        "禁用的 403 不带 code——code 键专属 must_change 契约：{body}"
    );
}

// ============================== ④ must_change 守卫 ==============================

#[sqlx::test]
async fn must_change_password_is_403_with_code(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = seed_admin(&pool, AdminRole::Admin, true).await;
    let token = admin_token(&state, id, AdminRole::Admin);

    let (status, body) = get_profile(&state, Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "被强制改密应 403：{body}");
    // 逐字节钉死：code 是前端跳改密页的硬契约（admin-design §7 原文文案）。
    assert_eq!(
        body, r#"{"error":"password change required","code":"must_change_password"}"#,
        "403 body 应逐字节等于契约文案"
    );
}
