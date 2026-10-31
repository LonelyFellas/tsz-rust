use super::*;
use tsz_rust::speech::publication_audio::{
    AudioRequirement, RequirementIssue,
    review::{Owner, Reviewer, confirm_in, confirmed_in, inspect_in, probe_assets},
};

async fn setup(pool: &PgPool) -> (Reviewer, Uuid, AudioRequirement, Arc<dyn ObjectStore>) {
    let actor = actor(pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.publish',$1)").bind(actor).execute(pool).await.unwrap();
    let entry = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word','{}',$2,$2)").bind(entry).bind(actor).execute(pool).await.unwrap();
    let storage = store();
    let service = GeneratedAudioService::new(
        pool.clone(),
        Some(Arc::new(Provider {
            calls: AtomicUsize::new(0),
            fail_first: false,
        })),
        Some(storage.clone()),
    );
    let request = input(Uuid::now_v7(), Uuid::now_v7(), "Hello");
    let mut requirement = AudioRequirement {
        target_id: Uuid::now_v7(),
        content_id: None,
        source_path: "/content".into(),
        source_dialect: None,
        default_for: vec![],
        setting: Some(request.setting.clone()),
        source: Some(request.source.clone()),
        profile: None,
        asset_id: None,
        issue: None,
    };
    let asset = service.generate(actor, 0, request).await.unwrap().asset;
    requirement.asset_id = Some(asset.id);
    requirement.setting.as_mut().unwrap().audio_asset_id = Some(asset.id);
    sqlx::query("INSERT INTO lexicon.v3_audio_asset_references(asset_id,entry_id,scope,variant_id) VALUES($1,$2,'draft',$3)")
        .bind(asset.id).bind(entry).bind(requirement.target_id).execute(pool).await.unwrap();
    (
        Reviewer {
            actor_id: actor,
            security_version: 0,
        },
        entry,
        requirement,
        storage,
    )
}

#[sqlx::test]
async fn exact_batch_confirmation_is_persisted_and_requires_fresh_locked_inspection(pool: PgPool) {
    let (reviewer, entry, requirement, storage) = setup(&pool).await;
    let probes = probe_assets(
        &pool,
        Owner::Entry(entry),
        std::slice::from_ref(&requirement),
        storage.as_ref(),
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement.clone()],
        None,
        &probes,
    )
    .await
    .unwrap();
    assert!(manifest.ready());
    assert!(
        confirmed_in(&mut tx, reviewer.actor_id, 0, &manifest)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        confirm_in(&mut tx, reviewer.actor_id, 0, &manifest, "wrong")
            .await
            .is_err()
    );
    assert!(
        confirm_in(
            &mut tx,
            Uuid::now_v7(),
            0,
            &manifest,
            manifest.manifest_hash()
        )
        .await
        .is_err()
    );
    let receipt = confirm_in(
        &mut tx,
        reviewer.actor_id,
        0,
        &manifest,
        manifest.manifest_hash(),
    )
    .await
    .unwrap();
    assert_eq!(
        confirm_in(
            &mut tx,
            reviewer.actor_id,
            0,
            &manifest,
            manifest.manifest_hash()
        )
        .await
        .unwrap(),
        receipt
    );
    tx.commit().await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    assert!(
        confirm_in(
            &mut tx,
            reviewer.actor_id,
            0,
            &manifest,
            manifest.manifest_hash()
        )
        .await
        .is_err(),
        "old transaction's verified object must not be reusable"
    );
    let refreshed = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement],
        None,
        &probes,
    )
    .await
    .unwrap();
    assert_eq!(
        confirmed_in(&mut tx, reviewer.actor_id, 0, &refreshed)
            .await
            .unwrap(),
        Some(receipt)
    );
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM speech.publication_audio_reviews")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn missing_objects_and_changed_effects_cannot_be_confirmed(pool: PgPool) {
    let (reviewer, entry, requirement, storage) = setup(&pool).await;
    let probes = probe_assets(
        &pool,
        Owner::Entry(entry),
        std::slice::from_ref(&requirement),
        storage.as_ref(),
    )
    .await
    .unwrap();
    let mut changed = requirement.clone();
    changed.setting.as_mut().unwrap().rate_percent = -20;
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![changed],
        None,
        &probes,
    )
    .await
    .unwrap();
    assert!(!manifest.ready());
    assert!(matches!(
        manifest.items()[0].issue,
        Some(RequirementIssue::EffectMismatch)
    ));
    assert!(
        confirm_in(
            &mut tx,
            reviewer.actor_id,
            0,
            &manifest,
            manifest.manifest_hash()
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let key: String = sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id=$1")
        .bind(requirement.asset_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    storage
        .delete(&ObjectKey::parse(key).unwrap())
        .await
        .unwrap();
    let probes = probe_assets(
        &pool,
        Owner::Entry(entry),
        std::slice::from_ref(&requirement),
        storage.as_ref(),
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement],
        None,
        &probes,
    )
    .await
    .unwrap();
    assert!(matches!(
        manifest.items()[0].issue,
        Some(RequirementIssue::ObjectUnavailable)
    ));
    assert!(!manifest.ready());
    assert!(
        confirm_in(
            &mut tx,
            reviewer.actor_id,
            0,
            &manifest,
            manifest.manifest_hash()
        )
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn receipts_do_not_survive_revision_or_security_version_changes(pool: PgPool) {
    let (reviewer, entry, requirement, storage) = setup(&pool).await;
    let probes = probe_assets(
        &pool,
        Owner::Entry(entry),
        std::slice::from_ref(&requirement),
        storage.as_ref(),
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement.clone()],
        None,
        &probes,
    )
    .await
    .unwrap();
    confirm_in(
        &mut tx,
        reviewer.actor_id,
        0,
        &manifest,
        manifest.manifest_hash(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE admins SET security_version=1 WHERE id=$1")
        .bind(reviewer.actor_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            vec![requirement.clone()],
            None,
            &probes
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let reviewer = Reviewer {
        security_version: 1,
        ..reviewer
    };
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement.clone()],
        None,
        &probes,
    )
    .await
    .unwrap();
    assert!(
        confirmed_in(&mut tx, reviewer.actor_id, 1, &manifest)
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET revision=2 WHERE id=$1")
        .bind(entry)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            vec![requirement],
            None,
            &probes
        )
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn review_migration_roundtrip_preserves_audit_receipts(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../../migrations/20261029010000_publication_audio_reviews.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261029010000_publication_audio_reviews.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let (reviewer, entry, requirement, storage) = setup(&pool).await;
    let probes = probe_assets(
        &pool,
        Owner::Entry(entry),
        std::slice::from_ref(&requirement),
        storage.as_ref(),
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let manifest = inspect_in(
        &mut tx,
        reviewer,
        Owner::Entry(entry),
        1,
        vec![requirement],
        None,
        &probes,
    )
    .await
    .unwrap();
    let receipt = confirm_in(
        &mut tx,
        reviewer.actor_id,
        0,
        &manifest,
        manifest.manifest_hash(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261029010000_publication_audio_reviews.down.sql"
        ))
        .execute(&pool)
        .await
        .is_err()
    );
    let retained: Uuid =
        sqlx::query_scalar("SELECT id FROM speech.publication_audio_reviews WHERE id=$1")
            .bind(receipt)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retained, receipt);
}

#[sqlx::test]
async fn inherited_first_choice_changes_invalidate_review_but_names_and_later_choices_do_not(
    pool: PgPool,
) {
    let (reviewer, entry, mut first, storage) = setup(&pool).await;
    let first_setting = first.setting.as_ref().unwrap().clone();
    let mut second_setting = first_setting.clone();
    second_setting.version_id = Some(Uuid::now_v7());
    second_setting.rate_percent = 10;
    let profile: tsz_rust::lexicon::dto::VoiceProfileV3 = serde_json::from_value(json!({
        "voices":[first_setting],"dubbing":{"mode":"inherit","area":"word"}
    }))
    .unwrap();
    first.source_path = "/content/uk".into();

    first.profile = Some(Arc::new(profile.clone()));
    let recommendation = |setting: &tsz_rust::lexicon::dto::VoiceSettingV3, name: &str| {
        json!({
            "id":setting.version_id,"name":name,"voice_alias":"sonia","selected":true,
            "parameters":{"rate_percent":setting.rate_percent,"pitch_semitones":0,"style":null}
        })
    };
    let a = recommendation(first.setting.as_ref().unwrap(), "A");
    let b = recommendation(&second_setting, "B");
    let groups = |items: serde_json::Value| json!([{"id":Uuid::now_v7(),"name":"word","areas":["word"],"uk":items,"us":[],"revision":1,"is_global":false}]);
    let items = vec![first];
    let probes = probe_assets(&pool, Owner::Entry(entry), &items, storage.as_ref())
        .await
        .unwrap();
    let hash;
    {
        let selected = json!([a.clone(), b.clone()]);
        sqlx::query("UPDATE speech.dubbing_groups SET groups=$1 WHERE singleton")
            .bind(groups(selected))
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let manifest = inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            items.clone(),
            None,
            &probes,
        )
        .await
        .unwrap();
        assert!(manifest.ready(), "{:?}", manifest.items());
        hash = manifest.manifest_hash().to_owned();
        confirm_in(
            &mut tx,
            reviewer.actor_id,
            0,
            &manifest,
            manifest.manifest_hash(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let mut renamed = a.clone();
    renamed["name"] = json!("renamed");
    let mut added = a.clone();
    added["id"] = json!(Uuid::now_v7());
    for (selected, ready) in [
        (json!([renamed, b.clone()]), true),
        (json!([b.clone(), a.clone()]), false),
        (json!([a.clone(), b.clone(), added.clone()]), true),
        (json!([added, a, b]), false),
    ] {
        sqlx::query("UPDATE speech.dubbing_groups SET groups=$1 WHERE singleton")
            .bind(groups(selected))
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let manifest = inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            items.clone(),
            None,
            &probes,
        )
        .await
        .unwrap();
        assert_eq!(manifest.ready(), ready);
        if ready {
            assert_eq!(manifest.manifest_hash(), hash);
        } else {
            assert!(
                confirmed_in(&mut tx, reviewer.actor_id, 0, &manifest)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        tx.rollback().await.unwrap();
    }
}

#[sqlx::test]
async fn inherited_grammar_profiles_use_the_owner_dialect_instead_of_array_paths(pool: PgPool) {
    use tsz_rust::lexicon::dto::Dialect;
    let (reviewer, entry, uk, storage) = setup(&pool).await;
    sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,'jenny','azure','en-US-JennyNeural','en-US','female','[]','v1')")
        .bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    let service = GeneratedAudioService::new(
        pool.clone(),
        Some(Arc::new(Provider {
            calls: AtomicUsize::new(0),
            fail_first: false,
        })),
        Some(storage.clone()),
    );
    let mut request = input(Uuid::now_v7(), Uuid::now_v7(), "Hello");
    request.setting.voice_id = "jenny".into();
    request.setting.locale = Some(tsz_rust::speech::locale::EnglishLocale::Us);
    let mut us = uk.clone();
    us.target_id = Uuid::now_v7();
    us.setting = Some(request.setting.clone());
    us.asset_id = Some(
        service
            .generate(reviewer.actor_id, 0, request)
            .await
            .unwrap()
            .asset
            .id,
    );
    us.setting.as_mut().unwrap().audio_asset_id = us.asset_id;
    sqlx::query("INSERT INTO lexicon.v3_audio_asset_references(asset_id,entry_id,scope,variant_id) VALUES($1,$2,'draft',$3)")
        .bind(us.asset_id).bind(entry).bind(us.target_id).execute(&pool).await.unwrap();
    let rec = |item: &AudioRequirement| json!({"id":item.setting.as_ref().unwrap().version_id,"name":"chosen","voice_alias":item.setting.as_ref().unwrap().voice_id,"selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}});
    sqlx::query("UPDATE speech.dubbing_groups SET groups=$1 WHERE singleton")
        .bind(json!([{"id":Uuid::now_v7(),"name":"grammar","areas":["grammar"],"uk":[rec(&uk)],"us":[rec(&us)],"revision":1,"is_global":false}])).execute(&pool).await.unwrap();
    for (mut item, dialect) in [(uk, Dialect::Uk), (us, Dialect::Us)] {
        item.source_path = "/meanings/pos/0/grammar_structures/0/variants/0".into();
        item.source_dialect = Some(dialect);
        item.profile = Some(Arc::new(
            serde_json::from_value(
                json!({"voices":[item.setting],"dubbing":{"mode":"inherit","area":"grammar"}}),
            )
            .unwrap(),
        ));
        let probes = probe_assets(
            &pool,
            Owner::Entry(entry),
            std::slice::from_ref(&item),
            storage.as_ref(),
        )
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let manifest = inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            vec![item.clone()],
            None,
            &probes,
        )
        .await
        .unwrap();
        assert!(manifest.ready(), "{dialect:?}: {:?}", manifest.items());
        tx.rollback().await.unwrap();
        item.source_dialect = Some(Dialect::Common);
        let mut tx = pool.begin().await.unwrap();
        let common = inspect_in(
            &mut tx,
            reviewer,
            Owner::Entry(entry),
            1,
            vec![item],
            None,
            &probes,
        )
        .await
        .unwrap();
        assert!(
            !common.ready(),
            "common still requires both effective accent selections"
        );
        tx.rollback().await.unwrap();
    }
}
