//! 词性配置九个端点的鉴权、wire、事务版本与乐观锁契约测试。

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

const ROOT: &str = "/api/v1/admin/settings/parts-of-speech";

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

struct LexiconUsageFixture {
    entry_id: Uuid,
    pos_node_id: Uuid,
    sense_node_id: Uuid,
}

async fn seed_lexicon_usage(
    pool: &PgPool,
    admin_id: Uuid,
    part_id: Uuid,
    sub_part_id: Uuid,
    publication_count: i32,
    keep_draft: bool,
) -> LexiconUsageFixture {
    let entry_id = Uuid::now_v7();
    let pos_node_id = Uuid::now_v7();
    let sense_node_id = Uuid::now_v7();

    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 3, 'en', 'word', '{}'::jsonb, $2, $2)
        "#,
    )
    .bind(entry_id)
    .bind(admin_id)
    .execute(pool)
    .await
    .expect("插入 usage 测试词条应成功");

    for (node_id, node_type) in [(pos_node_id, "pos"), (sense_node_id, "sense")] {
        sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, $3)")
            .bind(node_id)
            .bind(entry_id)
            .bind(node_type)
            .execute(pool)
            .await
            .expect("插入 usage 测试稳定节点应成功");
    }

    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 'unified', 'unified', 0)
        "#,
    )
    .bind(pos_node_id)
    .bind(entry_id)
    .bind(part_id)
    .execute(pool)
    .await
    .expect("插入 usage 测试 active draft POS 应成功");

    sqlx::query(
        r#"
        INSERT INTO lexicon.senses (
            id, entry_id, entry_pos_id, sub_part_of_speech_id,
            level, depends_on_context, sort_order
        ) VALUES ($1, $2, $3, $4, 'A1', FALSE, 0)
        "#,
    )
    .bind(sense_node_id)
    .bind(entry_id)
    .bind(pos_node_id)
    .bind(sub_part_id)
    .execute(pool)
    .await
    .expect("插入 usage 测试 active draft sense 应成功");

    for publication_number in 1..=publication_count {
        let publication_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publications (
                id, entry_id, publication_number, source_revision,
                content_schema_version, snapshot, snapshot_hash, published_by_admin_id
            ) VALUES ($1, $2, $3, $3, 3, '{}'::jsonb, $4, $5)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(publication_number)
        .bind(publication_id.as_bytes().to_vec())
        .bind(admin_id)
        .execute(pool)
        .await
        .expect("插入 usage 测试历史 publication 应成功");

        for (node_id, node_type) in [(pos_node_id, "pos"), (sense_node_id, "sense")] {
            sqlx::query(
                r#"
                INSERT INTO lexicon.entry_publication_nodes (
                    publication_id, entry_id, node_id, node_type
                ) VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(publication_id)
            .bind(entry_id)
            .bind(node_id)
            .bind(node_type)
            .execute(pool)
            .await
            .expect("插入 publication 稳定节点应成功");
        }

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_part_of_speech_refs (
                publication_id, entry_id, source_node_id, part_of_speech_id
            ) VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(pos_node_id)
        .bind(part_id)
        .execute(pool)
        .await
        .expect("插入 publication POS 引用应成功");

        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_publication_sub_part_of_speech_refs (
                publication_id, entry_id, source_node_id, sub_part_of_speech_id
            ) VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(publication_id)
        .bind(entry_id)
        .bind(sense_node_id)
        .bind(sub_part_id)
        .execute(pool)
        .await
        .expect("插入 publication 细分词性引用应成功");
    }

    if !keep_draft {
        sqlx::query("DELETE FROM lexicon.senses WHERE id = $1")
            .bind(sense_node_id)
            .execute(pool)
            .await
            .expect("移除历史 publication 对应 active draft sense 应成功");
        sqlx::query("DELETE FROM lexicon.entry_pos WHERE id = $1")
            .bind(pos_node_id)
            .execute(pool)
            .await
            .expect("移除历史 publication 对应 active draft POS 应成功");
    }

    LexiconUsageFixture {
        entry_id,
        pos_node_id,
        sense_node_id,
    }
}

#[sqlx::test]
async fn catalog_read_allows_active_admin_but_management_requires_super_admin(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::Admin, false).await;
    let admin_token = token(&state, admin_id, AdminRole::Admin);

    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&admin_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "普通管理员应能读 catalog：{body}");
    assert_eq!(body["catalog_version"], 6);
    // 词形候选按所属词性收窄：种子把动词的四个时态、名词的复数、形容词的两级各归其主，
    // 原形对所有词性通用所以不进候选。
    let form_types_by_pos = json!({
        "noun": ["plural"],
        "pronoun": [],
        "verb": [
            "third_person_singular",
            "present_participle",
            "past_tense",
            "past_participle"
        ],
        "adjective": ["comparative", "superlative"],
        "adverb": []
    });
    for item in body["items"].as_array().unwrap() {
        let expected = &form_types_by_pos[item["code"].as_str().unwrap()];
        assert_eq!(&item["allowed_form_types"], expected, "{item}");
        assert_eq!(&item["default_form_types"], expected, "{item}");
    }
    assert_eq!(body["items"].as_array().map(Vec::len), Some(5));
    assert_eq!(body["items"][0]["code"], "noun");
    assert_eq!(body["items"][0]["short_name_zh"], "名词");
    assert_eq!(body["items"][0]["full_name_en"], "noun");
    assert!(
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["sub_parts_extensible"] == true),
        "默认种子全部是基础词性"
    );
    assert_eq!(
        body["items"][0]["sub_parts"].as_array().map(Vec::len),
        Some(5)
    );
    assert_eq!(
        body["items"][0]["sub_parts"][0]["short_name_zh"],
        "可数名词"
    );
    assert_eq!(body["items"][0]["sub_parts"][0]["abbreviation"], "n.");
    assert_eq!(
        body["items"][0]["sub_parts"][0]["full_name_en"],
        "countable noun"
    );

    let (status, _, body, _) = call(&state, Method::GET, ROOT, Some(&admin_token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    let super_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let super_token = token(&state, super_id, AdminRole::SuperAdmin);
    let (status, _, body, _) = call(&state, Method::GET, ROOT, Some(&super_token), None).await;
    assert_eq!(status, StatusCode::OK, "超级管理员应能读管理列表：{body}");
    assert_eq!(body["pagination"]["page_size"], 10);
    assert_eq!(body["pagination"]["total"], 5);
    let noun = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["code"] == "noun")
        .expect("第一页应包含 noun 种子");
    assert_eq!(noun["created_by"]["id"], "system");
    assert_eq!(noun["created_by"]["display_name"], "系统");
    assert!(noun.get("updated_by").is_none());

    let forced_id = seed_admin(&pool, AdminRole::SuperAdmin, true).await;
    let forced_token = token(&state, forced_id, AdminRole::SuperAdmin);
    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&forced_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "must_change_password");
}

#[sqlx::test]
async fn part_and_sub_part_lifecycle_is_transactional_and_revision_safe(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);

    let (status, _, created, _) = call(
        &state,
        Method::POST,
        ROOT,
        Some(&bearer),
        Some(json!({
            "code": "particle",
            "name_zh": "  小品词  ",
            "name_en": "  Particle  ",
            "abbreviation": " part. ",
            "short_name_zh": "  小品  ",
            "full_name_en": "  Particle Word  ",
            "sort_order": -10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建基本词性失败：{created}");
    let part_id = created["id"].as_str().unwrap();
    assert_eq!(created["name_zh"], "小品词");
    assert_eq!(created["abbreviation"], "part.");
    assert_eq!(created["short_name_zh"], "小品");
    assert_eq!(created["full_name_en"], "Particle Word");
    assert_eq!(
        created["sub_parts_extensible"], true,
        "任意基本词性都能挂细分词性"
    );
    assert_eq!(
        created["sub_pos_required"], false,
        "刚建出来还没配细分词性，释义选填"
    );
    assert_eq!(created["revision"], 1);
    assert_eq!(created["usage_count"], 0);
    assert_eq!(created["sub_part_count"], 0);
    assert_eq!(created["created_by"]["id"], admin_id.to_string());
    assert!(created.get("updated_by").is_none());

    let (status, _, body, _) = call(
        &state,
        Method::POST,
        ROOT,
        Some(&bearer),
        Some(json!({
            "code": "noun",
            "name_zh": "另一个中文名",
            "name_en": "Another English Name",
            "abbreviation": "another.",
            "short_name_zh": "另一个",
            "full_name_en": "another english name",
            "sort_order": 999
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "part_of_speech_conflict");
    assert_eq!(body["field"], "code");

    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "code": "changed_code",
            "name_zh": "小品词",
            "name_en": "Particle",
            "abbreviation": "part.",
            "short_name_zh": "小品",
            "full_name_en": "particle word",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_request_body");

    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 0,
            "name_zh": "小品词",
            "name_en": "Particle",
            "abbreviation": "part.",
            "short_name_zh": "小品",
            "full_name_en": "particle word",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_part_of_speech");
    assert_eq!(body["field"], "base_revision");

    let (status, _, updated, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "新小品词",
            "name_en": "Particle updated",
            "abbreviation": "pt.",
            "short_name_zh": "小品",
            "full_name_en": "particle word",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新基本词性失败：{updated}");
    assert_eq!(updated["code"], "particle");
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["updated_by"]["id"], admin_id.to_string());

    let (status, _, stale, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "过期修改",
            "name_en": "Stale update",
            "abbreviation": "stale.",
            "short_name_zh": "小品",
            "full_name_en": "particle word",
            "sort_order": 0
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["code"], "revision_conflict");
    assert_eq!(stale["field"], "base_revision");
    assert_eq!(stale["meta"]["current_revision"], 2);
    assert_eq!(stale["meta"]["part_of_speech_id"], part_id);
    assert_eq!(stale["meta"]["code"], "particle");

    // 自建的小品词同样可以扩展细分词性：不再按固定编码集合拦截。
    let (status, _, own_sub, _) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/{part_id}/sub-parts"),
        Some(&bearer),
        Some(json!({
            "code": "PRT-FOCUS",
            "name_zh": "焦点小品词",
            "name_en": "Focus particle",
            "short_name_zh": "焦点",
            "abbreviation": "fn.",
            "full_name_en": "focus particle",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "自建词性挂细分词性失败：{own_sub}"
    );
    assert_eq!(own_sub["part_of_speech_id"], part_id);
    assert_eq!(own_sub["name_zh"], "焦点小品词");
    let own_sub_id = own_sub["id"].as_str().unwrap().to_owned();

    // 配了细分词性，这条自建词性的释义就该必填：必填与否只看配了没有，不认编码。
    let (_, _, list, _) = call(&state, Method::GET, ROOT, Some(&bearer), None).await;
    let particle = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == part_id)
        .expect("自建词性应在管理列表里")
        .clone();
    assert_eq!(particle["sub_part_count"], 1);
    assert_eq!(
        particle["sub_pos_required"], true,
        "配了细分词性就该必填：{particle}"
    );
    // 向导读的是 catalog 这条路径，两边必须一致。
    let (_, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&bearer),
        None,
    )
    .await;
    let catalog_particle = catalog["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == part_id)
        .expect("自建词性应在 catalog 里")
        .clone();
    assert_eq!(
        catalog_particle["sub_pos_required"], true,
        "catalog 与管理列表口径必须一致：{catalog_particle}"
    );

    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _, sub, _) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/{noun_id}/sub-parts"),
        Some(&bearer),
        Some(json!({
            "code": "N-FOCUS",
            "name_zh": "焦点名词",
            "name_en": "Focus noun",
            "short_name_zh": "焦点",
            "abbreviation": "fn.",
            "full_name_en": "focus noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建细分词性失败：{sub}");
    let sub_id = sub["id"].as_str().unwrap();
    assert_eq!(sub["part_of_speech_id"], noun_id.to_string());
    assert_eq!(sub["short_name_zh"], "焦点");
    assert_eq!(sub["abbreviation"], "fn.");
    assert_eq!(sub["full_name_en"], "focus noun");
    assert_eq!(sub["revision"], 1);
    assert!(sub.get("updated_by").is_none());

    let (status, _, sub_list, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/{noun_id}/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sub_list["items"].as_array().map(Vec::len), Some(6));
    assert!(sub_list.get("pagination").is_none());

    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "错误父级",
            "name_en": "Wrong parent",
            "short_name_zh": "焦点",
            "abbreviation": "fn.",
            "full_name_en": "wrong parent",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "sub_part_of_speech_not_found");

    let (status, _, updated_sub, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{noun_id}/sub-parts/{sub_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "焦点助词",
            "name_en": "Focus marker",
            "short_name_zh": "焦点",
            "abbreviation": "fn.",
            "full_name_en": "focus marker",
            "sort_order": -20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新细分词性失败：{updated_sub}");
    assert_eq!(updated_sub["revision"], 2);

    // 名词还挂着细分词性：即使没有词条引用也不能删，先清空细分词性。
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{noun_id}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "part_of_speech_has_sub_parts");

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{noun_id}/sub-parts/{sub_id}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["meta"]["current_revision"], 2);

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{noun_id}/sub-parts/{sub_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty(), "204 响应 body 必须为空");

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_query");

    // 放开后新可达的一步：自建词性挂上细分词性就删不掉，必须先清空细分词性。
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "part_of_speech_has_sub_parts");

    let (status, _, _, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}/sub-parts/{own_sub_id}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 删空之后必须回到选填。仍然必填的话下拉里一个候选都没有，这条词性的词义就永远发不出去。
    let (_, _, list, _) = call(&state, Method::GET, ROOT, Some(&bearer), None).await;
    let particle = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == part_id)
        .expect("自建词性应在管理列表里")
        .clone();
    assert_eq!(particle["sub_part_count"], 0);
    assert_eq!(
        particle["sub_pos_required"], false,
        "细分词性删空后必须回到选填：{particle}"
    );
    let (_, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&bearer),
        None,
    )
    .await;
    let catalog_particle = catalog["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == part_id)
        .expect("自建词性应在 catalog 里")
        .clone();
    assert!(
        catalog_particle["sub_parts"].as_array().unwrap().is_empty(),
        "细分词性应已删空：{catalog_particle}"
    );
    assert_eq!(
        catalog_particle["sub_pos_required"], false,
        "catalog 与管理列表口径必须一致：{catalog_particle}"
    );

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());

    let (status, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 比放开前多两步写操作：自建词性挂细分词性、再把它删掉。
    assert_eq!(catalog["catalog_version"], 14);
    assert!(
        catalog["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["code"] != "particle")
    );
}

#[sqlx::test]
async fn query_and_path_rejections_follow_problem_details_contract(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);

    for query in ["q=%25", "q=_"] {
        let (status, _, body, _) = call(
            &state,
            Method::GET,
            &format!("{ROOT}?{query}"),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["pagination"]["total"], 0, "通配符必须按字面匹配");
    }

    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}?page=0"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_query");

    let missing_parent = Uuid::now_v7();
    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/{missing_parent}/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "part_of_speech_not_found");

    let (status, headers, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/not-a-uuid/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["code"], "invalid_path_parameter");
    assert_eq!(body["field"], "id");
    assert!(body.get("meta").is_none());

    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{noun_id}/sub-parts/not-a-uuid?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_path_parameter");
    assert_eq!(body["field"], "sub_id");
}

#[sqlx::test]
async fn usage_counts_merge_active_drafts_and_all_publications_before_delete(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);
    let (part_id, sub_part_id): (Uuid, Uuid) = sqlx::query_as(
        r#"
        SELECT p.id, s.id
        FROM catalog.parts_of_speech p
        JOIN catalog.sub_parts_of_speech s ON s.part_of_speech_id = p.id
        WHERE p.code = 'noun' AND s.code = 'N-COUNT'
        "#,
    )
    .fetch_one(&pool)
    .await
    .expect("noun/N-COUNT 种子应存在");

    // 同一稳定节点同时存在于 active draft 和两个 publication，只能计一次。
    let active = seed_lexicon_usage(&pool, admin_id, part_id, sub_part_id, 2, true).await;
    // 第二个词条仅保留历史 publication 引用，仍必须计数并阻止删除。
    let historical = seed_lexicon_usage(&pool, admin_id, part_id, sub_part_id, 1, false).await;

    let (status, _, part_list, _) = call(&state, Method::GET, ROOT, Some(&bearer), None).await;
    assert_eq!(status, StatusCode::OK, "读取基本词性列表失败：{part_list}");
    let noun = part_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == part_id.to_string())
        .expect("基本词性列表应包含 noun");
    assert_eq!(noun["usage_count"], 2, "基本词性应按 distinct entry 去重");

    let (status, _, sub_list, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/{part_id}/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "读取细分词性列表失败：{sub_list}");
    let n_count = sub_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == sub_part_id.to_string())
        .expect("细分词性列表应包含 N-COUNT");
    assert_eq!(
        n_count["usage_count"], 2,
        "细分词性应按稳定 sense node 去重"
    );

    // 写端点回读完整详情时也必须带同一实时计数。
    let (status, _, part_detail, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": noun["name_zh"],
            "name_en": noun["name_en"],
            "abbreviation": noun["abbreviation"],
            "short_name_zh": noun["short_name_zh"],
            "full_name_en": noun["full_name_en"],
            "sort_order": noun["sort_order"]
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "基本词性详情回读失败：{part_detail}"
    );
    assert_eq!(part_detail["revision"], 2);
    assert_eq!(part_detail["usage_count"], 2);

    let (status, _, sub_detail, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": n_count["name_zh"],
            "name_en": n_count["name_en"],
            "short_name_zh": n_count["short_name_zh"],
            "abbreviation": n_count["abbreviation"],
            "full_name_en": n_count["full_name_en"],
            "sort_order": n_count["sort_order"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "细分词性详情回读失败：{sub_detail}");
    assert_eq!(sub_detail["revision"], 2);
    assert_eq!(sub_detail["usage_count"], 2);

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "引用中的细分词性不可删除：{body}"
    );
    assert_eq!(body["code"], "sub_part_of_speech_in_use");
    assert_eq!(body["meta"]["usage_count"], 2);

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "引用中的基本词性不可删除：{body}"
    );
    assert_eq!(body["code"], "part_of_speech_in_use");
    assert_eq!(body["meta"]["usage_count"], 2);

    // 清除所有当前草稿/历史 publication 引用后，预检应允许删除。
    sqlx::query("DELETE FROM lexicon.entry_publications WHERE entry_id = ANY($1)")
        .bind(vec![active.entry_id, historical.entry_id])
        .execute(&pool)
        .await
        .expect("清理 usage 测试 publications 应成功");
    sqlx::query("DELETE FROM lexicon.senses WHERE id = $1")
        .bind(active.sense_node_id)
        .execute(&pool)
        .await
        .expect("清理 active draft sense 应成功");
    sqlx::query("DELETE FROM lexicon.entry_pos WHERE id = $1")
        .bind(active.pos_node_id)
        .execute(&pool)
        .await
        .expect("清理 active draft POS 应成功");

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());

    // 引用清空后仍挂着其余种子细分词性：先拦下，清空细分词性后才能删。
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "part_of_speech_has_sub_parts");

    sqlx::query("DELETE FROM catalog.sub_parts_of_speech WHERE part_of_speech_id = $1")
        .bind(part_id)
        .execute(&pool)
        .await
        .expect("清空剩余细分词性应成功");

    // 名下还挂着词形变化时同样拦下，要求先清空。
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "part_of_speech_has_form_types");
    sqlx::query("DELETE FROM catalog.form_types WHERE part_of_speech_id = $1")
        .bind(part_id)
        .execute(&pool)
        .await
        .expect("清空剩余词形变化应成功");

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());
}

#[sqlx::test]
async fn form_type_catalog_crud_permissions_revision_and_base_protection(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let root = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let admin = seed_admin(&pool, AdminRole::Admin, false).await;
    let bearer = token(&state, root, AdminRole::SuperAdmin);
    let admin_token = token(&state, admin, AdminRole::Admin);
    let path = "/api/v1/admin/settings/form-types";
    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let input = json!({"part_of_speech_id":noun_id,"code":"custom_variant","name_zh":"自定义词形","short_name_zh":"自定义","name_en":"Custom variant","abbreviation":"custom","full_name_en":"custom variant","sort_order":100});
    let (status, _, _, _) = call(
        &state,
        Method::POST,
        path,
        Some(&admin_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, created, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let (status, _, duplicate, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(duplicate["code"], "form_type_conflict");
    let mut update = input.clone();
    update.as_object_mut().unwrap().remove("code");
    update["base_revision"] = json!(1);
    update["name_zh"] = json!("新词形名称");
    let (status, _, saved, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{id}"),
        Some(&bearer),
        Some(update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["code"], "custom_variant");
    assert_eq!(saved["revision"], 2);
    let (status, _, conflict, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{id}"),
        Some(&bearer),
        Some(update),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "revision_conflict");
    let (status, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&admin_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        catalog["form_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "custom_variant" && f["name_zh"] == "新词形名称")
    );
    // 新建的词形只进它所属词性的候选，不再对所有词性可用。
    for part in catalog["items"].as_array().unwrap() {
        let allowed = part["allowed_form_types"].as_array().unwrap();
        assert_eq!(
            allowed.contains(&json!("custom_variant")),
            part["code"] == "noun",
            "{part}"
        );
    }
    // 原形对所有词性通用：既不能新建一个同码的，也不能给它指定归属。
    let (status, _, base_dup, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(json!({"part_of_speech_id":noun_id,"code":"base","name_zh":"另一个原形","short_name_zh":"另形","name_en":"Another base","abbreviation":"base2","full_name_en":"another base form","sort_order":300})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "原形只能有一个且必须全局：{base_dup}"
    );
    assert_eq!(base_dup["code"], "invalid_form_type");

    // 改原形时不接受归属：这条守卫在 handler 里，撞不到数据库那层。
    let base_id = catalog["form_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "base")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, base_move, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{base_id}"),
        Some(&bearer),
        Some(json!({"base_revision":1,"part_of_speech_id":noun_id,"name_zh":"原形","short_name_zh":"原形","name_en":"Base form","abbreviation":"base","full_name_en":"base form","sort_order":0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{base_move}");
    assert_eq!(base_move["code"], "invalid_form_type");
    assert_eq!(base_move["field"], "part_of_speech_id");

    // 这次把唯一索引从全局改成同一词性内的全部意义：形容词与副词可以各有一个「比较级」。
    let adjective_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'adjective'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let adverb_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'adverb'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let twin = |part_id: Uuid, code: &str| json!({"part_of_speech_id":part_id,"code":code,"name_zh":"同名词形","short_name_zh":"同名","name_en":"Twin form","abbreviation":"twin","full_name_en":"twin form","sort_order":200});
    let (status, _, first, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(twin(adjective_id, "adjective_twin")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, _, second, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(twin(adverb_id, "adverb_twin")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "不同词性下允许同名词形：{second}"
    );
    assert_eq!(second["name_zh"], "同名词形");
    assert_eq!(second["part_of_speech_id"], adverb_id.to_string());
    // 同一个词性下仍然不允许重名。
    let (status, _, dup, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(twin(adverb_id, "adverb_twin_again")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{dup}");
    assert_eq!(dup["code"], "form_type_conflict");

    // 列表按词性过滤：只返回该词性名下的，外加对所有词性通用的原形。
    let (status, _, scoped, _) = call(
        &state,
        Method::GET,
        &format!("{path}?part_of_speech_id={adverb_id}"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{scoped}");
    let scoped_codes: Vec<&str> = scoped["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert_eq!(scoped_codes, vec!["base", "adverb_twin"], "{scoped}");

    // 改挂到另一个词性：候选跟着走。
    let adverb_twin_id = second["id"].as_str().unwrap();
    let (status, _, moved, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{adverb_twin_id}"),
        Some(&bearer),
        Some(json!({"base_revision":1,"part_of_speech_id":adjective_id,"name_zh":"改挂后的词形","short_name_zh":"改挂","name_en":"Moved form","abbreviation":"moved","full_name_en":"moved form","sort_order":200})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["part_of_speech_id"], adjective_id.to_string());
    let (status, _, after_move, _) = call(
        &state,
        Method::GET,
        &format!("{path}?part_of_speech_id={adverb_id}"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after_move["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["base"],
        "改挂之后副词名下只剩通用的原形：{after_move}"
    );

    // 归属必须指向存在的词性。
    let (status, _, missing, _) = call(
        &state,
        Method::POST,
        path,
        Some(&bearer),
        Some(twin(Uuid::now_v7(), "ghost_owner_form")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(missing["code"], "part_of_speech_not_found");

    let base = catalog["form_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "base")
        .unwrap();
    let (status, _, blocked, _) = call(
        &state,
        Method::DELETE,
        &format!("{path}/{}?base_revision=1", base["id"].as_str().unwrap()),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(blocked["code"], "form_type_required");
    let (status, _, _, _) = call(
        &state,
        Method::DELETE,
        &format!("{path}/{id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&admin_token),
        None,
    )
    .await;
    assert!(
        !catalog["form_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "custom_variant")
    );
}

#[sqlx::test]
async fn sub_part_code_carries_the_code_text_and_freezes_once_referenced(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);
    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let path = format!("{ROOT}/{noun_id}/sub-parts");

    // 代码文本落在编码上，正式英文只是展示名：两条更细的划分可以共用 N-UNCOUNT。
    let (status, _, mass, _) = call(
        &state,
        Method::POST,
        &path,
        Some(&bearer),
        Some(json!({
            "code": "N-UNCOUNT-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建物质名词失败：{mass}");
    let mass_id = mass["id"].as_str().unwrap().to_owned();

    let (status, _, abstract_noun, _) = call(
        &state,
        Method::POST,
        &path,
        Some(&bearer),
        Some(json!({
            "code": "N-UNCOUNT-ABSTRACT",
            "name_zh": "不可数抽象名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable abstract noun",
            "sort_order": 20
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "同一基本词性下正式英文应允许重复：{abstract_noun}"
    );

    // 父级对不上时先落 404，不能被新加的编码守卫改写成别的错误码。
    let other_part: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'verb'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _, wrong_parent, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{other_part}/sub-parts/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "code": "N-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "错误父级应 404：{wrong_parent}"
    );
    assert_eq!(wrong_parent["code"], "sub_part_of_speech_not_found");

    let (status, _, updated, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "code": "N-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未被引用时应能改编码：{updated}");
    assert_eq!(updated["code"], "N-MASS");
    assert_eq!(updated["revision"], 2);

    // 省略 code 表示不改：其余字段照常更新，编码保持原值。
    let (status, _, kept, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 2,
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "物质",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "省略编码应放行：{kept}");
    assert_eq!(kept["code"], "N-MASS", "省略 code 不应改动编码");
    assert_eq!(kept["short_name_zh"], "物质");
    assert_eq!(kept["revision"], 3);

    // 编码撞车仍然是 409，字段指向 code。
    let (status, _, conflict, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 3,
            "code": "N-UNCOUNT-ABSTRACT",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "编码撞车应 409：{conflict}");
    assert_eq!(conflict["code"], "sub_part_of_speech_conflict");
    assert_eq!(conflict["field"], "code");

    let mass_uuid: Uuid = mass_id.parse().unwrap();
    seed_lexicon_usage(&pool, admin_id, noun_id, mass_uuid, 0, true).await;

    let (status, _, blocked, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 3,
            "code": "N-UNCOUNT-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "不可数名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "被词义引用后不应能改编码：{blocked}"
    );
    assert_eq!(blocked["code"], "sub_part_of_speech_in_use");
    assert_eq!(blocked["meta"]["usage_count"], 1);

    // 过期 revision 撞上引用守卫时，如实报并发冲突，不能报成「已被引用」。
    let (status, _, stale, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "code": "N-UNCOUNT-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "物质",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "过期 revision 应 409：{stale}"
    );
    assert_eq!(stale["code"], "revision_conflict");
    assert_eq!(stale["meta"]["current_revision"], 3);

    // 编码没变时照常放行，其余展示字段仍可改。
    let (status, _, renamed, _) = call(
        &state,
        Method::PATCH,
        &format!("{path}/{mass_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 3,
            "code": "N-MASS",
            "name_zh": "不可数物质名词",
            "name_en": "N-UNCOUNT",
            "short_name_zh": "物质名词",
            "abbreviation": "n.",
            "full_name_en": "uncountable material noun",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "编码不变时应放行：{renamed}");
    assert_eq!(renamed["short_name_zh"], "物质名词");
}
