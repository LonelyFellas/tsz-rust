use sqlx::PgPool;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    error::ErrorCode,
    speech::library::{
        VoiceLibraryRepository,
        dto::{
            DubbingGroup, DubbingRecommendation, SaveDubbingGroupsRequest,
            SaveVoiceDescriptionRequest, SaveVoiceSettingsRequest, VoiceParameters,
        },
    },
};
use uuid::Uuid;

#[sqlx::test]
async fn groups_enforce_scope_locale_and_atomic_revision(pool: PgPool) {
    voice(&pool).await;
    sqlx::query(r#"INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version)
        VALUES($1,'en-gb-sonia','azure','en-GB-SoniaNeural','en-GB','female','[]','v1')"#)
        .bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    let actor = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    let recommendation = |alias: &str| DubbingRecommendation {
        id: Uuid::now_v7(),
        name: "清晰慢读".into(),
        voice_alias: alias.into(),
        parameters: VoiceParameters {
            rate_percent: -25,
            pitch_semitones: 0,
            volume_percent: Some(70),
            style: None,
        },
    };
    let group = DubbingGroup {
        id: Uuid::now_v7(),
        name: "词形与发音".into(),
        areas: vec!["word".into(), "azure".into()],
        uk: vec![recommendation("en-gb-sonia")],
        us: vec![recommendation("en-us-jenny")],
    };
    assert_eq!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![group.clone()],
                    base_revision: 0
                }
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    repository
        .save_groups(
            actor,
            0,
            SaveDubbingGroupsRequest {
                groups: vec![group.clone()],
                base_revision: 0,
            },
        )
        .await
        .unwrap();
    let mut invalid = group.clone();
    invalid.id = Uuid::now_v7();
    invalid.name = "重复板块".into();
    assert!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![group.clone(), invalid],
                    base_revision: 1
                }
            )
            .await
            .is_err()
    );
    let mut invalid = group.clone();
    invalid.uk[0].voice_alias = "en-us-jenny".into();
    assert!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![invalid],
                    base_revision: 1
                }
            )
            .await
            .is_err()
    );
    let read = repository.groups(actor).await.unwrap();
    assert_eq!(read.revision, 1);
    assert_eq!(read.groups[0].uk[0].voice_alias, "en-gb-sonia");
    assert_eq!(read.groups[0].uk[0].parameters.volume_percent, Some(70));
    assert_eq!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![],
                    base_revision: 0
                }
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::RevisionConflict
    );
    repository
        .save_groups(
            actor,
            0,
            SaveDubbingGroupsRequest {
                groups: vec![],
                base_revision: 1,
            },
        )
        .await
        .unwrap();
    assert!(repository.groups(actor).await.unwrap().groups.is_empty());
}

#[sqlx::test]
async fn global_dubbing_only_can_manage_library_but_revoked_sessions_cannot_write(pool: PgPool) {
    voice(&pool).await;
    let actor = admin(&pool).await;
    sqlx::query("UPDATE admin_permission_grants SET permission_key='speech.global_dubbing' WHERE admin_id=$1").bind(actor).execute(&pool).await.unwrap();
    let repository = VoiceLibraryRepository::new(pool.clone());
    assert_eq!(repository.list(actor).await.unwrap().items.len(), 1);
    repository.groups(actor).await.unwrap();
    repository
        .save_settings(actor, "en-us-jenny", 0, settings(0, "valid"))
        .await
        .unwrap();
    repository
        .save_description(
            actor,
            "en-us-jenny",
            0,
            SaveVoiceDescriptionRequest {
                description_zh: "介绍".into(),
                base_revision: 0,
            },
        )
        .await
        .unwrap();
    repository
        .save_groups(
            actor,
            0,
            SaveDubbingGroupsRequest {
                groups: vec![],
                base_revision: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE admins SET security_version=security_version+1 WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .save_settings(actor, "en-us-jenny", 0, settings(1, "stale"))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidToken
    );
    assert_eq!(
        repository
            .save_description(
                actor,
                "en-us-jenny",
                0,
                SaveVoiceDescriptionRequest {
                    description_zh: "stale".into(),
                    base_revision: 1
                }
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidToken
    );
    assert_eq!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![],
                    base_revision: 1
                }
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidToken
    );
    assert_eq!(
        repository.list(actor).await.unwrap().items[0].notes,
        "valid"
    );
}

#[sqlx::test]
async fn nul_text_is_rejected_before_database_writes(pool: PgPool) {
    voice(&pool).await;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let repository = VoiceLibraryRepository::new(pool);
    assert_eq!(
        repository
            .save_settings(actor, "en-us-jenny", 0, settings(0, "bad\0text"))
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    assert_eq!(
        repository
            .save_description(
                actor,
                "en-us-jenny",
                0,
                SaveVoiceDescriptionRequest {
                    description_zh: "bad\0text".into(),
                    base_revision: 0
                }
            )
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    let rec = DubbingRecommendation {
        id: Uuid::now_v7(),
        name: "bad\0name".into(),
        voice_alias: "en-us-jenny".into(),
        parameters: settings(0, "").parameters.unwrap(),
    };
    let group = DubbingGroup {
        id: Uuid::now_v7(),
        name: "bad\0group".into(),
        areas: vec!["word".into()],
        uk: vec![rec.clone()],
        us: vec![rec],
    };
    assert_eq!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![group.clone()],
                    base_revision: 0
                }
            )
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    let mut invalid_recommendation = group;
    invalid_recommendation.name = "valid group".into();
    assert_eq!(
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![invalid_recommendation],
                    base_revision: 0
                }
            )
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    assert_eq!(repository.groups(actor).await.unwrap().revision, 0);
}

#[sqlx::test]
async fn group_save_waits_for_catalog_sync_lock(pool: PgPool) {
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let mut catalog = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(734892105)")
        .execute(&mut *catalog)
        .await
        .unwrap();
    let repository = VoiceLibraryRepository::new(pool.clone());
    let pending = tokio::spawn(async move {
        repository
            .save_groups(
                actor,
                0,
                SaveDubbingGroupsRequest {
                    groups: vec![],
                    base_revision: 0,
                },
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid=0 AND objid=734892105)").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            assert!(!pending.is_finished(), "group save must wait for catalog sync");
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    catalog.commit().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap().revision, 1);
}

async fn admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("voice-{}", id.simple()),
            display_name: "Voice editor".into(),
            password_hash: "unused-test-hash".into(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.access',$1)")
        .bind(id).execute(pool).await.unwrap();
    id
}

async fn voice(pool: &PgPool) {
    sqlx::query(r#"INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version)
        VALUES($1,'en-us-jenny','azure','en-US-JennyNeural','en-US','female','["chat"]','v1')"#)
        .bind(Uuid::now_v7()).execute(pool).await.unwrap();
}

fn settings(revision: i64, notes: &str) -> SaveVoiceSettingsRequest {
    SaveVoiceSettingsRequest {
        parameters: Some(VoiceParameters {
            rate_percent: -25,
            pitch_semitones: 2,
            volume_percent: Some(65),
            style: Some("chat".into()),
        }),
        notes: notes.into(),
        base_revision: revision,
    }
}

#[sqlx::test]
async fn personal_settings_read_back_and_do_not_leak_between_admins(pool: PgPool) {
    voice(&pool).await;
    let a = admin(&pool).await;
    let b = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    let saved = repository
        .save_settings(a, "en-us-jenny", 0, settings(0, "适合慢读"))
        .await
        .unwrap();
    assert_eq!(saved.personal_revision, 1);
    let read = repository.list(a).await.unwrap();
    assert_eq!(read.items[0].notes, "适合慢读");
    assert_eq!(read.items[0].parameters.as_ref().unwrap().rate_percent, -25);
    assert_eq!(
        read.items[0].parameters.as_ref().unwrap().volume_percent,
        Some(65)
    );
    let other = repository.list(b).await.unwrap();
    assert_eq!(other.items[0].notes, "");
    assert!(other.items[0].parameters.is_none());
    repository
        .save_settings(
            a,
            "en-us-jenny",
            0,
            SaveVoiceSettingsRequest {
                parameters: None,
                notes: "保留笔记".into(),
                base_revision: 1,
            },
        )
        .await
        .unwrap();
    let restored = repository.list(a).await.unwrap();
    assert!(restored.items[0].parameters.is_none());
    assert_eq!(restored.items[0].notes, "保留笔记");
}

#[sqlx::test]
async fn notes_can_preserve_old_parameters_after_voice_capabilities_change(pool: PgPool) {
    voice(&pool).await;
    let actor = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    repository
        .save_settings(actor, "en-us-jenny", 0, settings(0, "旧听感"))
        .await
        .unwrap();
    sqlx::query("UPDATE speech.voices SET styles='[]' WHERE alias='en-us-jenny'")
        .execute(&pool)
        .await
        .unwrap();
    let saved = repository
        .save_settings(actor, "en-us-jenny", 0, settings(1, "新听感"))
        .await
        .unwrap();
    assert_eq!(saved.notes, "新听感");
    assert_eq!(saved.parameters.unwrap().style.as_deref(), Some("chat"));
    let mut invalid = settings(2, "不能写入新参数");
    invalid.parameters.as_mut().unwrap().rate_percent = 20;
    assert!(
        repository
            .save_settings(actor, "en-us-jenny", 0, invalid)
            .await
            .is_err()
    );
}

#[sqlx::test]
async fn stale_and_concurrent_updates_do_not_overwrite_saved_settings(pool: PgPool) {
    voice(&pool).await;
    let actor = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool);
    let (first, second) = tokio::join!(
        repository.save_settings(actor, "en-us-jenny", 0, settings(0, "first")),
        repository.save_settings(actor, "en-us-jenny", 0, settings(0, "second"))
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let failure = if let Err(error) = first {
        error
    } else {
        second.unwrap_err()
    };
    assert_eq!(failure.code(), ErrorCode::RevisionConflict);
    assert_eq!(
        repository.list(actor).await.unwrap().items[0].personal_revision,
        1
    );
}

#[sqlx::test]
async fn shared_description_requires_global_permission_and_preserves_personal_data(pool: PgPool) {
    voice(&pool).await;
    let a = admin(&pool).await;
    let b = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    repository
        .save_settings(a, "en-us-jenny", 0, settings(0, "private"))
        .await
        .unwrap();
    let description = || SaveVoiceDescriptionRequest {
        description_zh: "清晰自然的女声".into(),
        base_revision: 0,
    };
    assert_eq!(
        repository
            .save_description(a, "en-us-jenny", 0, description())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)")
        .bind(a).execute(&pool).await.unwrap();
    repository
        .save_description(a, "en-us-jenny", 0, description())
        .await
        .unwrap();
    let other = repository.list(b).await.unwrap();
    assert_eq!(
        other.items[0].description_zh.as_deref(),
        Some("清晰自然的女声")
    );
    assert!(other.items[0].notes.is_empty());
    assert_eq!(repository.list(a).await.unwrap().items[0].notes, "private");
    assert_eq!(
        repository
            .save_description(a, "en-us-jenny", 0, description())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::RevisionConflict
    );
}

#[sqlx::test]
async fn invalid_parameters_disabled_voices_and_revoked_access_are_rejected(pool: PgPool) {
    voice(&pool).await;
    let actor = admin(&pool).await;
    let repository = VoiceLibraryRepository::new(pool.clone());
    let mut request = settings(0, "");
    request.parameters.as_mut().unwrap().rate_percent = -100;
    assert_eq!(
        repository
            .save_settings(actor, "en-us-jenny", 0, request)
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    let mut request = settings(0, "");
    request.parameters.as_mut().unwrap().style = Some("unsupported".into());
    assert!(
        repository
            .save_settings(actor, "en-us-jenny", 0, request)
            .await
            .is_err()
    );
    assert!(
        repository
            .save_settings(actor, "en-us-jenny", 0, settings(0, &"字".repeat(3001)))
            .await
            .is_err()
    );
    sqlx::query("UPDATE speech.voices SET enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .save_settings(actor, "en-us-jenny", 0, settings(0, ""))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::SpeechVoiceNotFound
    );
    assert!(repository.list(actor).await.unwrap().items.is_empty());
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository.list(actor).await.unwrap_err().code(),
        ErrorCode::Forbidden
    );
}

#[sqlx::test]
async fn migration_can_be_reverted_without_removing_existing_voice_catalog(pool: PgPool) {
    voice(&pool).await;
    sqlx::raw_sql(include_str!(
        "../migrations/20261021160000_dubbing_groups.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261021160000_dubbing_groups.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261021150000_voice_library.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM speech.voices")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::raw_sql(include_str!(
        "../migrations/20261021150000_voice_library.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let actor = admin(&pool).await;
    assert_eq!(
        VoiceLibraryRepository::new(pool)
            .list(actor)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}
