use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin, permissions::catalog},
    state::AppState,
};

#[derive(Clone, Default)]
struct SqlStatements(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct SqlStatementLayer;

fn sql_count_span(statements: SqlStatements) -> tracing::Span {
    use tracing_subscriber::{prelude::*, registry::LookupSpan};
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(SqlStatementLayer),
        )
        .unwrap();
    });
    let span = tracing::info_span!("permission_sql_count");
    span.with_subscriber(|(id, dispatch)| {
        dispatch
            .downcast_ref::<tracing_subscriber::Registry>()
            .unwrap()
            .span(id)
            .unwrap()
            .extensions_mut()
            .insert(statements);
    });
    span
}

impl<S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>>
    tracing_subscriber::Layer<S> for SqlStatementLayer
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let Some(statements) = context.event_scope(event).and_then(|mut scope| {
            scope.find_map(|span| span.extensions().get::<SqlStatements>().cloned())
        }) else {
            return;
        };
        struct StatementVisitor(Option<String>);
        impl tracing::field::Visit for StatementVisitor {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "db.statement" {
                    self.0 = Some(value.to_owned());
                }
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "db.statement" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        let mut visitor = StatementVisitor(None);
        event.record(&mut visitor);
        if let Some(sql) = visitor.0 {
            statements.0.lock().unwrap().push(sql);
        }
    }
}

#[sqlx::test]
async fn request_authorization_is_loaded_once_and_new_requests_recheck_revocation(pool: PgPool) {
    use axum::extract::FromRequestParts;
    use tracing::Instrument;
    use tsz_rust::admin::{AdminAuth, permissions};
    let state = AppState::for_test(pool.clone());
    let id = seed(&pool, AdminRole::Admin).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.access',$1)").bind(id).execute(&pool).await.unwrap();
    let bearer = token(&state, id, AdminRole::Admin);
    let make_parts = || {
        Request::builder()
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .body(())
            .unwrap()
            .into_parts()
            .0
    };
    let mut parts = make_parts();
    let statements = SqlStatements::default();
    let (auth, first) = async {
        let auth = AdminAuth::from_request_parts(&mut parts, &state)
            .await
            .unwrap();
        let first = permissions::load(&state, &auth).await.unwrap();
        let again = AdminAuth::from_request_parts(&mut parts, &state)
            .await
            .unwrap();
        let second = permissions::load(&state, &again).await.unwrap();
        assert_eq!(first.permissions, second.permissions);
        (auth, first)
    }
    .instrument(sql_count_span(statements.clone()))
    .await;
    let queries = statements.0.lock().unwrap().clone();
    assert_eq!(
        queries.len(),
        4,
        "repeated extraction/load should issue only the initial four SELECTs: {queries:?}"
    );
    let profile_statements = SqlStatements::default();
    let (status, profile) = request(&state, &bearer, "GET", "/profile", None)
        .instrument(sql_count_span(profile_statements.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["id"], id.to_string());
    assert_eq!(profile["role"], "admin");
    assert_eq!(profile["permissions"], json!(["words.access"]));
    assert_eq!(
        profile_statements.0.lock().unwrap().len(),
        4,
        "middleware and profile must reuse the same identity and authorization"
    );
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let same_request = permissions::load(&state, &auth).await.unwrap();
    assert_eq!(same_request.permissions, first.permissions);
    assert_eq!(same_request.permission_version, first.permission_version);
    let refreshed = permissions::reload(&state, &auth).await.unwrap();
    assert!(refreshed.permissions.is_empty());
    assert_eq!(refreshed.permission_version, first.permission_version + 1);
    let new_auth = AdminAuth::from_request_parts(&mut make_parts(), &state)
        .await
        .unwrap();
    let new_request = permissions::load(&state, &new_auth).await.unwrap();
    assert!(new_request.permissions.is_empty());
    assert_eq!(new_request.permission_version, first.permission_version + 1);
    sqlx::query("UPDATE admins SET must_change_password=true WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let new_auth = AdminAuth::from_request_parts(&mut make_parts(), &state)
        .await
        .unwrap();
    assert!(permissions::load(&state, &new_auth).await.is_err());
    sqlx::query("UPDATE admins SET security_version=security_version+1 WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        AdminAuth::from_request_parts(&mut parts, &state)
            .await
            .is_ok(),
        "already verified request identity is reused"
    );
    assert!(
        AdminAuth::from_request_parts(&mut make_parts(), &state)
            .await
            .is_err(),
        "new request must reject invalidated token"
    );
    assert!(permissions::reload(&state, &auth).await.is_err());
}

#[sqlx::test]
async fn maximum_batch_grants_use_one_insert_per_target_and_preserve_atomic_audit(pool: PgPool) {
    use tracing::Instrument;
    use tsz_rust::admin::permissions::{model::*, service};
    let actor = seed(&pool, AdminRole::SuperAdmin).await;
    let mut ids = Vec::new();
    for _ in 0..100 {
        ids.push(seed(&pool, AdminRole::Admin).await);
    }
    let all: Vec<String> = catalog::all_keys().into_iter().collect();
    let input = ChangeRequest {
        catalog_version: catalog::catalog_version(),
        targets: ids
            .iter()
            .map(|id| ChangeTarget {
                admin_id: *id,
                expected_version: 0,
                grant: all.clone(),
                revoke: vec![],
            })
            .collect(),
    };
    let request_id = Uuid::now_v7();
    let statements = SqlStatements::default();
    let result = service::apply(&pool, actor, request_id, input)
        .instrument(sql_count_span(statements.clone()))
        .await
        .unwrap();
    let inserts = statements
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|sql| sql.contains("INSERT INTO admin_permission_grants"))
        .count();
    assert_eq!(
        inserts, 100,
        "29 permissions per target must be one INSERT, not 2900"
    );
    assert_eq!(result.targets.len(), 100);
    for target in &result.targets {
        assert_eq!(target.permissions, all);
        assert_eq!(target.permission_version, 1);
    }
    let (grants, actors, timestamps): (i64,i64,i64) = sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE granted_by=$1), count(DISTINCT granted_at) FROM admin_permission_grants").bind(actor).fetch_one(&pool).await.unwrap();
    assert_eq!(grants, 100 * all.len() as i64);
    assert_eq!(actors, grants);
    assert_eq!(timestamps, 1, "all grants use the same transaction now()");
    let audits: Vec<Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit.admin_actions WHERE actor_admin_id=$1 AND request_id=$2",
    )
    .bind(actor)
    .bind(request_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(audits.len(), 100);
    for audit in audits {
        assert_eq!(audit["grant"], json!(all));
        assert_eq!(audit["before"], json!([]));
        assert_eq!(audit["after"], json!(all));
        assert_eq!(audit["revoke"], json!([]));
    }
    let no_op = ChangeRequest {
        catalog_version: catalog::catalog_version(),
        targets: ids
            .iter()
            .map(|id| ChangeTarget {
                admin_id: *id,
                expected_version: 1,
                grant: all.clone(),
                revoke: vec![],
            })
            .collect(),
    };
    let statements = SqlStatements::default();
    service::apply(&pool, actor, Uuid::now_v7(), no_op)
        .instrument(sql_count_span(statements.clone()))
        .await
        .unwrap();
    assert!(!statements.0.lock().unwrap().iter().any(|sql| {
        sql.contains("INSERT INTO admin_permission_grants")
            || sql.contains("INSERT INTO audit.admin_actions")
            || sql.contains("UPDATE admins")
    }));
}

#[sqlx::test]
async fn audit_count_and_items_share_admin_permission_time_and_pagination_filters(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed(&pool, AdminRole::SuperAdmin).await;
    let target = seed(&pool, AdminRole::Admin).await;
    let other = seed(&pool, AdminRole::SuperAdmin).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let mut expected = Vec::new();
    for (by, resource, action, metadata, time) in [
        (
            actor,
            target,
            "admin.permissions.change",
            json!({"grant":["words.edit"]}),
            "2026-10-01T10:00:00Z",
        ),
        (
            actor,
            target,
            "admin.permissions.change",
            json!({"revoke":["words.edit"]}),
            "2026-10-01T11:00:00Z",
        ),
        (
            actor,
            target,
            "admin.permissions.change",
            json!({"grant":["users.access"]}),
            "2026-10-01T11:00:00Z",
        ),
        (
            other,
            Uuid::now_v7(),
            "admin.permission_tags.delete",
            json!({"before":{"permissions":["words.edit"]}}),
            "2026-10-01T12:00:00Z",
        ),
        (
            actor,
            target,
            "not.a.permission.action",
            json!({"grant":["words.edit"]}),
            "2026-10-01T11:00:00Z",
        ),
    ] {
        let id = Uuid::now_v7();
        if expected.len() < 2 {
            expected.push(id);
        }
        sqlx::query("INSERT INTO audit.admin_actions(id,actor_admin_id,action,resource_type,resource_id,request_id,metadata,occurred_at) VALUES($1,$2,$3,'admin',$4,$5,$6,$7)")
            .bind(id).bind(by).bind(action).bind(resource).bind(Uuid::now_v7()).bind(metadata)
            .bind(time.parse::<chrono::DateTime<chrono::Utc>>().unwrap()).execute(&pool).await.unwrap();
    }
    let filter = format!(
        "permission_key=words.edit&admin_id={target}&since=2026-10-01T10:00:00Z&until=2026-10-01T11:00:00Z&page_size=1"
    );
    for (page, id) in [(1, expected[1]), (2, expected[0])] {
        let (status, body) = request(
            &state,
            &bearer,
            "GET",
            &format!("/permission-audits?{filter}&page={page}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 2);
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["id"], id.to_string());
    }
    let (_, body) = request(
        &state,
        &bearer,
        "GET",
        &format!("/permission-audits?permission_key=words.edit&admin_id={actor}"),
        None,
    )
    .await;
    assert_eq!(body["total"], 2, "actor filter and action whitelist");
    let (_, body) = request(&state,&bearer,"GET","/permission-audits?permission_key=words.edit&since=2026-10-01T12:00:00Z&until=2026-10-01T12:00:00Z",None).await;
    assert_eq!(body["total"], 1);
    assert_eq!(
        body["items"][0]["action"], "admin.permission_tags.delete",
        "nested before.permissions is included"
    );
}

async fn seed(pool: &PgPool, role: AdminRole) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: id.to_string(),
            display_name: "权限测试".into(),
            password_hash: "unused".into(),
            role,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .unwrap()
}

async fn request(
    state: &AppState,
    bearer: &str,
    method: &str,
    path: &str,
    input: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api/v1/admin{path}"))
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    let body = match input {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn preview(
    state: &AppState,
    bearer: &str,
    targets: &[Uuid],
    grant: &[&str],
    revoke: &[&str],
) -> Value {
    let (status, body) = request(state, bearer, "POST", "/permission-changes/preview", Some(json!({
        "catalog_version": catalog::catalog_version(), "targets": targets.iter().map(|id| json!({"admin_id": id})).collect::<Vec<_>>(),
        "grant": grant, "revoke": revoke,
    }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn commit_input(preview: &Value) -> Value {
    json!({ "catalog_version": preview["catalog_version"], "targets": preview["targets"].as_array().unwrap().iter().map(|target| json!({
        "admin_id": target["admin_id"], "expected_version": target["expected_version"], "grant": target["grant"], "revoke": target["revoke"],
    })).collect::<Vec<_>>() })
}

#[sqlx::test]
async fn zero_grants_profile_business_and_admin_realm_are_fail_closed(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let ordinary = seed(&pool, AdminRole::Admin).await;
    let bearer = token(&state, ordinary, AdminRole::SuperAdmin);
    let (status, profile) = request(&state, &bearer, "GET", "/profile", None).await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["permissions"], json!([]));
    assert_eq!(profile["permission_version"], 0);
    assert_eq!(profile["catalog_version"], catalog::catalog_version());
    assert_eq!(profile["can_publish_lexicon"], false);
    for (method, path) in [
        ("GET", "/lexicon/entries"),
        ("HEAD", "/lexicon/entries"),
        ("POST", "/lexicon/entries/component-targets/search"),
        ("GET", "/users"),
        ("GET", "/teacher-applications"),
        ("GET", "/permissions"),
    ] {
        let input = (method == "POST").then(|| json!({}));
        let (status, body) = request(&state, &bearer, method, path, input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    let web_bearer = state.token_manager.generate(ordinary, "user").unwrap();
    let (status, _) = request(&state, &web_bearer, "GET", "/profile", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE admins SET can_publish_lexicon = true WHERE id = $1")
        .bind(ordinary)
        .execute(&pool)
        .await
        .unwrap();
    let (_, profile) = request(&state, &bearer, "GET", "/profile", None).await;
    assert_eq!(
        profile["can_publish_lexicon"], false,
        "old boolean cannot grant new abilities"
    );
}

#[sqlx::test]
async fn single_and_batch_changes_expand_dependencies_preserve_existing_and_audit(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let ids = [
        seed(&pool, AdminRole::Admin).await,
        seed(&pool, AdminRole::Admin).await,
    ];
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let initial = preview(&state, &bearer, &[ids[0]], &["users.access"], &[]).await;
    let (status, _) = request(
        &state,
        &bearer,
        "POST",
        "/permission-changes",
        Some(commit_input(&initial)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let batch = preview(&state, &bearer, &ids, &["words.edit_others"], &[]).await;
    assert_eq!(
        batch["targets"][0]["dependency_grants"],
        json!(["words.access", "words.edit"])
    );
    let (status, result) = request(
        &state,
        &bearer,
        "POST",
        "/permission-changes",
        Some(commit_input(&batch)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result["targets"][0]["permissions"],
        json!([
            "users.access",
            "words.access",
            "words.edit",
            "words.edit_others"
        ])
    );
    assert_eq!(result["targets"][0]["permission_version"], 2);
    assert_eq!(result["targets"][1]["permission_version"], 1);
    let (status, audits) = request(
        &state,
        &bearer,
        "GET",
        "/permission-audits?permission_key=words.edit_others",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{audits}");
    assert_eq!(audits["total"], 2);
    assert_eq!(audits["items"][0]["request_id"], result["request_id"]);
    let revoke = preview(&state, &bearer, &ids, &[], &["words.edit"]).await;
    assert_eq!(
        revoke["targets"][0]["dependency_revocations"],
        json!(["words.edit_others"])
    );
    let (status, result) = request(
        &state,
        &bearer,
        "POST",
        "/permission-changes",
        Some(commit_input(&revoke)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result["targets"][0]["permissions"],
        json!(["users.access", "words.access"])
    );
    let (status, recipients) = request(
        &state,
        &bearer,
        "GET",
        "/permissions/words.access/admins?page=1&page_size=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recipients}");
    assert_eq!(recipients["total"], 2);
    assert_eq!(recipients["items"].as_array().unwrap().len(), 1);
    assert_eq!(recipients["super_admins_are_implicit"], true);
}

#[sqlx::test]
async fn stale_versions_invalid_targets_and_dependency_failures_are_atomic(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let ids = [
        seed(&pool, AdminRole::Admin).await,
        seed(&pool, AdminRole::Admin).await,
    ];
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let old = preview(&state, &bearer, &ids, &["words.access"], &[]).await;
    let first = preview(&state, &bearer, &[ids[1]], &["users.access"], &[]).await;
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&first))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, _) = request(
        &state,
        &bearer,
        "POST",
        "/permission-changes",
        Some(commit_input(&old)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, untouched) = request(
        &state,
        &bearer,
        "GET",
        &format!("/admins/{}/permissions", ids[0]),
        None,
    )
    .await;
    assert_eq!(untouched["permissions"], json!([]));
    assert_eq!(untouched["permission_version"], 0);
    for target in [Uuid::now_v7(), super_id] {
        let input = json!({"catalog_version": catalog::catalog_version(), "targets": [
            {"admin_id": ids[0], "expected_version": 0, "grant": ["words.access"], "revoke": []},
            {"admin_id": target, "expected_version": 0, "grant": ["words.access"], "revoke": []},
        ]});
        let (status, _) =
            request(&state, &bearer, "POST", "/permission-changes", Some(input)).await;
        assert!(matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
        ));
    }
    let incomplete = json!({"catalog_version": catalog::catalog_version(), "targets": [{"admin_id": ids[0], "expected_version": 0, "grant": ["words.edit_others"], "revoke": []}]});
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(incomplete)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let wrong_catalog = json!({"catalog_version": "old", "targets": [{"admin_id": ids[0], "expected_version": 0, "grant": ["words.access"], "revoke": []}]});
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(wrong_catalog)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.admin_actions WHERE action = 'admin.permissions.change'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn no_op_does_not_increment_and_concurrent_super_admins_do_not_lose_updates(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_a = seed(&pool, AdminRole::SuperAdmin).await;
    let super_b = seed(&pool, AdminRole::SuperAdmin).await;
    let id = seed(&pool, AdminRole::Admin).await;
    let a = token(&state, super_a, AdminRole::SuperAdmin);
    let b = token(&state, super_b, AdminRole::SuperAdmin);
    let preview_a = preview(&state, &a, &[id], &["words.access"], &[]).await;
    let preview_b = preview(&state, &b, &[id], &["users.access"], &[]).await;
    let (result_a, result_b) = tokio::join!(
        request(
            &state,
            &a,
            "POST",
            "/permission-changes",
            Some(commit_input(&preview_a))
        ),
        request(
            &state,
            &b,
            "POST",
            "/permission-changes",
            Some(commit_input(&preview_b))
        ),
    );
    assert_eq!(
        [result_a.0, result_b.0]
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        [result_a.0, result_b.0]
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
    let no_op = preview(&state, &a, &[id], &[], &[]).await;
    let (status, response) = request(
        &state,
        &a,
        "POST",
        "/permission-changes",
        Some(commit_input(&no_op)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["targets"][0]["permission_version"], 1);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.admin_actions WHERE action = 'admin.permissions.change'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn tag_changes_are_many_to_many_atomic_and_never_change_grants(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let id = seed(&pool, AdminRole::Admin).await;
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let grant = preview(&state, &bearer, &[id], &["words.access"], &[]).await;
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&grant))
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut tags = Vec::new();
    for name in ["编辑", "审核"] {
        let (status, tag) = request(
            &state,
            &bearer,
            "POST",
            "/permission-tags",
            Some(json!({"name": name})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{tag}");
        tags.push(tag);
    }
    let change = json!({"catalog_version": catalog::catalog_version(), "targets": tags.iter().map(|tag| json!({"tag_id": tag["id"], "expected_version": 0, "add": ["words.edit", "words.access"], "remove": []})).collect::<Vec<_>>()});
    let (status, result) = request(
        &state,
        &bearer,
        "POST",
        "/permission-tag-changes",
        Some(change.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result[0]["permissions"],
        json!(["words.access", "words.edit"])
    );
    assert_eq!(result[0]["version"], 1);
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-tag-changes",
            Some(change)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let path = format!("/permission-tags/{}", tags[0]["id"].as_str().unwrap());
    assert_eq!(
        request(
            &state,
            &bearer,
            "PATCH",
            &path,
            Some(json!({"name": "编辑组", "expected_version": 1}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(
            &state,
            &bearer,
            "DELETE",
            &format!("{path}?expected_version=1"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &state,
            &bearer,
            "DELETE",
            &format!("{path}?expected_version=2"),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, snapshot) = request(
        &state,
        &bearer,
        "GET",
        &format!("/admins/{id}/permissions"),
        None,
    )
    .await;
    assert_eq!(snapshot["permissions"], json!(["words.access"]));
    assert_eq!(snapshot["permission_version"], 1);
    let (_, remaining) = request(&state, &bearer, "GET", "/permission-tags", None).await;
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(
        remaining[0]["permissions"],
        json!(["words.access", "words.edit"])
    );
}

#[sqlx::test]
async fn revoked_grant_old_token_and_deprecated_boolean_entry_cannot_bypass(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let id = seed(&pool, AdminRole::Admin).await;
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let ordinary = token(&state, id, AdminRole::Admin);
    let grant = preview(&state, &bearer, &[id], &["words.access"], &[]).await;
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&grant))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&state, &ordinary, "GET", "/lexicon/entries", None)
            .await
            .0,
        StatusCode::OK
    );
    let revoke = preview(&state, &bearer, &[id], &[], &["words.access"]).await;
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&revoke))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&state, &ordinary, "GET", "/lexicon/entries", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request(
        &state,
        &bearer,
        "PATCH",
        &format!("/admins/{id}/lexicon-publication-permission"),
        Some(json!({"can_publish_lexicon": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let value: bool = sqlx::query_scalar("SELECT can_publish_lexicon FROM admins WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!value);
}

#[sqlx::test]
async fn migration_preview_is_read_only_and_never_replays_legacy_flags_over_new_grants(
    pool: PgPool,
) {
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let plain = seed(&pool, AdminRole::Admin).await;
    let publisher = seed(&pool, AdminRole::Admin).await;
    sqlx::query("UPDATE admins SET can_publish_lexicon = true WHERE id = $1")
        .bind(publisher)
        .execute(&pool)
        .await
        .unwrap();
    let report = tsz_rust::admin::permissions::migration::preview(&pool)
        .await
        .unwrap();
    assert!(!report.applied);
    assert_eq!(report.targets.len(), 2);
    let plain_report = report
        .targets
        .iter()
        .find(|target| target.admin_id == plain)
        .unwrap();
    assert_eq!(
        plain_report.proposed_permissions,
        [
            "sentences.access",
            "users.access",
            "users.read_sensitive",
            "words.access"
        ]
    );
    let publish_report = report
        .targets
        .iter()
        .find(|target| target.admin_id == publisher)
        .unwrap();
    assert!(publish_report.approval_required);
    assert!(!publish_report.scope_changes.is_empty());
    assert!(
        !publish_report
            .proposed_permissions
            .iter()
            .any(|key| key.ends_with("edit")
                || key.ends_with("edit_others")
                || key == "users.set_status"
                || key.starts_with("teacherapply.")
                || key == "speech.generate"
                || key.starts_with("lexicon_settings."))
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_permission_grants")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let state = AppState::for_test(pool.clone());
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let grant = preview(&state, &bearer, &[publisher], &["users.access"], &[]).await;
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&grant))
        )
        .await
        .0,
        StatusCode::OK
    );
    let report = tsz_rust::admin::permissions::migration::preview(&pool)
        .await
        .unwrap();
    let target = report
        .targets
        .iter()
        .find(|target| target.admin_id == publisher)
        .unwrap();
    assert!(!target.migration_candidate);
    assert_eq!(target.proposed_permissions, ["users.access"]);
    assert!(target.grant.is_empty() && target.revoke.is_empty());
}

#[sqlx::test]
async fn audit_failure_rolls_back_authorization(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let super_id = seed(&pool, AdminRole::SuperAdmin).await;
    let id = seed(&pool, AdminRole::Admin).await;
    let bearer = token(&state, super_id, AdminRole::SuperAdmin);
    let grant = preview(&state, &bearer, &[id], &["words.access"], &[]).await;
    sqlx::query("ALTER TABLE audit.admin_actions ADD CONSTRAINT permissions_test_failure CHECK (action <> 'admin.permissions.change')").execute(&pool).await.unwrap();
    assert_eq!(
        request(
            &state,
            &bearer,
            "POST",
            "/permission-changes",
            Some(commit_input(&grant))
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let (_, snapshot) = request(
        &state,
        &bearer,
        "GET",
        &format!("/admins/{id}/permissions"),
        None,
    )
    .await;
    assert_eq!(snapshot["permissions"], json!([]));
    assert_eq!(snapshot["permission_version"], 0);
}
