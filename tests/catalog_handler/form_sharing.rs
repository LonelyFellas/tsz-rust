//! 默认共享配置只改变 catalog，不回写词条；保存须独立授权并按 revision 串行化。
use super::*;

const SHARING: &str = "/api/v1/admin/settings/parts-of-speech/form-sharing";
const FORMS: &str = "/api/v1/admin/settings/form-types";

async fn read_template(state: &AppState, bearer: &str) -> Value {
    let (status, _, value, _) = call(state, Method::GET, SHARING, Some(bearer), None).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

fn replacement(template: &Value) -> Value {
    json!({"base_revision": template["revision"], "groups": template["groups"]})
}

async fn catalog_value(state: &AppState, bearer: &str) -> Value {
    let (status, _, value, _) = call(
        state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[sqlx::test]
async fn independent_permissions_persistence_catalog_and_revocation(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, actor, AdminRole::Admin);
    assert_eq!(
        call(&state, Method::GET, SHARING, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'words.access',$1)")
        .bind(actor).execute(&pool).await.unwrap();
    let original = catalog_value(&state, &bearer).await;
    assert_eq!(original["form_sharing"]["revision"], 1);
    assert_eq!(
        call(&state, Method::GET, SHARING, Some(&bearer), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut input = replacement(&original["form_sharing"]);
    for key in ["lexicon_settings.access", "lexicon_settings.edit_forms"] {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,$2,$1)")
            .bind(actor).bind(key).execute(&pool).await.unwrap();
    }
    assert_eq!(
        read_template(&state, &bearer).await,
        original["form_sharing"]
    );
    assert_eq!(
        call(
            &state,
            Method::PUT,
            SHARING,
            Some(&bearer),
            Some(input.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES ($1,'lexicon_settings.edit_form_sharing',$1)")
        .bind(actor).execute(&pool).await.unwrap();
    // 被关闭的默认生成开关仍可在模板中配置，但保存共享配置不能恢复该开关。
    sqlx::query("UPDATE catalog.form_types SET is_default=false WHERE code='plural'")
        .execute(&pool)
        .await
        .unwrap();
    input["groups"].as_array_mut().unwrap().reverse();
    let (status, _, saved, _) = call(
        &state,
        Method::PUT,
        SHARING,
        Some(&bearer),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["groups"], input["groups"]);
    assert_eq!(
        read_template(&AppState::for_test(pool.clone()), &bearer).await,
        saved
    );
    let catalog = catalog_value(&state, &bearer).await;
    assert_eq!(catalog["form_sharing"], saved);
    assert_eq!(
        catalog["catalog_version"].as_i64().unwrap(),
        original["catalog_version"].as_i64().unwrap() + 1
    );
    let noun = catalog["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "noun")
        .unwrap();
    assert_eq!(noun["default_form_types"], json!([]));
    assert_eq!(noun["allowed_form_types"], json!(["plural"]));
    let updater: Uuid =
        sqlx::query_scalar("SELECT updated_by_admin_id FROM catalog.form_sharing_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(updater, actor);
    // 缓存过鉴权的请求也必须在事务内复核授权，不能在撤权后沿用旧快照写入。
    use axum::extract::{FromRequestParts, State};
    let (mut parts, _) = Request::builder()
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(())
        .unwrap()
        .into_parts();
    let auth = tsz_rust::admin::AdminAuth::from_request_parts(&mut parts, &state)
        .await
        .unwrap();
    tsz_rust::admin::permissions::load(&state, &auth)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='lexicon_settings.edit_form_sharing'")
        .bind(actor).execute(&pool).await.unwrap();
    assert_eq!(
        call(
            &state,
            Method::PUT,
            SHARING,
            Some(&bearer),
            Some(replacement(&saved))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let error = tsz_rust::catalog::form_sharing::update(
        State(state.clone()),
        auth,
        tsz_rust::api::ApiJson(serde_json::from_value(replacement(&saved)).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), tsz_rust::error::ErrorCode::Forbidden);
    assert_eq!(read_template(&state, &bearer).await, saved);
}

#[sqlx::test]
async fn invalid_labels_groups_and_bodies_do_not_mutate_template(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let before = read_template(&state, &bearer).await;
    let mut inputs = vec![];
    let mut input = replacement(&before);
    input["groups"][0]["members"] = json!([input["groups"][0]["members"][0]]);
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][0]["members"][1] = input["groups"][0]["members"][0].clone();
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][1]["members"][0] = input["groups"][0]["members"][0].clone();
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][1]["id"] = input["groups"][0]["id"].clone();
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][0]["members"][0]["part_of_speech_id"] = json!(Uuid::now_v7());
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][0]["members"][0]["form_type_id"] = json!(Uuid::now_v7());
    inputs.push(input);
    let mut input = replacement(&before);
    // 将复数错误挂给另一词性，不能仅验证两个 ID 各自存在。
    input["groups"][1]["members"][0]["part_of_speech_id"] =
        input["groups"][1]["members"][1]["part_of_speech_id"].clone();
    inputs.push(input);
    let phrase: Uuid = sqlx::query_scalar("INSERT INTO catalog.parts_of_speech(id,kind,code,name_zh,name_en,abbreviation,short_name_zh,full_name_en) VALUES($1,'phrase','phrase_test','短语测试','Phrase test','pt.','短测','phrase test') RETURNING id")
        .bind(Uuid::now_v7()).fetch_one(&pool).await.unwrap();
    let mut input = replacement(&before);
    input["groups"][0]["members"][0]["part_of_speech_id"] = json!(phrase);
    inputs.push(input);
    let mut input = replacement(&before);
    input["base_revision"] = json!(0);
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"] = json!(vec![input["groups"][0].clone(); 101]);
    inputs.push(input);
    let mut input = replacement(&before);
    input["groups"][0]["members"] = json!(vec![input["groups"][0]["members"][0].clone(); 101]);
    inputs.push(input);
    for input in inputs {
        let (status, _, problem, _) =
            call(&state, Method::PUT, SHARING, Some(&bearer), Some(input)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
        assert_eq!(problem["code"], "invalid_form_type");
    }
    for field in ["unknown", "id"] {
        let mut input = replacement(&before);
        input[field] = json!("unexpected");
        assert_eq!(
            call(&state, Method::PUT, SHARING, Some(&bearer), Some(input))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(read_template(&state, &bearer).await, before);
    assert_eq!(catalog_value(&state, &bearer).await["catalog_version"], 9);
}

#[sqlx::test]
async fn concurrent_replacements_conflict_and_empty_template_persists(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let before = read_template(&state, &bearer).await;
    let mut other = replacement(&before);
    other["groups"] = json!([]);
    let (a, b) = tokio::join!(
        call(
            &state,
            Method::PUT,
            SHARING,
            Some(&bearer),
            Some(replacement(&before))
        ),
        call(&state, Method::PUT, SHARING, Some(&bearer), Some(other))
    );
    let (success, conflict) = if a.0 == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(success.0, StatusCode::OK, "{success:?}");
    assert_eq!(conflict.0, StatusCode::CONFLICT, "{conflict:?}");
    assert_eq!(conflict.2["code"], "revision_conflict");
    assert_eq!(conflict.2["field"], "base_revision");
    assert_eq!(conflict.2["meta"]["current_revision"], 2);
    assert_eq!(read_template(&state, &bearer).await, success.2);
    assert_eq!(catalog_value(&state, &bearer).await["catalog_version"], 10);
    let (status, _, saved, _) = call(
        &state,
        Method::PUT,
        SHARING,
        Some(&bearer),
        Some(json!({"base_revision":2,"groups":[]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved, json!({"revision":3,"groups":[]}));
    assert_eq!(read_template(&state, &bearer).await, saved);
}

#[sqlx::test]
async fn references_block_delete_and_reparent_until_removed(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let before = read_template(&state, &bearer).await;
    let pronoun = before["groups"][0]["members"][1]["part_of_speech_id"]
        .as_str()
        .unwrap();
    sqlx::query("DELETE FROM catalog.sub_parts_of_speech WHERE part_of_speech_id=$1")
        .bind(pronoun.parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, problem, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{pronoun}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "part_of_speech_in_use");
    assert_eq!(problem["field"], "form_sharing");
    let plural = before["groups"][1]["members"][0]["form_type_id"]
        .as_str()
        .unwrap();
    let (status, _, problem, _) = call(
        &state,
        Method::DELETE,
        &format!("{FORMS}/{plural}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "form_type_in_use");
    assert_eq!(problem["field"], "form_sharing");
    let (_, _, forms, _) = call(&state, Method::GET, FORMS, Some(&bearer), None).await;
    let form = forms["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == plural)
        .unwrap();
    let patch = json!({"base_revision":1,"part_of_speech_id":pronoun,"name_zh":form["name_zh"],"name_en":form["name_en"],"short_name_zh":form["short_name_zh"],"abbreviation":form["abbreviation"],"full_name_en":form["full_name_en"],"sort_order":form["sort_order"]});
    let (status, _, problem, _) = call(
        &state,
        Method::PATCH,
        &format!("{FORMS}/{plural}"),
        Some(&bearer),
        Some(patch.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "form_type_in_use");
    assert_eq!(problem["field"], "part_of_speech_id");
    // 展示字段与默认生成开关仍能编辑；解除模板引用后才允许改挂/删除。
    let mut rename = patch.clone();
    rename.as_object_mut().unwrap().remove("part_of_speech_id");
    rename["is_default"] = json!(false);
    assert_eq!(
        call(
            &state,
            Method::PATCH,
            &format!("{FORMS}/{plural}"),
            Some(&bearer),
            Some(rename)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            Method::PUT,
            SHARING,
            Some(&bearer),
            Some(json!({"base_revision":1,"groups":[]}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut patch = patch;
    patch["base_revision"] = json!(2);
    assert_eq!(
        call(
            &state,
            Method::PATCH,
            &format!("{FORMS}/{plural}"),
            Some(&bearer),
            Some(patch)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            Method::DELETE,
            &format!("{FORMS}/{plural}?base_revision=3"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &state,
            Method::DELETE,
            &format!("{ROOT}/{pronoun}?base_revision=1"),
            Some(&bearer),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

async fn wait_for_lock(pool: &PgPool, query_pattern: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE $1)")
                .bind(query_pattern).fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("目标写事务应进入指定的数据库锁等待");
}

#[sqlx::test]
async fn reparent_and_template_save_serialize_in_both_orders(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, actor, AdminRole::SuperAdmin);
    let original = read_template(&state, &bearer).await;
    let plural = original["groups"][1]["members"][0]["form_type_id"]
        .as_str()
        .unwrap();
    let pronoun = original["groups"][0]["members"][1]["part_of_speech_id"]
        .as_str()
        .unwrap();
    let noun = original["groups"][1]["members"][0]["part_of_speech_id"].clone();
    let (_, _, forms, _) = call(&state, Method::GET, FORMS, Some(&bearer), None).await;
    let form = forms["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == plural)
        .unwrap();
    let mut patch = json!({"base_revision":1,"part_of_speech_id":pronoun,"name_zh":form["name_zh"],"name_en":form["name_en"],"short_name_zh":form["short_name_zh"],"abbreviation":form["abbreviation"],"full_name_en":form["full_name_en"],"sort_order":form["sort_order"]});
    assert_eq!(
        call(
            &state,
            Method::PUT,
            SHARING,
            Some(&bearer),
            Some(json!({"base_revision":1,"groups":[]}))
        )
        .await
        .0,
        StatusCode::OK
    );
    // 先让改挂提交：模板请求等待 singleton 锁，醒来后必须按新的归属重新验证。
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM catalog.form_sharing_settings FOR UPDATE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let input = json!({"base_revision":2,"groups":original["groups"]});
    let pending = tokio::spawn({
        let state = state.clone();
        let bearer = bearer.clone();
        let input = input.clone();
        async move { call(&state, Method::PUT, SHARING, Some(&bearer), Some(input)).await }
    });
    wait_for_lock(
        &pool,
        "%SELECT revision FROM catalog.form_sharing_settings%FOR UPDATE%",
    )
    .await;
    assert_eq!(
        call(
            &state,
            Method::PATCH,
            &format!("{FORMS}/{plural}"),
            Some(&bearer),
            Some(patch.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    blocker.commit().await.unwrap();
    let result = pending.await.unwrap();
    assert_eq!(result.0, StatusCode::BAD_REQUEST, "{result:?}");
    assert_eq!(
        read_template(&state, &bearer).await,
        json!({"revision":2,"groups":[]})
    );
    patch["base_revision"] = json!(2);
    patch["part_of_speech_id"] = noun;
    assert_eq!(
        call(
            &state,
            Method::PATCH,
            &format!("{FORMS}/{plural}"),
            Some(&bearer),
            Some(patch.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    // 先让模板提交：模板持有词形共享锁，改挂醒来后必须看见新引用并返回可读 409。
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM catalog.metadata FOR UPDATE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let saving = tokio::spawn({
        let state = state.clone();
        let bearer = bearer.clone();
        async move { call(&state, Method::PUT, SHARING, Some(&bearer), Some(input)).await }
    });
    wait_for_lock(&pool, "%UPDATE catalog.metadata SET version=version+1%").await;
    patch["base_revision"] = json!(3);
    patch["part_of_speech_id"] = json!(pronoun);
    let moving = tokio::spawn({
        let state = state.clone();
        let bearer = bearer.clone();
        let uri = format!("{FORMS}/{plural}");
        async move { call(&state, Method::PATCH, &uri, Some(&bearer), Some(patch)).await }
    });
    wait_for_lock(
        &pool,
        "%SELECT id FROM catalog.form_types WHERE id=$1 FOR UPDATE%",
    )
    .await;
    blocker.commit().await.unwrap();
    assert_eq!(saving.await.unwrap().0, StatusCode::OK);
    let result = moving.await.unwrap();
    assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
    assert_eq!(result.2["code"], "form_type_in_use");
    assert_eq!(
        read_template(&state, &bearer).await["groups"],
        original["groups"]
    );
}
