// Real-router/PostgreSQL regressions for the tshb-test publication acceptance scenarios.
// Included by wordlists_review: CI's lexicon-other shard discovers and verifies every case.
// Run with DATABASE_URL and TEST_REDIS_URL pointing to disposable local services:
// cargo test --locked --all-features --test wordlists_review publication_regressions
use super::*;
use std::{future::Future, time::Duration};
use tsz_rust::{auth::extract::AuthUser, state::AppState};

type Response = (StatusCode, Value);

struct Fixture {
    state: AppState,
    owner: AuthUser,
    reader: AuthUser,
    id: Uuid,
    entry: Uuid,
    admins: [Uuid; 2],
}

impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        let (state, owner) = setup_bound(pool).await;
        let (_, reader) = setup_bound(pool).await;
        let entry = entry(pool, "publication regression").await;
        let admins = [admin(pool).await, admin(pool).await];
        for reviewer in admins {
            sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
                .bind(reviewer)
                .execute(pool)
                .await
                .unwrap();
            for permission in ["wordlists.access", "wordlists.review"] {
                sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)")
                    .bind(reviewer).bind(permission).execute(pool).await.unwrap();
            }
        }
        let created = call(
            &state,
            &owner,
            "POST",
            "/api/v1/me/wordlists",
            create_body(&[entry]),
        )
        .await;
        ok(&created);
        Self {
            state,
            owner,
            reader,
            id: created.1["id"].as_str().unwrap().parse().unwrap(),
            entry,
            admins,
        }
    }

    async fn author(&self, method: &str, suffix: &str, body: Value) -> Response {
        call(
            &self.state,
            &self.owner,
            method,
            &format!("/api/v1/me/wordlists/{}{suffix}", self.id),
            body,
        )
        .await
    }

    async fn submit(&self, revision: i64, key: Uuid, route: &str) -> Response {
        self.author(
            "POST",
            route,
            json!({"expected_revision":revision,"idempotency_key":key}),
        )
        .await
    }

    async fn decision(&self, reviewer: Uuid, request: &Value, approve: bool) -> Response {
        admin_call(&self.state, reviewer, "POST",
            &format!("/api/v1/admin/wordlists/{}/review-requests/{}/decision", self.id, request["id"].as_str().unwrap()),
            json!({"expected_revision":request["submitted_revision"],"approve":approve,"reason":if approve {None} else {Some("请补充说明")} })).await
    }

    async fn withdraw(&self, revision: i64) -> Response {
        self.author("POST", "/withdraw", json!({"expected_revision":revision}))
            .await
    }

    async fn edit(&self, revision: i64, name: &str) {
        ok(&self.author("PUT", "", json!({"expected_revision":revision,"content":{"name":name,"entry_ids":[self.entry],"author_notes":[{"entry_id":self.entry,"author_note":name}]},"note_updates":[]})).await);
    }

    async fn public(&self, expected: StatusCode) {
        for suffix in ["", "/items"] {
            let response = call(
                &self.state,
                &self.reader,
                "GET",
                &format!("/api/v1/wordlists/{}{suffix}", self.id),
                Value::Null,
            )
            .await;
            assert_eq!(response.0, expected, "{response:?}");
            assert!(!response.1.to_string().contains("私密备注"));
        }
    }

    // Re-read persisted state, all request snapshots and audits: stale writes must change none of them.
    async fn snapshot(&self, pool: &PgPool) -> Value {
        sqlx::query_scalar("SELECT jsonb_build_object('list',to_jsonb(w),'requests',COALESCE((SELECT jsonb_agg(to_jsonb(r) ORDER BY r.created_at,r.id) FROM wordlist_review_requests r WHERE r.wordlist_id=w.id),'[]'::jsonb),'audits',COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY a.occurred_at,a.id) FROM audit.admin_actions a WHERE a.resource_type='wordlist' AND a.resource_id=w.id),'[]'::jsonb)) FROM wordlists w WHERE w.id=$1")
            .bind(self.id).fetch_one(pool).await.unwrap()
    }

    async fn assert_state(
        &self,
        pool: &PgPool,
        state: &str,
        revision: i64,
        requests: usize,
        audits: usize,
    ) -> Value {
        let snapshot = self.snapshot(pool).await;
        assert_eq!(snapshot["list"]["state"], state);
        assert_eq!(snapshot["list"]["revision"], revision);
        assert_eq!(snapshot["requests"].as_array().unwrap().len(), requests);
        assert_eq!(snapshot["audits"].as_array().unwrap().len(), audits);
        let current = self.author("GET", "", Value::Null).await;
        ok(&current);
        assert_eq!(current.1["state"], state);
        assert_eq!(current.1["revision"], revision);
        self.public(if state == "published" {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        })
        .await;
        snapshot
    }
}

fn ok(response: &Response) {
    assert_eq!(response.0, StatusCode::OK, "{response:?}");
}

fn conflict(response: &Response) {
    assert_eq!(response.0, StatusCode::CONFLICT, "{response:?}");
    assert_eq!(response.1["code"], "word_list_conflict", "{response:?}");
}

#[sqlx::test]
async fn rejected_and_withdrawn_publications_require_fresh_manual_review(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let historical = Fixture::new(&pool).await;
    // Model a legacy auto-approved publication: no human reviewer or decision audit.
    let legacy = historical
        .submit(1, Uuid::now_v7(), "/review-requests")
        .await;
    ok(&legacy);
    sqlx::query(
        "UPDATE wordlists SET state='published',review_state='approved',revision=3 WHERE id=$1",
    )
    .bind(historical.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE wordlist_review_requests SET state='approved',decided_at=clock_timestamp() WHERE wordlist_id=$1")
        .bind(historical.id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE wordlist_items i SET approved_lifecycle_revision=e.lifecycle_revision,approved_archive_generation=e.wordlist_archive_generation FROM lexicon.entries e WHERE i.wordlist_id=$1 AND e.id=i.entry_id")
        .bind(historical.id).execute(&pool).await.unwrap();
    let baseline = historical.assert_state(&pool, "published", 3, 1, 0).await;
    let first = f.submit(1, Uuid::now_v7(), "/review-requests").await;
    ok(&first);
    ok(&f.decision(f.admins[0], &first.1, false).await);
    let rejected = f.assert_state(&pool, "rejected", 3, 1, 1).await;
    assert_eq!(rejected["requests"][0]["reason"], "请补充说明");
    f.edit(3, "修改后重提").await;
    let second_key = Uuid::now_v7();
    let second = f.submit(4, second_key, "/publish").await;
    ok(&second);
    assert_ne!(first.1["id"], second.1["id"]);
    let pending = f.assert_state(&pool, "pending", 5, 2, 1).await;
    historical.public(StatusCode::OK).await;
    assert_eq!(historical.snapshot(&pool).await, baseline);
    assert_eq!(pending["requests"][0], rejected["requests"][0]);
    assert_eq!(pending["requests"][1]["name"], "修改后重提");
    assert_eq!(
        pending["requests"][1]["author_notes"][f.entry.to_string()],
        "修改后重提"
    );
    ok(&f.withdraw(5).await);
    let cancelled = f.assert_state(&pool, "draft", 6, 2, 1).await;
    assert_eq!(cancelled["requests"][1]["state"], "cancelled");
    conflict(&f.decision(f.admins[1], &second.1, true).await);
    assert_eq!(f.snapshot(&pool).await, cancelled);
    // Retrying the withdrawn operation returns its cancelled request, never a new pending one.
    let retry = f.submit(4, second_key, "/review-requests").await;
    ok(&retry);
    assert_eq!(retry.1["id"], second.1["id"]);
    assert_eq!(retry.1["state"], "cancelled");
    assert_eq!(f.snapshot(&pool).await, cancelled);
    let third = f.submit(6, Uuid::now_v7(), "/review-requests").await;
    ok(&third);
    assert_ne!(third.1["id"], second.1["id"]);
    let pending = f.assert_state(&pool, "pending", 7, 3, 1).await;
    conflict(&f.decision(f.admins[1], &second.1, true).await);
    assert_eq!(f.snapshot(&pool).await, pending);
    ok(&f.decision(f.admins[0], &third.1, true).await);
    let published = f.assert_state(&pool, "published", 8, 3, 2).await;
    ok(&f.withdraw(8).await);
    f.assert_state(&pool, "draft", 9, 3, 2).await;
    f.edit(9, "公开后撤回修改").await;
    let fourth = f.submit(10, Uuid::now_v7(), "/publish").await;
    ok(&fourth);
    assert_ne!(fourth.1["id"], third.1["id"]);
    let pending = f.assert_state(&pool, "pending", 11, 4, 2).await;
    assert_eq!(pending["requests"][2], published["requests"][2]);
    assert_eq!(pending["requests"][3]["name"], "公开后撤回修改");
    for old in [&second.1, &third.1] {
        conflict(&f.decision(f.admins[1], old, true).await);
        assert_eq!(f.snapshot(&pool).await, pending);
    }
    ok(&f.decision(f.admins[1], &fourth.1, true).await);
    f.assert_state(&pool, "published", 12, 4, 3).await;
    historical.public(StatusCode::OK).await;
    assert_eq!(historical.snapshot(&pool).await, baseline);
}

// Observe the actual wait graph, including a second waiter blocked behind the first waiter.
// Sleeping for a fixed duration would allow a nominally concurrent test to run serially.
async fn wait_for_requests(pool: &PgPool, holder: i32, count: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("WITH RECURSIVE waiters AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT a.pid FROM pg_stat_activity a JOIN waiters w ON w.pid=ANY(pg_blocking_pids(a.pid)) WHERE a.datname=current_database()) SELECT count(*) FROM waiters")
                .bind(holder).fetch_one(pool).await.unwrap();
            if waiting >= count { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("requests must overlap on the fixture's database lock");
}

async fn race(
    pool: &PgPool,
    f: &Fixture,
    ordered: bool,
    first: impl Future<Output = Response>,
    second: impl Future<Output = Response>,
) -> (Response, Response) {
    let mut blocker = pool.begin().await.unwrap();
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR SHARE")
        .bind(f.owner.subject)
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(f.id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let (_, a, b) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            async {
                wait_for_requests(pool, holder, 2).await;
                blocker.commit().await.unwrap();
            },
            first,
            async {
                if ordered {
                    wait_for_requests(pool, holder, 1).await;
                }
                second.await
            }
        )
    })
    .await
    .expect("concurrent requests must finish after releasing the fixture lock");
    (a, b)
}

#[sqlx::test]
async fn concurrent_duplicate_submissions_create_one_request(pool: PgPool) {
    for same_key in [true, false] {
        let f = Fixture::new(&pool).await;
        let key = Uuid::now_v7();
        let other_key = if same_key { key } else { Uuid::now_v7() };
        let (a, b) = race(
            &pool,
            &f,
            false,
            f.submit(1, key, "/publish"),
            f.submit(1, other_key, "/review-requests"),
        )
        .await;
        if same_key {
            ok(&a);
            ok(&b);
            assert_eq!(a, b);
        } else if a.0 == StatusCode::OK {
            ok(&a);
            conflict(&b);
        } else {
            conflict(&a);
            ok(&b);
        }
        f.assert_state(&pool, "pending", 2, 1, 0).await;
    }
}

#[sqlx::test]
async fn concurrent_admin_decisions_persist_only_the_winning_decision(pool: PgPool) {
    for second_approves in [true, false] {
        let f = Fixture::new(&pool).await;
        let pending = f.submit(1, Uuid::now_v7(), "/review-requests").await;
        ok(&pending);
        let (a, b) = race(
            &pool,
            &f,
            false,
            f.decision(f.admins[0], &pending.1, true),
            f.decision(f.admins[1], &pending.1, second_approves),
        )
        .await;
        let (winner, approve) = if a.0 == StatusCode::OK {
            ok(&a);
            conflict(&b);
            (f.admins[0], true)
        } else {
            conflict(&a);
            ok(&b);
            (f.admins[1], second_approves)
        };
        let snapshot = f
            .assert_state(
                &pool,
                if approve { "published" } else { "rejected" },
                3,
                1,
                1,
            )
            .await;
        assert_eq!(
            snapshot["requests"][0]["state"],
            if approve { "approved" } else { "rejected" }
        );
        assert_eq!(snapshot["requests"][0]["reviewer_id"], winner.to_string());
        assert_eq!(snapshot["audits"][0]["actor_admin_id"], winner.to_string());
        assert_eq!(
            snapshot["audits"][0]["action"],
            if approve {
                "wordlist.approve"
            } else {
                "wordlist.reject"
            }
        );
    }
}

#[sqlx::test]
async fn concurrent_review_and_withdrawal_respect_both_lock_orders(pool: PgPool) {
    for withdraw_first in [true, false] {
        let f = Fixture::new(&pool).await;
        let pending = f.submit(1, Uuid::now_v7(), "/review-requests").await;
        ok(&pending);
        if withdraw_first {
            let (withdraw, decision) = race(
                &pool,
                &f,
                true,
                f.withdraw(2),
                f.decision(f.admins[0], &pending.1, true),
            )
            .await;
            ok(&withdraw);
            conflict(&decision);
            let snapshot = f.assert_state(&pool, "draft", 3, 1, 0).await;
            assert_eq!(snapshot["requests"][0]["state"], "cancelled");
        } else {
            let (decision, withdraw) = race(
                &pool,
                &f,
                true,
                f.decision(f.admins[0], &pending.1, true),
                f.withdraw(2),
            )
            .await;
            ok(&decision);
            conflict(&withdraw);
            f.assert_state(&pool, "published", 3, 1, 1).await;
            ok(&f.withdraw(3).await);
            let snapshot = f.assert_state(&pool, "draft", 4, 1, 1).await;
            assert_eq!(snapshot["requests"][0]["state"], "approved");
        }
        let withdrawn = f.snapshot(&pool).await;
        conflict(&f.decision(f.admins[1], &pending.1, true).await);
        assert_eq!(f.snapshot(&pool).await, withdrawn);
    }
}
