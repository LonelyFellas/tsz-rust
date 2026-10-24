//! `GET /api/v1/admin/dashboard/overview` 的聚合口径与鉴权契约。

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    config::SmartLexiconV3Flags,
    state::AppState,
};

const URI: &str = "/api/v1/admin/dashboard/overview";

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("1{}", id.as_u128()),
            display_name: "看板测试管理员".to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn admin_token(state: &AppState, id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(id, AdminRole::Admin.as_str())
        .expect("签 admin token 应成功")
}

async fn get(state: &AppState, token: Option<&str>) -> (StatusCode, String) {
    let mut request = Request::builder().method("GET").uri(URI);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn seed_user(pool: &PgPool, created_at: DateTime<Utc>, status: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, phone, password_hash, display_name, status, created_at)
         VALUES ($1, $2, 'hashed-pw', '看板用户', $3, $4)",
    )
    .bind(id)
    .bind(format!("2{}", id.as_u128()))
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await
    .expect("seed user 应成功");
    id
}

async fn seed_task(pool: &PgPool, user_id: Uuid, state: &str, created_at: DateTime<Utc>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO learning_tasks
            (id, user_id, name, question_type, task_type, wordlist_ids,
             daily_question_count, state, create_key, create_hash, created_at)
         VALUES ($1, $2, '看板任务', 'spelling_zh_to_en', 'daily', ARRAY[$3::uuid],
                 1, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(user_id)
    .bind(Uuid::now_v7())
    .bind(state)
    .bind(Uuid::now_v7())
    .bind(vec![0_u8; 32])
    .bind(created_at)
    .execute(pool)
    .await
    .expect("seed task 应成功");
    id
}

/// 插入一个当日学习轮次，business_day / 窗口按学习业务的北京时间 04:00 换日计算。
async fn seed_daily_run(pool: &PgPool, task_id: Uuid, user_id: Uuid, state: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        WITH bounds AS (
            SELECT
                ((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date AS business_day,
                (((((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date)::timestamp)
                    + time '04:00') AT TIME ZONE 'Asia/Shanghai' AS day_start,
                (((((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date + 1)::timestamp)
                    + time '04:00') AT TIME ZONE 'Asia/Shanghai' AS day_end
        )
        INSERT INTO learning_runs
            (id, task_id, user_id, task_revision, question_type, task_type,
             business_day, window_start, window_end, expires_at, settings_snapshot,
             seed, target_count, state, generation_version, grading_version, started_at)
        SELECT $1, $2, $3, 1, 'spelling_zh_to_en', 'daily',
               b.business_day, b.day_start, b.day_end, b.day_end, '{}'::jsonb,
               $4, 1, $5, 'test-generation', 'test-grading', b.day_start
        FROM bounds b
        "#,
    )
    .bind(id)
    .bind(task_id)
    .bind(user_id)
    .bind(Uuid::now_v7())
    .bind(state)
    .execute(pool)
    .await
    .expect("seed run 应成功");
    id
}

async fn seed_completion(pool: &PgPool, run_id: Uuid, task_id: Uuid, user_id: Uuid) {
    sqlx::query(
        r#"
        INSERT INTO learning_completions
            (id, run_id, task_id, user_id, question_type, task_type, business_day,
             task_revision, target_count, answered_count, correct_count, completed_at,
             generation_version, grading_version)
        SELECT $1, $2, $3, $4, 'spelling_zh_to_en', 'daily',
               ((now() AT TIME ZONE 'Asia/Shanghai') - interval '4 hours')::date,
               1, 1, 1, 1, now(), 'test-generation', 'test-grading'
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(task_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seed completion 应成功");
}

async fn seed_answer(pool: &PgPool, run_id: Uuid) {
    let question_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO learning_questions
            (id, run_id, position, source_wordlist_id, source_revision,
             source_membership_id, entry_id, entry_archive_generation, publication_id,
             pos_id, sense_id, definition_id, form_ids, unit_key, prompt_snapshot,
             answer_snapshot, fingerprint)
         VALUES ($1, $2, 0, $3, 1, $4, $5, 0, $6, $7, $8, $9,
                 ARRAY[$10::uuid], 'unit', '{}'::jsonb, '{}'::jsonb, $11)",
    )
    .bind(question_id)
    .bind(run_id)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(vec![0_u8; 32])
    .execute(pool)
    .await
    .expect("seed question 应成功");
    sqlx::query(
        "INSERT INTO learning_answers
            (id, run_id, question_id, request_key, request_hash, submitted_answer,
             normalized_answer, is_correct, accepted_at, grading_version)
         VALUES ($1, $2, $3, $4, $5, 'apple', 'apple', true, now(), 'test-grading')",
    )
    .bind(Uuid::now_v7())
    .bind(run_id)
    .bind(question_id)
    .bind(Uuid::now_v7())
    .bind(vec![0_u8; 32])
    .execute(pool)
    .await
    .expect("seed answer 应成功");
}

async fn seed_entry(
    pool: &PgPool,
    admin_id: Uuid,
    created_at: DateTime<Utc>,
    archived: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO lexicon.entries
            (id, content_schema_version, language, kind, revision,
             detection_snapshot, created_by_admin_id, updated_by_admin_id,
             created_at, archived_at)
         VALUES ($1, 3, 'en', 'word', 1, '{}'::jsonb, $2, $2, $3, $4)",
    )
    .bind(id)
    .bind(admin_id)
    .bind(created_at)
    .bind(archived.then(Utc::now))
    .execute(pool)
    .await
    .expect("seed lexicon entry 应成功");
    id
}

async fn seed_wallet(pool: &PgPool, owner_type: &str, owner_id: Uuid, balance: i64) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO coin_wallets (id, owner_type, owner_id, balance)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(owner_type)
    .bind(owner_id)
    .bind(balance)
    .execute(pool)
    .await
    .expect("seed wallet 应成功");
    id
}

async fn seed_operation(
    pool: &PgPool,
    kind: &str,
    actor_type: &str,
    actor_id: Option<Uuid>,
    source_type: &str,
    source_id: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO coin_operations
            (id, kind, actor_type, actor_id, idempotency_scope, idempotency_key,
             request_hash, source_type, source_id, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '看板测试')",
    )
    .bind(id)
    .bind(kind)
    .bind(actor_type)
    .bind(actor_id)
    .bind(format!("dashboard-test-{id}"))
    .bind(format!("key-{id}"))
    .bind(vec![0_u8; 32])
    .bind(source_type)
    .bind(source_id)
    .execute(pool)
    .await
    .expect("seed operation 应成功");
    id
}

async fn seed_entry_row(
    pool: &PgPool,
    operation_id: Uuid,
    wallet_id: Uuid,
    delta: i64,
    balance_after: i64,
    created_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO coin_entries (operation_id, wallet_id, delta, balance_after, created_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(operation_id)
    .bind(wallet_id)
    .bind(delta)
    .bind(balance_after)
    .bind(created_at)
    .execute(pool)
    .await
    .expect("seed coin entry 应成功");
}

#[sqlx::test]
async fn overview_aggregates_real_data_by_beijing_boundaries(pool: PgPool) {
    let state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let admin = seed_admin(&pool).await;

    let now = Utc::now();
    let user_a = seed_user(&pool, now, "active").await;
    let user_b = seed_user(&pool, now - Duration::days(1), "active").await;
    let user_c = seed_user(&pool, now - Duration::days(2), "disabled").await;
    let user_d = seed_user(&pool, now - Duration::days(8), "active").await;
    let _user_d = user_d;
    let user_e = seed_user(&pool, now - Duration::days(20), "active").await;

    // A 完成今天一轮；B 今天已开始但未完成；E 今天未开始；C 已禁用，不计入学习基数。
    let task_a = seed_task(&pool, user_a, "active", now).await;
    let _task_a_archived = seed_task(&pool, user_a, "archived", now - Duration::days(10)).await;
    let task_b = seed_task(&pool, user_b, "active", now).await;
    let _task_c = seed_task(&pool, user_c, "active", now - Duration::days(10)).await;
    let _task_e = seed_task(&pool, user_e, "active", now - Duration::days(10)).await;
    let run_a = seed_daily_run(&pool, task_a, user_a, "completed").await;
    seed_completion(&pool, run_a, task_a, user_a).await;
    let run_b = seed_daily_run(&pool, task_b, user_b, "active").await;
    seed_answer(&pool, run_b).await;

    // 词条：今天 / 8 天前 / 21 天前各一条未归档；50 天前一条超出 6 周；
    // 今天一条已归档。归档不计入，其余四条计入。
    let _today_entry = seed_entry(&pool, admin, now, false).await;
    let _eight_days_entry = seed_entry(&pool, admin, now - Duration::days(8), false).await;
    let _three_weeks_entry = seed_entry(&pool, admin, now - Duration::days(21), false).await;
    let _old_entry = seed_entry(&pool, admin, now - Duration::days(50), false).await;
    let _archived_entry = seed_entry(&pool, admin, now, true).await;

    // 天生币：平台发放 + 注销作废计入；用户间投币不计入发放/扣除，但计入余额。
    let wallet_a = seed_wallet(&pool, "user", user_a, 5).await;
    let wallet_b = seed_wallet(&pool, "user", user_b, 13).await;
    let wallet_c = seed_wallet(&pool, "user", user_c, 5).await;
    let _admin_wallet = seed_wallet(&pool, "admin", admin, 999).await;

    let grant_a = seed_operation(&pool, "credit", "system", None, "daily_login_reward", "a").await;
    seed_entry_row(&pool, grant_a, wallet_a, 10, 10, now).await;
    let grant_b = seed_operation(&pool, "credit", "system", None, "daily_login_reward", "b").await;
    seed_entry_row(&pool, grant_b, wallet_b, 10, 10, now).await;
    let grant_c = seed_operation(&pool, "credit", "system", None, "daily_login_reward", "c").await;
    seed_entry_row(&pool, grant_c, wallet_c, 5, 5, now).await;

    let tip = seed_operation(
        &pool,
        "transfer",
        "user",
        Some(user_a),
        "wordlist_tip",
        "tip",
    )
    .await;
    seed_entry_row(&pool, tip, wallet_a, -5, 5, now).await;
    seed_entry_row(&pool, tip, wallet_b, 5, 15, now).await;

    let forfeit = seed_operation(
        &pool,
        "account_closure_forfeit",
        "system",
        None,
        "account_closure",
        "b",
    )
    .await;
    seed_entry_row(&pool, forfeit, wallet_b, -2, 13, now).await;

    let token = admin_token(&state, admin);
    let (status, body) = get(&state, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).expect("响应应为 JSON");

    assert_eq!(response["users"]["total"], 5);
    assert_eq!(response["users"]["today"], 1);
    assert_eq!(response["users"]["last_3_days"], 3);
    assert_eq!(response["users"]["last_7_days"], 3);

    assert_eq!(response["tasks"]["total"], 5);
    assert_eq!(response["tasks"]["completed"], 1);
    assert_eq!(response["tasks"]["today"], 2);
    assert_eq!(response["tasks"]["week"], 2);
    assert_eq!(
        response["learning_progress"],
        json!({"completed": 1, "in_progress": 1, "not_started": 1})
    );

    assert_eq!(response["lexicon"]["total"], 4);
    assert_eq!(response["lexicon"]["today"], 1);
    assert_eq!(response["lexicon"]["week"], 1);
    assert!(response["lexicon"]["month"].as_i64().unwrap() >= 1);
    let weekly_values: Vec<i64> = response["lexicon_creation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["value"].as_i64().unwrap())
        .collect();
    assert_eq!(weekly_values.len(), 6);
    assert_eq!(weekly_values.iter().sum::<i64>(), 3);

    assert_eq!(response["coins"]["circulation"], "23");
    assert_eq!(response["coins"]["today_net"], "23");
    assert_eq!(response["coins"]["week_net"], "23");
    assert_eq!(response["coins"]["month_net"], "23");

    let growth: Vec<i64> = response["user_growth"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["value"].as_i64().unwrap())
        .collect();
    assert_eq!(growth.len(), 14);
    assert_eq!(growth.iter().sum::<i64>(), 4);
    assert_eq!(growth[13], 1);

    let flow = response["coin_flow"].as_array().unwrap();
    assert_eq!(flow.len(), 7);
    assert_eq!(flow[6]["grant"], "25");
    assert_eq!(flow[6]["deduct"], "2");
    assert!(
        response["generated_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

#[sqlx::test]
async fn overview_respects_the_lexicon_read_flag(pool: PgPool) {
    let admin = seed_admin(&pool).await;
    let now = Utc::now();
    seed_entry(&pool, admin, now, false).await;
    seed_entry(&pool, admin, now, false).await;

    let disabled_state = AppState::for_test(pool.clone());
    let (status, body) = get(&disabled_state, Some(&admin_token(&disabled_state, admin))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["lexicon"]["total"], 0);

    let enabled_state = AppState::for_test_with_smart_lexicon_v3_flags(
        pool.clone(),
        SmartLexiconV3Flags::all_enabled(),
    );
    let (status, body) = get(&enabled_state, Some(&admin_token(&enabled_state, admin))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["lexicon"]["total"], 2);
    assert_eq!(response["lexicon"]["today"], 2);
}

#[sqlx::test]
async fn overview_requires_an_active_admin_token(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let (status, body) = get(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let admin = seed_admin(&pool).await;
    sqlx::query("UPDATE admins SET status='disabled' WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    let token = admin_token(&state, admin);
    let (status, body) = get(&state, Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
