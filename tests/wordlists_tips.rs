mod account_deletion_support;
mod wordlists_support;
use account_deletion_support::{apply, call, deadline, fund, setup_bound};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;
use wordlists_support::*;
fn intent(amount: &str) -> Value {
    json!({"event_id":Uuid::now_v7(),"idempotency_key":Uuid::now_v7(),"amount":amount})
}
fn liked(mut intent: Value) -> Value {
    intent["like"] = json!(true);
    intent
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
async fn fixed_amounts_cumulative_limit_atomic_like_and_replays(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let e = entry(&pool, "tips").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 100).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    let input = liked(intent("2"));
    let input_event = input["event_id"].clone();
    let (one, two) = tokio::join!(
        call(&state, &payer, "POST", &path, input.clone()),
        call(&state, &payer, "POST", &path, input.clone())
    );
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(one, two);
    assert_eq!(balance(&pool, payer.subject).await, 98);
    assert_eq!(balance(&pool, author.subject).await, 2);
    let states = call(
        &state,
        &payer,
        "GET",
        &format!("/api/v1/me/wordlist-tip-states?ids={id}"),
        Value::Null,
    )
    .await;
    assert_eq!(states.1["items"][0]["tip_amount"], "2");
    let inserted: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='like'").bind(id).bind(payer.subject).fetch_one(&pool).await.unwrap();
    assert_eq!(inserted, 1);
    // Replaying the same settled intent neither charges again nor re-notifies the like.
    assert_eq!(
        call(&state, &payer, "POST", &path, input.clone()).await.0,
        StatusCode::OK
    );
    let (tips, notifications): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM wordlist_tips),(SELECT count(*) FROM user_notifications)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tips, notifications), (1, 2));
    for (field, value) in [("amount", json!("1")), ("like", json!(false))] {
        let mut changed = input.clone();
        changed[field] = value;
        let rejected = call(&state, &payer, "POST", &path, changed).await;
        assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
        assert_eq!(rejected.1["code"], "coin_idempotency_conflict");
    }
    let mut changed = intent("2");
    changed["event_id"] = input_event;
    let rejected = call(&state, &payer, "POST", &path, changed).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    assert_eq!(rejected.1["code"], "coin_source_conflict");
    assert_eq!(
        call(
            &state,
            &payer,
            "PUT",
            &format!("/api/v1/me/wordlists/{id}/interactions"),
            json!({"kind":"like","active":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    // Replaying a successful tip must never resurrect a subsequently cancelled like.
    assert_eq!(call(&state, &payer, "POST", &path, input).await, one);
    let rejected = call(&state, &payer, "POST", &path, liked(intent("1"))).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    assert_eq!(rejected.1["code"], "word_list_tip_limit");
    assert_eq!(balance(&pool, payer.subject).await, 98);
    let likes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(payer.subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(likes, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_notifications")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        call(&state, &author, "POST", &path, intent("1")).await.0,
        StatusCode::FORBIDDEN
    );
    for amount in [
        "0",
        "-1",
        "01",
        "+1",
        "1.0",
        "3",
        "91",
        "9223372036854775808",
    ] {
        assert_eq!(
            call(&state, &payer, "POST", &path, intent(amount)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let count:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_tips),(SELECT count(*) FROM coin_operations WHERE source_type='wordlist_tip'),(SELECT COALESCE(sum(e.delta),0)::bigint FROM coin_entries e JOIN coin_operations o ON o.id=e.operation_id WHERE o.source_type='wordlist_tip')").fetch_one(&pool).await.unwrap();
    assert_eq!(count, (1, 1, 0));
    let notifications = call(
        &state,
        &author,
        "GET",
        "/api/v1/me/notifications",
        Value::Null,
    )
    .await;
    assert_eq!(notifications.0, StatusCode::OK, "{}", notifications.1);
    assert_eq!(notifications.1["total"], 2);
    let items = notifications.1["items"].as_array().unwrap();
    assert_eq!(items[0]["kind"], "wordlist_tipped");
    assert_eq!(items[0]["amount"], "2");
    assert_eq!(items[1]["kind"], "wordlist_liked");
    assert_eq!(items[0]["wordlist_id"], id.to_string());
    assert_eq!(items[0]["actor_name"], "Deletion");
    // Pre-liked lists are not re-notified when a tip asks for the same like.
    let second = publish_list(&pool, &state, &author, e).await;
    let second_path = format!("/api/v1/wordlists/{second}/tips");
    assert_eq!(
        call(
            &state,
            &payer,
            "PUT",
            &format!("/api/v1/me/wordlists/{second}/interactions"),
            json!({"kind":"like","active":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM user_notifications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &second_path, liked(intent("1")))
            .await
            .0,
        StatusCode::OK
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM user_notifications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before + 1);
    // Reversing the additive migration loses only social relationships, never financial facts.
    let mut migration_tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261015010000_wordlist_interactions.down.sql"
    ))
    .execute(&mut *migration_tx)
    .await
    .unwrap();
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_tips")
        .fetch_one(&mut *migration_tx)
        .await
        .unwrap();
    assert_eq!(retained, 2);
    migration_tx.rollback().await.unwrap();
    let request = apply(&state, &author, "3").await;
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_interactions")
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
    let states = call(
        &state,
        &payer,
        "GET",
        &format!("/api/v1/me/wordlist-tip-states?ids={id}"),
        Value::Null,
    )
    .await;
    assert_eq!(states.1["items"][0]["tip_amount"], "2");
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
async fn tipped_one_can_add_one_and_tip_states_stay_private(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let (_, stranger) = setup_bound(&pool).await;
    let e = entry(&pool, "staged tips").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 10).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("1")).await.0,
        StatusCode::OK
    );
    let states = call(
        &state,
        &payer,
        "GET",
        &format!("/api/v1/me/wordlist-tip-states?ids={id}"),
        Value::Null,
    )
    .await;
    assert_eq!(states.1["items"][0]["tip_amount"], "1");
    assert_eq!(
        call(&state, &payer, "POST", &path, liked(intent("1")))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(balance(&pool, payer.subject).await, 8);
    assert_eq!(balance(&pool, author.subject).await, 2);
    let liked_row: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='like')").bind(id).bind(payer.subject).fetch_one(&pool).await.unwrap();
    assert!(liked_row);
    assert_eq!(
        call(&state, &payer, "POST", &path, intent("2")).await.0,
        StatusCode::CONFLICT
    );
    let amount: i64 = sqlx::query_scalar("SELECT COALESCE(sum(amount),0)::bigint FROM wordlist_tips WHERE wordlist_id=$1 AND payer_user_id=$2").bind(id).bind(payer.subject).fetch_one(&pool).await.unwrap();
    assert_eq!(amount, 2);
    // Other payers keep their own zero state; batch queries answer per id.
    let stranger_states = call(
        &state,
        &stranger,
        "GET",
        &format!("/api/v1/me/wordlist-tip-states?ids={id}"),
        Value::Null,
    )
    .await;
    assert_eq!(stranger_states.1["items"][0]["tip_amount"], "0");
    // The private endpoint includes zero states, deduplicates ids, and never caches account data.
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let unknown = Uuid::now_v7();
    let uri = format!("/api/v1/me/wordlist-tip-states?ids={id},{unknown},{id}");
    let token = state
        .token_manager
        .generate_with_version(payer.subject, "student", payer.security_version)
        .unwrap();
    let response = tsz_rust::router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(&uri)
                .header("Authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CACHE_CONTROL],
        "private, no-store"
    );
    let batch: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let items = batch["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.contains(&json!({"wordlist_id":id,"tip_amount":"2"})));
    assert!(items.contains(&json!({"wordlist_id":unknown,"tip_amount":"0"})));
    let guest = tsz_rust::router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(&uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(guest.status(), StatusCode::UNAUTHORIZED);
    let excessive = vec![id.to_string(); 101].join(",");
    for query in ["".to_owned(), excessive] {
        let result = call(
            &state,
            &payer,
            "GET",
            &format!("/api/v1/me/wordlist-tip-states?ids={query}"),
            Value::Null,
        )
        .await;
        assert_eq!(result.0, StatusCode::BAD_REQUEST, "{}", result.1);
        assert_eq!(result.1["code"], "invalid_query");
    }
    assert_eq!(
        call(
            &state,
            &payer,
            "GET",
            "/api/v1/me/wordlist-tip-states?ids=not-a-uuid",
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[sqlx::test]
async fn legacy_success_replays_with_original_hash_but_blocks_new_tips(pool: PgPool) {
    use tsz_rust::{
        coins::{model::*, service as coins},
        wordlists::{service, tips::CreateWordlistTip},
    };
    // Preserve the exact pre-like serde field order, rather than hashing a JSON map.
    #[derive(serde::Serialize)]
    struct LegacyIntent {
        event_id: Uuid,
        idempotency_key: Uuid,
        amount: String,
    }
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let e = entry(&pool, "legacy tips").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 20).await;
    let input = intent("7");
    let legacy: CreateWordlistTip = serde_json::from_value(input.clone()).unwrap();
    let old = LegacyIntent {
        event_id: legacy.event_id,
        idempotency_key: legacy.idempotency_key,
        amount: "7".into(),
    };
    let old_hash = service::hash(&(payer.subject, id, author.subject, &old)).unwrap();
    assert_eq!(
        service::hash(&(payer.subject, id, author.subject, &legacy)).unwrap(),
        old_hash
    );
    let mut explicit_false = input.clone();
    explicit_false["like"] = json!(false);
    let decoded: CreateWordlistTip = serde_json::from_value(explicit_false.clone()).unwrap();
    assert_eq!(
        service::hash(&(payer.subject, id, author.subject, &decoded)).unwrap(),
        old_hash
    );
    let owners = [
        Owner {
            owner_type: OwnerType::User,
            owner_id: payer.subject,
        },
        Owner {
            owner_type: OwnerType::User,
            owner_id: author.subject,
        },
    ];
    let ctx = Context {
        actor: Actor::Account(owners[0]),
        idempotency_scope: format!("wordlist-tip:{}", payer.subject),
        idempotency_key: old.idempotency_key.to_string(),
        source_type: "wordlist_tip".into(),
        source_id: old.event_id.to_string(),
        reason: "词表投币".into(),
        evidence_ref: None,
    };
    let mut tx = pool.begin().await.unwrap();
    let receipt = coins::transfer_in(&mut tx, owners[0], owners[1], Amount::new(7).unwrap(), &ctx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO wordlist_tips(event_id,wordlist_id,payer_user_id,author_user_id,amount,request_key,request_hash,operation_id) VALUES($1,$2,$3,$4,7,$5,$6,$7)").bind(old.event_id).bind(id).bind(payer.subject).bind(author.subject).bind(old.idempotency_key).bind(old_hash).bind(receipt.operation_id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let path = format!("/api/v1/wordlists/{id}/tips");
    let replay = call(&state, &payer, "POST", &path, input.clone()).await;
    assert_eq!(replay.0, StatusCode::OK, "{}", replay.1);
    assert_eq!(replay.1["amount"], "7");
    assert_eq!(
        replay,
        call(&state, &payer, "POST", &path, explicit_false).await
    );
    let changed = call(&state, &payer, "POST", &path, liked(input)).await;
    assert_eq!(changed.0, StatusCode::CONFLICT, "{}", changed.1);
    assert_eq!(changed.1["code"], "coin_idempotency_conflict");
    for amount in ["1", "2"] {
        let rejected = call(&state, &payer, "POST", &path, liked(intent(amount))).await;
        assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
        assert_eq!(rejected.1["code"], "word_list_tip_limit");
    }
    let new_seven = call(&state, &payer, "POST", &path, intent("7")).await;
    assert_eq!(new_seven.0, StatusCode::BAD_REQUEST, "{}", new_seven.1);
    assert_eq!(new_seven.1["code"], "coin_invalid_amount");
    let states = call(
        &state,
        &payer,
        "GET",
        &format!("/api/v1/me/wordlist-tip-states?ids={id}"),
        Value::Null,
    )
    .await;
    assert_eq!(states.0, StatusCode::OK, "{}", states.1);
    assert_eq!(states.1["items"][0]["tip_amount"], "7");
    assert_eq!(balance(&pool, payer.subject).await, 13);
    assert_eq!(balance(&pool, author.subject).await, 7);
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_tips),(SELECT count(*) FROM wordlist_interactions),(SELECT count(*) FROM user_notifications)").fetch_one(&pool).await.unwrap();
    assert_eq!(
        counts,
        (1, 0, 0),
        "historical facts must not create new side effects on replay"
    );
}
#[sqlx::test]
async fn concurrent_amounts_respect_cap_and_interaction_lock_order(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let e = entry(&pool, "tip races").await;
    fund(&pool, &payer, 10).await;
    let mut total = 0;
    for amounts in [("1", "2"), ("2", "2")] {
        let id = publish_list(&pool, &state, &author, e).await;
        let path = format!("/api/v1/wordlists/{id}/tips");
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                call(&state, &payer, "POST", &path, liked(intent(amounts.0))),
                call(&state, &payer, "POST", &path, liked(intent(amounts.1)))
            )
        })
        .await
        .expect("concurrent tips must not deadlock");
        assert!(
            (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
                || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT),
            "{a:?} {b:?}"
        );
        let (success, rejected) = if a.0 == StatusCode::OK {
            (a, b)
        } else {
            (b, a)
        };
        assert_eq!(rejected.1["code"], "word_list_tip_limit");
        let tipped: i64 = sqlx::query_scalar(
            "SELECT sum(amount)::bigint FROM wordlist_tips WHERE wordlist_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            tipped,
            success.1["amount"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
        );
        assert!((1..=2).contains(&tipped));
        total += tipped;
    }
    let id = publish_list(&pool, &state, &author, e).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    let interaction_path = format!("/api/v1/me/wordlists/{id}/interactions");
    let (tip, like) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            call(&state, &payer, "POST", &path, liked(intent("1"))),
            call(
                &state,
                &payer,
                "PUT",
                &interaction_path,
                json!({"kind":"like","active":true})
            )
        )
    })
    .await
    .expect("tip and interaction must share the same lock order");
    assert_eq!(tip.0, StatusCode::OK, "{}", tip.1);
    assert_eq!(like.0, StatusCode::OK, "{}", like.1);
    total += 1;
    assert_eq!(balance(&pool, payer.subject).await, 10 - total);
    assert_eq!(balance(&pool, author.subject).await, total);
    let counts: (i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1),(SELECT count(*) FROM user_notifications WHERE wordlist_id=$1)").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        counts,
        (1, 2),
        "simultaneous like intents must notify only once"
    );
}
#[sqlx::test]
async fn insufficient_balance_and_like_failure_leave_no_partial_tip(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let e = entry(&pool, "tip rollback").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 1).await;
    let path = format!("/api/v1/wordlists/{id}/tips");
    let input = liked(intent("2"));
    let rejected = call(&state, &payer, "POST", &path, input.clone()).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    assert_eq!(rejected.1["code"], "coin_insufficient_balance");
    let self_tip = call(&state, &author, "POST", &path, liked(intent("1"))).await;
    assert_eq!(self_tip.0, StatusCode::FORBIDDEN, "{}", self_tip.1);
    fund(&pool, &payer, 1).await;
    sqlx::raw_sql("CREATE FUNCTION reject_like() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test like failure'; END $$; CREATE TRIGGER reject_like BEFORE INSERT ON wordlist_interactions FOR EACH ROW EXECUTE FUNCTION reject_like();").execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path, input.clone()).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, payer.subject).await, 2);
    assert_eq!(balance(&pool, author.subject).await, 0);
    let counts: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_tips),(SELECT count(*) FROM wordlist_interactions),(SELECT count(*) FROM user_notifications),(SELECT count(*) FROM coin_operations WHERE source_type='wordlist_tip')").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    sqlx::raw_sql("DROP TRIGGER reject_like ON wordlist_interactions")
        .execute(&pool)
        .await
        .unwrap();
    // A failed transaction does not consume either idempotency identifier or the cap.
    let retry = call(&state, &payer, "POST", &path, input).await;
    assert_eq!(retry.0, StatusCode::OK, "{}", retry.1);
    assert_eq!(balance(&pool, payer.subject).await, 0);
    assert_eq!(balance(&pool, author.subject).await, 2);
}
#[sqlx::test]
async fn publication_updates_allow_tips_but_archive_restore_requires_new_review(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
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
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let e = entry(&pool, "parallel tips").await;
    let a = publish_list(&pool, &state, &author, e).await;
    let b = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 10).await;
    let path_a = format!("/api/v1/wordlists/{a}/tips");
    let path_b = format!("/api/v1/wordlists/{b}/tips");
    let (one, two) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            call(&state, &payer, "POST", &path_a, intent("1")),
            call(&state, &payer, "POST", &path_b, liked(intent("1")))
        )
    })
    .await
    .unwrap();
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(two.0, StatusCode::OK, "{}", two.1);
    sqlx::raw_sql("CREATE FUNCTION fail_tip_fact() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test fact failure'; END $$; CREATE TRIGGER fail_tip_fact BEFORE INSERT ON wordlist_tips FOR EACH ROW EXECUTE FUNCTION fail_tip_fact();").execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path_a, liked(intent("1")))
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, payer.subject).await, 8);
    assert_eq!(balance(&pool, author.subject).await, 2);
    let likes: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='like'").bind(a).bind(payer.subject).fetch_one(&pool).await.unwrap();
    assert_eq!(likes, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE source_type='wordlist_tip'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    sqlx::raw_sql("DROP TRIGGER fail_tip_fact ON wordlist_tips; CREATE FUNCTION reject_notification() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'notification failure'; END $$; CREATE TRIGGER reject_notification BEFORE INSERT ON user_notifications FOR EACH ROW WHEN (NEW.kind='wordlist_tipped') EXECUTE FUNCTION reject_notification();").execute(&pool).await.unwrap();
    assert_eq!(
        call(&state, &payer, "POST", &path_a, liked(intent("1")))
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(balance(&pool, payer.subject).await, 8);
    assert_eq!(balance(&pool, author.subject).await, 2);
    let likes: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1 AND user_id=$2 AND kind='like'").bind(a).bind(payer.subject).fetch_one(&pool).await.unwrap();
    assert_eq!(
        likes, 0,
        "tip notification failure must roll back the newly added like"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_tips")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_notifications")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
}
#[sqlx::test]
async fn withdraw_serializes_with_tip_and_receipts_remain_owner_scoped(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, payer) = setup_bound(&pool).await;
    let (_, stranger) = setup_bound(&pool).await;
    let e = entry(&pool, "withdraw race").await;
    let id = publish_list(&pool, &state, &author, e).await;
    fund(&pool, &payer, 20).await;
    let input = liked(intent("2"));
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
        tokio::spawn(async move { call(&state2, &auth, "POST", &path2, intent("1")).await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async{loop{let wait:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();if wait{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
    sqlx::query("UPDATE wordlists SET state='withdrawn',revision=revision+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(waiting.await.unwrap().0, StatusCode::NOT_FOUND);
    assert_eq!(balance(&pool, payer.subject).await, 18);
    assert_eq!(balance(&pool, author.subject).await, 2);
    assert_eq!(
        call(&state, &payer, "GET", &receipt, Value::Null).await.0,
        StatusCode::OK
    );
    let pending = apply(&state, &payer, "18").await;
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
