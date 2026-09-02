//! Surface match snapshot 的 Redis 分页契约。
//!
//! 这里刻意打真 Redis：`SurfaceSnapshotStore` 的游标推进有两套实现——`mod tests` 里那套纯函数
//! `advance_bundle`，和生产真正执行的 Lua 脚本。只在 Lua 侧成立的 bug（回写快照时把 JSON 里的
//! 空数组编码成空对象，导致下一页反序列化失败）纯函数测试一个都测不到。

use serde_json::{Value, json};
use tsz_rust::lexicon::{
    dto::{
        LexiconSurfaceMatchV2, MatchedEntryContextV2, SurfaceConfirmationReasonV2,
        SurfaceMatchPageAny, SurfaceMatchPageV2, SurfacePolicyNameV2,
    },
    surface_policy::SurfacePolicyStore,
    surface_snapshot::{
        CreateSurfaceSnapshot, ExpectedSurfaceConfirmation, SurfaceConfirmationBinding,
        SurfaceConsumptionCommand, SurfaceSnapshotStore, surface_owner_bundle_digest,
    },
};
use uuid::Uuid;

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

fn match_item(index: usize) -> LexiconSurfaceMatchV2 {
    serde_json::from_value(json!({
        "match_id": format!("match-{index:02}"),
        "match_category": "exact_headword",
        "severity": "warning",
        "attention_level": "high",
        "can_continue": true,
        "confirmation_reasons": ["unacknowledged_surface_matches"],
        "candidate": {
            "candidate_type": "headword",
            "candidate_ref": "candidate:common",
            "surface": "workspace",
            "normalized_surface": "workspace",
            "dialect": "common",
            "entry_kind": "word"
        },
        "existing": {
            "word_id": Uuid::from_u128(0x1000 + index as u128),
            "headword": "workspace",
            "kind": "word",
            "status": "draft",
            "source": {
                "source_kind": "headword",
                "source_id": format!("source-{index:02}"),
                "content_scope": "draft",
                "surface": "workspace",
                "dialect": "common"
            }
        }
    }))
    .expect("fixture match 应能反序列化")
}

/// `inbound_relations.previews` 在真实数据里常年是空数组，而它正是踩中 Lua 空数组编码的字段：
/// fixture 必须保留这个空数组，否则这条测试就测不到它本来要守的东西。
fn matched_context(index: usize) -> MatchedEntryContextV2 {
    serde_json::from_value(json!({
        "word_id": Uuid::from_u128(0x1000 + index as u128),
        "pos_labels": ["noun"],
        "gloss_previews": [format!("gloss-{index}")],
        "updated_at": "2026-09-02T00:00:00Z",
        "inbound_relations": {
            "total": 0,
            "by_type": {"synonym": 0, "antonym": 0, "derivative": 0},
            "previews": [],
            "truncated": false
        }
    }))
    .expect("fixture context 应能反序列化")
}

fn owner_bundle() -> Value {
    json!({
        "detection_id": Uuid::from_u128(0xd37ec710),
        "canonical_detection": {"headword": "workspace"}
    })
}

fn binding(actor_id: Uuid, policy_epoch: u64) -> SurfaceConfirmationBinding {
    SurfaceConfirmationBinding {
        actor_id,
        command: SurfaceConsumptionCommand::CreateEntry,
        owner_context: Uuid::from_u128(0xd37ec710).to_string(),
        base_revision: None,
        canonical_content_digest: "content-v1".to_owned(),
        owner_evidence_digest: surface_owner_bundle_digest(&owner_bundle())
            .expect("owner bundle digest"),
        normalization_version: 1,
        // 这条策略的默认值就是 enabled，懒初始化即可拿到能签令牌的快照，不必再动数据库。
        policy_name: SurfacePolicyNameV2::SurfaceWarningAcknowledgement,
        policy_epoch,
    }
}

fn page_of(page: &SurfaceMatchPageAny) -> &SurfaceMatchPageV2 {
    match page {
        SurfaceMatchPageAny::V2(page) => page,
        SurfaceMatchPageAny::V3(_) => panic!("V2 owner bundle 不该投影成 V3 页"),
    }
}

fn next_cursor(page: &SurfaceMatchPageV2) -> String {
    match page {
        SurfaceMatchPageV2::EnabledNext(page) => page.next_cursor.clone(),
        _ => panic!("expected a non-terminal page, got {page:?}"),
    }
}

fn match_ids(page: &SurfaceMatchPageV2) -> Vec<String> {
    let items = match page {
        SurfaceMatchPageV2::EnabledNext(page) => &page.page.items,
        SurfaceMatchPageV2::EnabledTerminal(page) => &page.page.items,
        SurfaceMatchPageV2::TemporarilyDisabled(page) => &page.page.items,
    };
    items.iter().map(|item| item.match_id.clone()).collect()
}

#[tokio::test]
async fn paging_to_the_terminal_page_signs_a_token_without_corrupting_the_snapshot() {
    let redis = tsz_rust::platform::connect_redis(&test_redis_url())
        .await
        .expect("测试 Redis 连接池应能创建");
    let prefix = format!("test:surface-policy:{}:", Uuid::now_v7());
    let policy = SurfacePolicyStore::with_prefix_for_test(redis.clone(), prefix.clone())
        .policy(SurfacePolicyNameV2::SurfaceWarningAcknowledgement)
        .await
        .expect("策略应能懒初始化");
    assert!(policy.enabled, "该策略默认就该是 enabled");

    let store = SurfaceSnapshotStore::with_policy_prefix_for_test(redis, prefix);
    let actor = Uuid::now_v7();
    let reasons = vec![SurfaceConfirmationReasonV2::UnacknowledgedSurfaceMatches];
    let created = store
        .create(CreateSurfaceSnapshot {
            binding: binding(actor, policy.epoch),
            policy_enabled: true,
            policy_block_code: None,
            items: (0..3).map(match_item).collect(),
            matched_entry_contexts: (0..3).map(matched_context).collect(),
            confirmation_reasons: reasons,
            owner_bundle: owner_bundle(),
            // 每页一条：三条候选必须翻两次才到末页，正好走满 Lua 的推进分支。
            page_size: 1,
        })
        .await
        .expect("快照应能创建");

    let mut seen = match_ids(&created.page);
    let second = store
        .page(actor, created.snapshot_id, &next_cursor(&created.page))
        .await
        .expect("第二页不该因为首页回写把快照写坏而失败");
    seen.extend(match_ids(page_of(&second)));
    let third = store
        .page(actor, created.snapshot_id, &next_cursor(page_of(&second)))
        .await
        .expect("末页不该因为第二页回写把快照写坏而失败");
    seen.extend(match_ids(page_of(&third)));

    seen.sort();
    assert_eq!(
        seen,
        vec!["match-00", "match-01", "match-02"],
        "三页合起来必须不重不漏地覆盖全部候选"
    );

    let token = match page_of(&third) {
        SurfaceMatchPageV2::EnabledTerminal(page) => page.surface_confirmation_token.clone(),
        other => panic!("末页必须签发确认令牌，实际拿到 {other:?}"),
    };

    let verified = store
        .verify(
            &token,
            &ExpectedSurfaceConfirmation {
                binding: binding(actor, policy.epoch),
                current_policy: policy,
            },
        )
        .await
        .expect("末页签发的令牌必须能验证");
    assert_eq!(verified.match_ids.len(), 3);
    assert_eq!(
        verified.owner_bundle,
        owner_bundle(),
        "owner bundle 必须原样穿过分页回写"
    );

    store
        .remove_verified(&verified)
        .await
        .expect("消费后清理不该失败");
}
