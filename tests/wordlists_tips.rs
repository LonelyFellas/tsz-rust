mod account_deletion_support;
mod wordlists_support;
use account_deletion_support::{apply, call, deadline, fund, setup};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
use wordlists_support::*;
fn intent(amount: &str) -> Value {
    json!({"event_id":Uuid::now_v7(),"idempotency_key":Uuid::now_v7(),"amount":amount})
}
async fn publish_list(pool: &PgPool, state: &AppState, auth: &AuthUser, entry: Uuid) -> Uuid {
    let created = call(
        state,
        auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[entry]),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    let id = created.1["id"].as_str().unwrap();
    let pending = call(
        state,
        auth,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = pending.1["id"].as_str().unwrap();
    let reviewer = admin(pool).await;
    let result = admin_call(
        state,
        reviewer,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
        json!({"expected_revision":2,"approve":true,"reason":null}),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    id.parse().unwrap()
}
async fn balance(pool: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COALESCE((SELECT balance FROM coin_wallets WHERE owner_type='user' AND owner_id=$1),0)").bind(user).fetch_one(pool).await.unwrap()
}
#[sqlx::test]
async fn independent_events_atomic_transfer_replays_and_financial_retention(pool: PgPool) {
    let (state, author) = setup(&pool).await;
    let (_, payer) = setup(&pool).await;
    let e = entry(&pool, "tips").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 100).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    let input = intent("7");
    let (one, two) = tokio::join!(
        call(&state, &payer, "POST", &path, input.clone()),
        call(&state, &payer, "POST", &path, input.clone())
    );
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(one, two);
    assert_eq!(balance(&pool, payer.subject).await, 93);
    assert_eq!(balance(&pool, author.subject).await, 7);
    let mut changed = input.clone();
    changed["amount"] = json!("8");
    assert_eq!(
        call(&state, &payer, "POST", &path, changed).await.0,
        StatusCode::CONFLICT
    );
    let mut changed = input.clone();
    changed["idempotency_key"] = json!(Uuid::now_v7());
    assert_eq!(
        call(&state, &payer, "POST", &path, changed).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("3")).await.0,
        StatusCode::OK
    );
    assert_eq!(balance(&pool, payer.subject).await, 90);
    assert_eq!(balance(&pool, author.subject).await, 10);
    assert_eq!(
        call(&state, &author, "POST", &path, intent("1")).await.0,
        StatusCode::FORBIDDEN
    );
    for amount in ["0", "-1", "01", "+1", "1.0", "9223372036854775808"] {
        assert_eq!(
            call(&state, &payer, "POST", &path, intent(amount)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("91")).await.0,
        StatusCode::CONFLICT
    );
    let count:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_tips),(SELECT count(*) FROM coin_operations WHERE source_type='wordlist_tip'),(SELECT COALESCE(sum(e.delta),0)::bigint FROM coin_entries e JOIN coin_operations o ON o.id=e.operation_id WHERE o.source_type='wordlist_tip')").fetch_one(&pool).await.unwrap();
    assert_eq!(count, (2, 2, 0));
    let request = apply(&state, &author, "10").await;
    assert_eq!(
        call(&state, &payer, "POST", &path, input).await.0,
        StatusCode::CONFLICT
    );
    deadline(&pool, request.id, -1).await;
    assert!(
        tsz_rust::account_deletion::service::complete(&pool, author.subject, request.id)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlists")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let history = call(
        &state,
        &payer,
        "GET",
        "/api/v1/me/wordlist-tips",
        Value::Null,
    )
    .await;
    assert_eq!(history.0, StatusCode::OK, "{}", history.1);
    assert_eq!(history.1["items"].as_array().unwrap().len(), 2);
    assert!(history.1["items"][0]["wordlist_name"].is_null());
    assert!(
        !history.1["items"][0]["wordlist_accessible"]
            .as_bool()
            .unwrap()
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../migrations/20261006050000_wordlist_tips.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}
#[sqlx::test]
async fn publication_updates_allow_tips_but_archive_restore_requires_new_review(pool: PgPool) {
    let (state, author) = setup(&pool).await;
    let (_, payer) = setup(&pool).await;
    let e = entry(&pool, "version one").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 100).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    publish(&pool, e, admin(&pool).await, "version two", 2).await;
    sqlx::query("UPDATE lexicon.entries SET lifecycle_revision=lifecycle_revision+1 WHERE id=$1")
        .bind(e)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp(),lifecycle_revision=lifecycle_revision+1 WHERE id=$1").bind(e).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=NULL,lifecycle_revision=lifecycle_revision+1 WHERE id=$1").bind(e).execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
        StatusCode::CONFLICT
    );
    let mine = format!("/api/v1/me/wordlists/{id}");
    assert_eq!(
        call(
            &state,
            &author,
            "POST",
            &format!("{mine}/withdraw"),
            json!({"expected_revision":3})
        )
        .await
        .0,
        StatusCode::OK
    );
    let pending = call(
        &state,
        &author,
        "POST",
        &format!("{mine}/review-requests"),
        json!({"expected_revision":4,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = pending.1["id"].as_str().unwrap();
    assert_eq!(
        admin_call(
            &state,
            admin(&pool).await,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":5,"approve":true,"reason":null})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
        StatusCode::OK
    );
}
#[sqlx::test]
async fn concurrent_lists_do_not_deadlock_and_fact_failure_rolls_back_wallets(pool: PgPool) {
    let (state, author) = setup(&pool).await;
    let (_, payer) = setup(&pool).await;
    let e = entry(&pool, "parallel tips").await;
    let a = publish_list(&pool, &state, &author, e).await;
    let b = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 10).await;
    let path_a = format!("/api/v1/wordlists/{a}/tips");
    let path_b = format!("/api/v1/wordlists/{b}/tips");
    let (one, two) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            call(&state, &payer, "POST", &path_a, intent("3")),
            call(&state, &payer, "POST", &path_b, intent("3"))
        )
    })
    .await
    .unwrap();
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(two.0, StatusCode::OK, "{}", two.1);
    sqlx::raw_sql("CREATE FUNCTION fail_tip_fact() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test fact failure'; END $$; CREATE TRIGGER fail_tip_fact BEFORE INSERT ON wordlist_tips FOR EACH ROW EXECUTE FUNCTION fail_tip_fact();").execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path_a, intent("1")).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, payer.subject).await, 4);
    assert_eq!(balance(&pool, author.subject).await, 6);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE source_type='wordlist_tip'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}

#[sqlx::test]
async fn withdraw_serializes_with_tip_and_receipts_remain_owner_scoped(pool: PgPool) {
    let (state, author) = setup(&pool).await;
    let (_, payer) = setup(&pool).await;
    let (_, stranger) = setup(&pool).await;
    let e = entry(&pool, "withdraw race").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 20).await;
    let input = intent("7");
    let path = format!("/api/v1/wordlists/{id}/tips");
    assert_eq!(
        call(&state, &payer, "POST", &path, input.clone()).await.0,
        StatusCode::OK
    );
    let receipt = format!(
        "/api/v1/me/wordlist-tips/{}",
        input["event_id"].as_str().unwrap()
    );
    assert_eq!(
        call(&state, &stranger, "GET", &receipt, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let state2 = state.clone();
    let auth = AuthUser {
        subject: payer.subject,
        role: payer.role.clone(),
        security_version: payer.security_version,
    };
    let path2 = path.clone();
    let waiting =
        tokio::spawn(async move { call(&state2, &auth, "POST", &path2, intent("2")).await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async{loop{let wait:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();if wait{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
    sqlx::query("UPDATE wordlists SET state='withdrawn',revision=revision+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(waiting.await.unwrap().0, StatusCode::NOT_FOUND);
    assert_eq!(balance(&pool, payer.subject).await, 13);
    assert_eq!(balance(&pool, author.subject).await, 7);
    assert_eq!(
        call(&state, &payer, "GET", &receipt, Value::Null).await.0,
        StatusCode::OK
    );
    let pending = apply(&state, &payer, "13").await;
    assert_eq!(
        call(&state, &payer, "GET", &receipt, Value::Null).await.0,
        StatusCode::OK
    );
    assert_ne!(
        call(&state, &payer, "POST", &path, input).await.0,
        StatusCode::OK
    );
    deadline(&pool, pending.id, -1).await;
    assert_eq!(
        call(&state, &payer, "GET", &receipt, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
}
