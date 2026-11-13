//! 语体/行业目录真实存储、分类隔离、授权和版本冲突。

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    state::AppState,
};

const ROOT: &str = "/api/v1/admin/lexicon/taxonomies";

async fn seed_admin(pool: &PgPool, role: AdminRole, must_change_password: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("catalog-{}", id.simple()),
            display_name: match role {
                AdminRole::SuperAdmin => "目录超级管理员".to_owned(),
                AdminRole::Admin => "目录普通管理员".to_owned(),
            },
            password_hash: "hashed-password".to_owned(),
            role,
            must_change_password,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签发测试 token 应成功")
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "响应应为 JSON：{error}，body={}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, headers, json, bytes)
}

#[sqlx::test]
async fn categories_crud_conflicts_and_concurrent_updates(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let payload =
        json!({"code":"technical", "name_zh":" 专业 ", "name_en":" Technical ", "sort_order":10});
    for kind in ["registers", "domains"] {
        let root = format!("{ROOT}/{kind}");
        let (status, _, created, _) = call(
            &state,
            Method::POST,
            &root,
            Some(&bearer),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["name_zh"], "专业");
        assert_eq!(created["name_en"], "Technical");
        let id = created["id"].as_str().unwrap();
        let item = format!("{root}/{id}");
        let (_, _, listed, _) = call(
            &state,
            Method::GET,
            &format!("{root}?q=TECH&page=1&page_size=1"),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(listed["items"][0], created);
        assert_eq!(listed["pagination"]["total"], 1);
        for (field, value) in [
            ("code", json!("technical")),
            ("name_zh", json!("专业")),
            ("name_en", json!("TECHNICAL")),
        ] {
            let mut duplicate =
                json!({"code":"other", "name_zh":"其他", "name_en":"Other", "sort_order":20});
            duplicate[field] = value;
            let (status, _, problem, _) =
                call(&state, Method::POST, &root, Some(&bearer), Some(duplicate)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{problem}");
            assert_eq!(problem["code"], "taxonomy_conflict");
            assert_eq!(problem["field"], field);
        }
        let patch = json!({"name_zh":"专业用语", "name_en":"Technical usage", "sort_order":-1, "base_revision":1});
        let (first, second) = tokio::join!(
            call(
                &state,
                Method::PATCH,
                &item,
                Some(&bearer),
                Some(patch.clone())
            ),
            call(
                &state,
                Method::PATCH,
                &item,
                Some(&bearer),
                Some(patch.clone())
            )
        );
        let mut statuses = [first.0.as_u16(), second.0.as_u16()];
        statuses.sort();
        assert_eq!(statuses, [200, 409]);
        let (_, _, persisted, _) = call(&state, Method::GET, &root, Some(&bearer), None).await;
        assert_eq!(persisted["items"][0]["revision"], 2);
        assert_eq!(persisted["items"][0]["sort_order"], -1);
        assert_eq!(persisted["items"][0]["name_zh"], "专业用语");
        let other_kind = if kind == "registers" {
            "domains"
        } else {
            "registers"
        };
        assert_eq!(
            call(
                &state,
                Method::DELETE,
                &format!("{ROOT}/{other_kind}/{id}?base_revision=2"),
                Some(&bearer),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &state,
                Method::DELETE,
                &format!("{item}?base_revision=1"),
                Some(&bearer),
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let mut code_change = patch.clone();
        code_change["code"] = json!("changed");
        assert_eq!(
            call(
                &state,
                Method::PATCH,
                &item,
                Some(&bearer),
                Some(code_change)
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut invalid = patch;
        invalid["name_zh"] = json!("   ");
        assert_eq!(
            call(&state, Method::PATCH, &item, Some(&bearer), Some(invalid))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &state,
                Method::DELETE,
                &format!("{item}?base_revision=2"),
                Some(&bearer),
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        let (_, _, empty, _) = call(&state, Method::GET, &root, Some(&bearer), None).await;
        assert_eq!(empty["pagination"]["total"], 0);
    }
    // 两类允许相同编码/名称。
    for kind in ["registers", "domains"] {
        assert_eq!(
            call(
                &state,
                Method::POST,
                &format!("{ROOT}/{kind}"),
                Some(&bearer),
                Some(payload.clone())
            )
            .await
            .0,
            StatusCode::CREATED
        );
    }
    assert_eq!(
        call(
            &state,
            Method::GET,
            &format!("{ROOT}/unknown"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &state,
            Method::GET,
            &format!("{ROOT}/registers?page=0"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, _, literal, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/registers?q=%25"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(literal["pagination"]["total"], 0);
}

#[sqlx::test]
async fn delegated_permissions_are_enforced_and_revoked(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::Admin);
    let root = format!("{ROOT}/domains");
    let payload = json!({"code":"law", "name_zh":"法律", "name_en":"Law", "sort_order":10});
    assert_eq!(
        call(&state, Method::GET, &root, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&state, Method::GET, &root, Some(&bearer), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'lexicon_settings.access',$1)").bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, Method::GET, &root, Some(&bearer), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            Method::POST,
            &root,
            Some(&bearer),
            Some(payload.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'lexicon_settings.edit_taxonomies',$1)").bind(actor).execute(&pool).await.unwrap();
    let (status, _, created, _) = call(
        &state,
        Method::POST,
        &root,
        Some(&bearer),
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let item = format!("{root}/{}", created["id"].as_str().unwrap());
    assert_eq!(
        call(
            &state,
            Method::DELETE,
            &format!("{item}?base_revision=1"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='lexicon_settings.edit_taxonomies'").bind(actor).execute(&pool).await.unwrap();
    let update = json!({"name_zh":"法学", "name_en":"Law", "sort_order":10, "base_revision":1});
    assert_eq!(
        call(&state, Method::PATCH, &item, Some(&bearer), Some(update))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'lexicon_settings.delete',$1)").bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        call(
            &state,
            Method::DELETE,
            &format!("{item}?base_revision=1"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn migration_can_be_reversed_and_reapplied_without_changing_parts(pool: PgPool) {
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog.parts_of_speech")
        .fetch_one(&pool)
        .await
        .unwrap();
    let admin = seed_admin(&pool, AdminRole::Admin, false).await;
    let tag = Uuid::now_v7();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'lexicon_settings.edit_taxonomies',$1)").bind(admin).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO permission_tags(id,name) VALUES($1,'回退验证')")
        .bind(tag)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO permission_tag_items(tag_id,permission_key) VALUES($1,'lexicon_settings.edit_taxonomies')").bind(tag).execute(&pool).await.unwrap();
    let old_admin_version: i64 =
        sqlx::query_scalar("SELECT permission_version FROM admins WHERE id=$1")
            .bind(admin)
            .fetch_one(&pool)
            .await
            .unwrap();
    let old_tag_version: i64 =
        sqlx::query_scalar("SELECT version FROM permission_tags WHERE id=$1")
            .bind(tag)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261113150000_lexicon_taxonomies.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let missing: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('catalog.lexicon_taxonomies')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(missing.is_none());
    let admin_version: i64 =
        sqlx::query_scalar("SELECT permission_version FROM admins WHERE id=$1")
            .bind(admin)
            .fetch_one(&pool)
            .await
            .unwrap();
    let tag_version: i64 = sqlx::query_scalar("SELECT version FROM permission_tags WHERE id=$1")
        .bind(tag)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(admin_version, old_admin_version + 1);
    assert_eq!(tag_version, old_tag_version + 1);
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM admin_permission_grants WHERE admin_id=$1")
            .bind(admin)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(grants, 0);

    sqlx::raw_sql(include_str!(
        "../migrations/20261113150000_lexicon_taxonomies.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog.parts_of_speech")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[sqlx::test]
async fn logout_all_rejects_writes_authenticated_before_the_body_arrives(pool: PgPool) {
    for method in [Method::POST, Method::PATCH] {
        let state = AppState::for_test(pool.clone());
        let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
        let bearer = token(&state, actor, AdminRole::SuperAdmin);
        let root = format!("{ROOT}/registers");
        let input = json!({"code":format!("p_{}", &actor.simple().to_string()[..20]), "name_zh":"延迟请求", "name_en":"Paused request", "sort_order":10});
        let (uri, payload, existing) = if method == Method::PATCH {
            let (status, _, created, _) = call(
                &state,
                Method::POST,
                &root,
                Some(&bearer),
                Some(input.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            (
                format!("{root}/{}", created["id"].as_str().unwrap()),
                json!({"name_zh":"过期会话写入", "name_en":"Stale session", "sort_order":20, "base_revision":1}),
                Some(created),
            )
        } else {
            (root, input, None)
        };
        let (reading, has_started) = tokio::sync::oneshot::channel();
        let (release, resume) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let body = Body::from_stream(futures_util::stream::once(async move {
            // ApiJson 开始读 body 时，路由 middleware 已完成头部鉴权。
            reading.send(()).unwrap();
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(resume.await.unwrap()))
        }));
        let request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        let app = tsz_rust::router(state.clone());
        let pending = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(5), has_started)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            call(
                &state,
                Method::POST,
                "/api/v1/admin/auth/logout-all",
                Some(&bearer),
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        release.send(serde_json::to_vec(&payload).unwrap()).unwrap();
        let response = pending.await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} must recheck the revoked session inside its write transaction"
        );
        let problem: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(problem["code"], "invalid_token");
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT name_zh,revision FROM catalog.lexicon_taxonomies WHERE created_by_admin_id=$1",
        )
        .bind(actor)
        .fetch_all(&pool)
        .await
        .unwrap();
        if let Some(created) = existing {
            assert_eq!(
                rows,
                vec![(created["name_zh"].as_str().unwrap().to_owned(), 1)]
            );
        } else {
            assert!(rows.is_empty());
        }
    }
}
