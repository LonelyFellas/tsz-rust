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
async fn current_accent_survives_catalog_sync_and_description_restore_is_independent(pool: PgPool) {
    use tsz_rust::speech::library::dto::{RestoreVoiceDescriptionRequest, SaveVoiceAccentRequest};
    use tsz_rust::speech::{CatalogVoice, Voice, locale::VoiceAccent};
    let actor = admin(&pool).await;
    let repo = VoiceLibraryRepository::new(pool.clone());
    let catalog = tsz_rust::speech::preview::PreviewRepository::new(pool.clone());
    let voices = [
        "en-GB", "en-AU", "en-NZ", "en-US", "en-CA", "en-IE", "en-SG",
    ]
    .map(|locale| CatalogVoice {
        voice: Voice::new("azure", format!("{locale}-TestNeural"), locale, vec![]).unwrap(),
        gender: "female".into(),
    });
    let aliases = catalog.sync_voices(&voices).await.unwrap();
    let initial = repo.list(actor).await.unwrap();
    for (locale, expected) in [
        ("en-GB", VoiceAccent::Gb),
        ("en-AU", VoiceAccent::Gb),
        ("en-NZ", VoiceAccent::Gb),
        ("en-US", VoiceAccent::Us),
        ("en-CA", VoiceAccent::Us),
        ("en-IE", VoiceAccent::None),
        ("en-SG", VoiceAccent::None),
    ] {
        let alias = format!("{}-testneural", locale.to_ascii_lowercase());
        assert_eq!(
            initial
                .items
                .iter()
                .find(|item| item.voice_alias == alias)
                .unwrap()
                .accent,
            expected
        );
    }
    let alias = aliases.last().unwrap();
    assert_eq!(
        repo.save_accent(
            actor,
            alias,
            0,
            SaveVoiceAccentRequest {
                accent: VoiceAccent::Gb,
                base_revision: 0,
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)")
        .bind(actor).execute(&pool).await.unwrap();
    let saved = repo
        .save_accent(
            actor,
            alias,
            0,
            SaveVoiceAccentRequest {
                accent: VoiceAccent::Gb,
                base_revision: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.accent_revision, 1);
    assert_eq!(
        repo.save_accent(
            actor,
            alias,
            0,
            SaveVoiceAccentRequest {
                accent: VoiceAccent::Us,
                base_revision: 0,
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    catalog.sync_voices(&voices).await.unwrap();
    catalog.ensure_catalog_voices().await.unwrap();
    let read = repo
        .list(actor)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|item| &item.voice_alias == alias)
        .unwrap();
    assert_eq!(read.accent, VoiceAccent::Gb);
    assert_eq!(read.accent_revision, 1);
    let native = catalog.voice_by_alias(alias).await.unwrap().unwrap();
    assert_eq!(native.voice.locale(), "en-SG");
    assert_eq!(native.voice.provider_voice_id(), "en-SG-TestNeural");
    let mut personal = settings(0, "private notes");
    personal.parameters.as_mut().unwrap().style = None;
    repo.save_settings(actor, alias, 0, personal).await.unwrap();
    repo.save_description(
        actor,
        alias,
        0,
        SaveVoiceDescriptionRequest {
            description_zh: "自定义介绍".into(),
            base_revision: 0,
        },
    )
    .await
    .unwrap();
    let restored = repo
        .restore_description(
            actor,
            alias,
            0,
            RestoreVoiceDescriptionRequest { base_revision: 1 },
        )
        .await
        .unwrap();
    assert_eq!(restored.description_zh, None);
    assert_eq!(restored.description_revision, 2);
    assert_eq!(restored.notes, "private notes");
    assert_eq!(restored.accent, VoiceAccent::Gb);
    assert_eq!(
        repo.save_description(
            actor,
            alias,
            0,
            SaveVoiceDescriptionRequest {
                description_zh: "stale".into(),
                base_revision: 0,
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
}

#[sqlx::test]
async fn current_accent_migration_round_trip_rejects_lossy_rollback(pool: PgPool) {
    const DOWN: &str = include_str!("../migrations/20261026090000_voice_current_accent.down.sql");
    const UP: &str = include_str!("../migrations/20261026090000_voice_current_accent.up.sql");
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    voice(&pool).await;
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT accent FROM speech.voices WHERE alias='en-us-jenny'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "us"
    );
    sqlx::query("UPDATE speech.voices SET accent='gb' WHERE alias='en-us-jenny'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::raw_sql(DOWN)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string()
            .contains("Cannot roll back current voice accents")
    );
    sqlx::query("UPDATE speech.voices SET accent='us' WHERE alias='en-us-jenny'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
}

fn group(name: &str, areas: &[&str]) -> DubbingGroup {
    DubbingGroup {
        id: Uuid::now_v7(),
        name: name.into(),
        areas: areas.iter().map(|area| (*area).into()).collect(),
        uk: vec![],
        us: vec![],
        revision: 0,
        is_global: false,
    }
}

#[sqlx::test]
async fn single_groups_preserve_other_groups_and_allow_shared_areas(pool: PgPool) {
    use tsz_rust::speech::library::dto::{DeleteDubbingGroupRequest, SaveDubbingGroupRequest};
    let actor = admin(&pool).await;
    let repo = VoiceLibraryRepository::new(pool.clone());
    let initial = repo.groups(actor).await.unwrap();
    assert_eq!(initial.groups.len(), 1);
    assert!(initial.groups[0].is_global);
    let a = group("A", &["word"]);
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: a.clone(),
                base_revision: 0
            },
            true
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let a = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: a,
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    let b = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("B", &["example"]),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    let mut draft_a = a.clone();
    draft_a.name = "A edited".into();
    let mut draft_b = b.clone();
    draft_b.name = "B edited".into();
    let saved = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: draft_b.clone(),
                base_revision: b.revision,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        saved.groups.iter().find(|g| g.id == a.id).unwrap().name,
        "A"
    );
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: draft_b,
                base_revision: b.revision
            },
            false
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    let saved = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: draft_a,
                base_revision: a.revision,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        saved.groups.iter().find(|g| g.id == b.id).unwrap().name,
        "B edited"
    );
    repo.save_group(
        actor,
        0,
        SaveDubbingGroupRequest {
            group: group("C", &["word"]),
            base_revision: 0,
        },
        true,
    )
    .await
    .unwrap();
    assert!(
        repo.delete_group(
            actor,
            0,
            initial.groups[0].id,
            DeleteDubbingGroupRequest { base_revision: 0 }
        )
        .await
        .is_err()
    );
    assert_eq!(
        repo.save_groups(
            actor,
            0,
            SaveDubbingGroupsRequest {
                groups: vec![],
                base_revision: saved.revision
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    assert_eq!(repo.groups(actor).await.unwrap().groups.len(), 4);
}

#[sqlx::test]
async fn concurrent_group_writes_are_independent_and_allow_shared_areas(pool: PgPool) {
    use tsz_rust::speech::library::dto::SaveDubbingGroupRequest;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let a = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("A", &[]),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    let b = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("B", &[]),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    let (left, right) = tokio::join!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: a.clone(),
                base_revision: 1
            },
            false
        ),
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: b,
                base_revision: 1
            },
            false
        )
    );
    assert!(left.is_ok());
    assert!(right.is_ok());
    let (left, right) = tokio::join!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: a.clone(),
                base_revision: 2
            },
            false
        ),
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: a,
                base_revision: 2
            },
            false
        )
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let (left, right) = tokio::join!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("C", &["word"]),
                base_revision: 0
            },
            true
        ),
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("D", &["word"]),
                base_revision: 0
            },
            true
        )
    );
    assert!(left.is_ok());
    assert!(right.is_ok());
    assert_eq!(
        repo.groups(actor)
            .await
            .unwrap()
            .groups
            .iter()
            .filter(|group| group.areas.iter().any(|area| area == "word"))
            .count(),
        2
    );
}

#[sqlx::test]
async fn group_completion_stably_orders_selected_and_keeps_unchanged_accent_mismatch(pool: PgPool) {
    use tsz_rust::speech::library::dto::{SaveDubbingGroupRequest, SaveVoiceAccentRequest};
    use tsz_rust::speech::locale::VoiceAccent;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let mut draft = group("selected", &["word"]);
    draft.uk = [false, true, true, false]
        .into_iter()
        .enumerate()
        .map(|(index, selected)| DubbingRecommendation {
            id: Uuid::now_v7(),
            name: index.to_string(),
            voice_alias: "en-gb-sonianeural".into(),
            selected,
            parameters: VoiceParameters {
                rate_percent: 0,
                pitch_semitones: 0,
                volume_percent: None,
                style: None,
                style_degree: None,
                role: None,
            },
        })
        .collect();
    let mut saved = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: draft,
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    assert_eq!(
        saved.uk.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["1", "2", "0", "3"]
    );
    repo.save_accent(
        actor,
        "en-gb-sonianeural",
        0,
        SaveVoiceAccentRequest {
            accent: VoiceAccent::None,
            base_revision: 0,
        },
    )
    .await
    .unwrap();
    saved.name = "still retained".into();
    let revision = saved.revision;
    let retained = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: saved,
                base_revision: revision,
            },
            false,
        )
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    assert_eq!(retained.uk.len(), 4);
    let mut changed = retained.clone();
    changed.uk[0].parameters.rate_percent = -20;
    assert!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: changed,
                base_revision: retained.revision
            },
            false
        )
        .await
        .is_err()
    );
}

#[sqlx::test]
async fn group_migration_preserves_existing_ids_and_refuses_unselected_rollback(pool: PgPool) {
    const UP: &str = include_str!("../migrations/20261026100000_dubbing_group_drafts.up.sql");
    const DOWN: &str = include_str!("../migrations/20261026100000_dubbing_group_drafts.down.sql");
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    let id = Uuid::now_v7();
    let rec = Uuid::now_v7();
    let old = serde_json::json!([{ "id":id,"name":"legacy","areas":["word"],"uk":[{"id":rec,"name":"natural","voice_alias":"old","parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}],"us":[{"id":Uuid::now_v7(),"name":"natural US","voice_alias":"old-us","parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}] }]);
    sqlx::query("UPDATE speech.dubbing_groups SET groups=$1")
        .bind(&old)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
    let migrated: serde_json::Value =
        sqlx::query_scalar("SELECT groups FROM speech.dubbing_groups")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(migrated[1]["id"], serde_json::json!(id));
    assert_eq!(migrated[1]["uk"][0]["id"], serde_json::json!(rec));
    assert_eq!(migrated[1]["uk"][0]["selected"], true);
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    let roundtrip: serde_json::Value =
        sqlx::query_scalar("SELECT groups FROM speech.dubbing_groups")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(roundtrip, old);
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE speech.dubbing_groups SET groups=jsonb_set(groups,'{1,uk,0,selected}','false')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(sqlx::raw_sql(DOWN).execute(&pool).await.is_err());
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
    let mut invalid_group = group("bad\0group", &["word"]);
    assert_eq!(
        repository
            .save_group(
                actor,
                0,
                tsz_rust::speech::library::dto::SaveDubbingGroupRequest {
                    group: invalid_group.clone(),
                    base_revision: 0
                },
                true
            )
            .await
            .unwrap_err()
            .status_code()
            .as_u16(),
        400
    );
    invalid_group.name = "valid".into();
    invalid_group.uk.push(DubbingRecommendation {
        id: Uuid::now_v7(),
        name: "bad\0name".into(),
        voice_alias: "en-us-jenny".into(),
        parameters: settings(0, "").parameters.unwrap(),
        selected: true,
    });
    assert!(
        repository
            .save_group(
                actor,
                0,
                tsz_rust::speech::library::dto::SaveDubbingGroupRequest {
                    group: invalid_group,
                    base_revision: 0
                },
                true
            )
            .await
            .is_err()
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
            .save_group(
                actor,
                0,
                tsz_rust::speech::library::dto::SaveDubbingGroupRequest {
                    group: group("wait", &[]),
                    base_revision: 0,
                },
                true,
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
            style_degree: None,
            role: None,
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

fn paired_input(name: &str) -> serde_json::Value {
    use serde_json::json;
    let side = |alias| json!({"id":Uuid::now_v7(),"voice_alias":alias,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}});
    json!({"base_revision":0,"group":{"id":Uuid::now_v7(),"name":name,"areas":[],"is_global":false,"pairs":[
        {"id":Uuid::now_v7(),"name":"first","selected":false,"uk":side("en-gb-sonianeural"),"us":side("en-us-jennyneural")},
        {"id":Uuid::now_v7(),"name":"second","selected":true,"uk":side("en-gb-sonianeural"),"us":side("en-us-jennyneural")}
    ]}})
}

#[sqlx::test]
async fn paired_writes_preserve_order_protect_old_clients_and_serialize_revisions(pool: PgPool) {
    use tsz_rust::speech::library::dto::{DeleteDubbingGroupRequest, SaveDubbingGroupRequest};
    let actor = admin(&pool).await;
    let repo = VoiceLibraryRepository::new(pool.clone());
    let mut input = paired_input("paired");
    let request = || serde_json::from_value(input.clone()).unwrap();
    assert_eq!(
        repo.save_paired_group(actor, 0, request(), true)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let initial = repo.groups_v3(actor).await.unwrap();
    let created = repo
        .save_paired_group(actor, 0, request(), true)
        .await
        .unwrap();
    assert_eq!(created.order_revision, initial.order_revision + 1);
    assert_eq!(
        serde_json::to_value(&created.groups[0]).unwrap()["recommendations"]["pairs"],
        input["group"]["pairs"]
    );
    let stored = repo
        .groups(actor)
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    assert!(
        !stored.uk[0].selected,
        "v3 must not sort selected items to the front"
    );
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: stored.clone(),
                base_revision: 1
            },
            false
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    assert_eq!(
        repo.delete_group(
            actor,
            0,
            stored.id,
            DeleteDubbingGroupRequest { base_revision: 1 }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    input["base_revision"] = 1.into();
    input["group"]["pairs"][0]["uk"]["parameters"]["rate_percent"] = (-20).into();
    let (a, b) = tokio::join!(
        repo.save_paired_group(
            actor,
            0,
            serde_json::from_value(input.clone()).unwrap(),
            false
        ),
        repo.save_paired_group(
            actor,
            0,
            serde_json::from_value(input.clone()).unwrap(),
            false
        )
    );
    let saved = match (a, b) {
        (Ok(saved), Err(error)) | (Err(error), Ok(saved)) => {
            assert_eq!(error.code(), ErrorCode::RevisionConflict);
            saved
        }
        _ => panic!("exactly one write must succeed"),
    };
    assert_eq!(saved.order_revision, created.order_revision);
    assert_eq!(saved.groups[0].revision, 2);
    let read = repo.groups_v3(actor).await.unwrap();
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        serde_json::to_value(saved).unwrap()
    );
    let sides = repo
        .groups(actor)
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    assert_eq!(sides.uk[0].parameters.rate_percent, -20);
    assert_eq!(
        serde_json::to_value(sides.us).unwrap(),
        serde_json::to_value(stored.us).unwrap()
    );
    assert_eq!(
        repo.delete_paired_group(
            actor,
            0,
            stored.id,
            DeleteDubbingGroupRequest { base_revision: 1 }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    let removed = repo
        .delete_paired_group(
            actor,
            0,
            stored.id,
            DeleteDubbingGroupRequest { base_revision: 2 },
        )
        .await
        .unwrap();
    assert_eq!(removed.order_revision, read.order_revision + 1);
    assert_eq!(removed.groups.len(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM speech.dubbing_group_pairings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn legacy_pair_conversion_is_explicit_and_retains_every_side_identity(pool: PgPool) {
    use serde_json::json;
    use tsz_rust::speech::library::dto::SaveDubbingGroupRequest;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let mut input = paired_input("history");
    let pair_a = input["group"]["pairs"][0].clone();
    let pair_b = input["group"]["pairs"][1].clone();
    // Same name and provider voice, different parameters; there is no US side.
    let mut old = group("history", &[]);
    old.id = serde_json::from_value(input["group"]["id"].clone()).unwrap();
    for (index, pair) in [pair_a, pair_b].iter().enumerate() {
        let mut side = pair["uk"].clone();
        side["name"] = json!("same");
        side["selected"] = json!(false);
        side["parameters"]["rate_percent"] = json!(index as i16 * 10);
        input["group"]["pairs"][index]["uk"] = pair["uk"].clone();
        input["group"]["pairs"][index]["uk"]["parameters"] = side["parameters"].clone();
        input["group"]["pairs"][index]["name"] = json!("same");
        input["group"]["pairs"][index]["selected"] = json!(false);
        old.uk.push(serde_json::from_value(side).unwrap());
    }
    let saved = repo
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: old.clone(),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap();
    let v3 = serde_json::to_value(repo.groups_v3(actor).await.unwrap()).unwrap();
    assert_eq!(v3["groups"][0]["recommendations"]["format"], "legacy");
    assert_eq!(
        v3["groups"][0]["recommendations"]["uk"],
        serde_json::to_value(&saved.groups[0].uk).unwrap()
    );
    assert_eq!(v3["groups"][0]["recommendations"]["us"], json!([]));
    use sqlx::Row;
    let inventory = sqlx::query(include_str!("../ops/speech-dubbing/inventory.sql"))
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(inventory[0].get::<bool, _>("unequal_sides"));
    let repeated: serde_json::Value = inventory[0].get("repeated_voices");
    assert_eq!(repeated[0]["versions"], 2);
    assert_eq!(repeated[0]["parameter_variants"], 2);
    input["base_revision"] = json!(1);
    let mut missing = input.clone();
    missing["group"]["pairs"].as_array_mut().unwrap().pop();
    assert_eq!(
        repo.save_paired_group(actor, 0, serde_json::from_value(missing).unwrap(), false)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidRequestBody
    );
    let converted = repo
        .save_paired_group(
            actor,
            0,
            serde_json::from_value(input.clone()).unwrap(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(converted.order_revision, 1);
    assert_eq!(
        serde_json::to_value(&converted.groups[0]).unwrap()["recommendations"]["pairs"],
        input["group"]["pairs"]
    );
    let retained = repo
        .groups(actor)
        .await
        .unwrap()
        .groups
        .into_iter()
        .rfind(|group| !group.is_global)
        .unwrap();
    assert_eq!(
        serde_json::to_value(retained.uk).unwrap(),
        serde_json::to_value(old.uk).unwrap()
    );
}

#[sqlx::test]
async fn paired_identity_and_voice_validation_rejects_without_partial_writes(pool: PgPool) {
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let good = paired_input("original");
    repo.save_paired_group(
        actor,
        0,
        serde_json::from_value(good.clone()).unwrap(),
        true,
    )
    .await
    .unwrap();
    let before = serde_json::to_value(repo.groups_v3(actor).await.unwrap()).unwrap();
    for (path, value) in [
        ("/group/pairs/0/id", good["group"]["pairs"][0]["id"].clone()),
        (
            "/group/pairs/0/id",
            good["group"]["pairs"][0]["uk"]["id"].clone(),
        ),
        (
            "/group/pairs/0/uk/id",
            good["group"]["pairs"][0]["id"].clone(),
        ),
        ("/group/id", good["group"]["pairs"][0]["id"].clone()),
        (
            "/group/pairs/0/us/voice_alias",
            serde_json::json!("en-gb-sonianeural"),
        ),
        (
            "/group/pairs/0/uk/parameters/rate_percent",
            serde_json::json!(999),
        ),
        ("/group/pairs/0/id", serde_json::json!(Uuid::nil())),
    ] {
        let mut invalid = paired_input("invalid");
        *invalid.pointer_mut(path).unwrap() = value;
        assert_eq!(
            repo.save_paired_group(actor, 0, serde_json::from_value(invalid).unwrap(), true)
                .await
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRequestBody
        );
        assert_eq!(
            serde_json::to_value(repo.groups_v3(actor).await.unwrap()).unwrap(),
            before
        );
    }
    let mut duplicate = paired_input("duplicate");
    duplicate["group"]["pairs"][1]["id"] = duplicate["group"]["pairs"][0]["id"].clone();
    assert!(
        repo.save_paired_group(actor, 0, serde_json::from_value(duplicate).unwrap(), true)
            .await
            .is_err()
    );
    let mut revoked = good.clone();
    revoked["base_revision"] = 1.into();
    sqlx::query("UPDATE admins SET security_version=security_version+1 WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repo.save_paired_group(actor, 0, serde_json::from_value(revoked).unwrap(), false)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::InvalidToken
    );
}

#[sqlx::test]
async fn pairing_migration_preserves_legacy_bytes_and_refuses_lossy_down(pool: PgPool) {
    const UP: &str =
        include_str!("../migrations/20261031090000_dubbing_recommendation_pairs.up.sql");
    const DOWN: &str =
        include_str!("../migrations/20261031090000_dubbing_recommendation_pairs.down.sql");
    let mut history = paired_input("migration history");
    let side = history["group"]["pairs"][0]["uk"].clone();
    history["group"]["uk"] = serde_json::json!([{ "id":side["id"],"name":"same","voice_alias":side["voice_alias"],"parameters":side["parameters"],"selected":false }]);
    history["group"]["us"] = serde_json::json!([]);
    history["group"].as_object_mut().unwrap().remove("pairs");
    history["group"]["revision"] = 7.into();
    sqlx::query("UPDATE speech.dubbing_groups SET groups=groups || jsonb_build_array($1::jsonb)")
        .bind(&history["group"])
        .execute(&pool)
        .await
        .unwrap();
    let before: serde_json::Value = sqlx::query_scalar("SELECT groups FROM speech.dubbing_groups")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(DOWN).execute(&pool).await.unwrap();
    sqlx::raw_sql(UP).execute(&pool).await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar("SELECT groups FROM speech.dubbing_groups")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    sqlx::query("INSERT INTO speech.dubbing_group_pairings(group_id,pairs) VALUES($1,'[]')")
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::raw_sql(DOWN)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string()
            .contains("Cannot discard saved dubbing pair identities")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM speech.dubbing_group_pairings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn group_order_is_independent_complete_and_conflicts_with_membership_changes(pool: PgPool) {
    use tsz_rust::speech::library::dto::{
        DeleteDubbingGroupRequest, SaveDubbingGroupOrderRequest, SaveDubbingGroupRequest,
    };
    let actor = admin(&pool).await;
    let repo = VoiceLibraryRepository::new(pool.clone());
    let global = repo.groups(actor).await.unwrap().groups[0].id;
    assert_eq!(
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![global],
                base_revision: 0
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::Forbidden
    );
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let a = group("priority A", &["word"]);
    let b = group("priority B", &["word"]);
    for g in [&a, &b] {
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: g.clone(),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap();
    }
    let start = repo.groups_v3(actor).await.unwrap();
    assert!(start.groups.last().unwrap().is_global);
    let ids = vec![b.id, a.id, global];
    let mut draft_a = a.clone();
    draft_a.name = "A draft must stay local".into();
    let mut content_b = b.clone();
    content_b.name = "B committed".into();
    let (ordered, edited) = tokio::join!(
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: ids.clone(),
                base_revision: start.order_revision
            }
        ),
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: content_b,
                base_revision: 1
            },
            false
        )
    );
    ordered.unwrap();
    edited.unwrap();
    let read = repo.groups_v3(actor).await.unwrap();
    assert_eq!(read.groups.iter().map(|g| g.id).collect::<Vec<_>>(), ids);
    assert_eq!(read.order_revision, start.order_revision + 1);
    assert_eq!(
        read.groups.iter().find(|g| g.id == a.id).unwrap().name,
        "priority A"
    );
    assert_eq!(
        read.groups.iter().find(|g| g.id == a.id).unwrap().revision,
        1
    );
    assert_eq!(
        read.groups.iter().find(|g| g.id == b.id).unwrap().revision,
        2
    );
    assert_eq!(
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: ids.clone(),
                base_revision: start.order_revision
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    for invalid in [
        vec![a.id, global],
        vec![a.id, a.id, global],
        vec![global, a.id, b.id],
        vec![a.id, Uuid::now_v7(), global],
    ] {
        assert_eq!(
            repo.save_order(
                actor,
                0,
                SaveDubbingGroupOrderRequest {
                    group_ids: invalid,
                    base_revision: read.order_revision
                }
            )
            .await
            .unwrap_err()
            .code(),
            ErrorCode::InvalidRequestBody
        );
    }
    // A no-op does not reset the waiting window or advance either revision.
    let unchanged = repo
        .save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: ids.clone(),
                base_revision: read.order_revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(unchanged.revision, read.revision);
    assert_eq!(unchanged.order_revision, read.order_revision);
    let (left, right) = tokio::join!(
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![a.id, b.id, global],
                base_revision: read.order_revision
            }
        ),
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![a.id, b.id, global],
                base_revision: read.order_revision
            }
        )
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let stale = repo.groups_v3(actor).await.unwrap();
    let c = group("priority C", &["word"]);
    let (membership, order) = tokio::join!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: c.clone(),
                base_revision: 0
            },
            true
        ),
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![b.id, a.id, global],
                base_revision: stale.order_revision
            }
        )
    );
    membership.unwrap();
    if let Err(error) = order {
        assert_eq!(error.code(), ErrorCode::RevisionConflict);
    }
    let current = repo.groups_v3(actor).await.unwrap();
    assert!(current.groups.last().unwrap().is_global);
    repo.delete_group(
        actor,
        0,
        c.id,
        DeleteDubbingGroupRequest { base_revision: 1 },
    )
    .await
    .unwrap();
    assert_eq!(
        repo.save_order(
            actor,
            0,
            SaveDubbingGroupOrderRequest {
                group_ids: current.groups.iter().map(|g| g.id).collect(),
                base_revision: current.order_revision
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    // Group-internal duplicates and duplicate names are still rejected.
    let mut duplicate = group("duplicate areas", &["word", "word"]);
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: duplicate.clone(),
                base_revision: 0
            },
            true
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::InvalidRequestBody
    );
    duplicate.areas = vec!["word".into()];
    duplicate.name = "priority A".into();
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: duplicate,
                base_revision: 0
            },
            true
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
    assert_eq!(
        repo.save_order(
            actor,
            1,
            SaveDubbingGroupOrderRequest {
                group_ids: vec![],
                base_revision: 0
            }
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::InvalidToken
    );
}

#[sqlx::test]
async fn waiting_window_starts_after_the_configuration_lock_is_acquired(pool: PgPool) {
    use tsz_rust::speech::library::dto::SaveDubbingGroupRequest;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT revision FROM speech.dubbing_groups WHERE singleton FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let value = group("waited submission", &["word"]);
    let id = value.id;
    let saving = {
        let pool = pool.clone();
        tokio::spawn(async move {
            VoiceLibraryRepository::new(pool)
                .save_group(
                    actor,
                    0,
                    SaveDubbingGroupRequest {
                        group: value,
                        base_revision: 0,
                    },
                    true,
                )
                .await
                .unwrap();
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM speech.dubbing_groups WHERE singleton FOR UPDATE%')").fetch_one(&pool).await.unwrap();
            if blocked {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let released: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    held.commit().await.unwrap();
    saving.await.unwrap();
    let deadline: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT due_at FROM speech.dubbing_audio_updates WHERE group_id=$1 AND state='pending'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        deadline >= released + chrono::Duration::seconds(60),
        "lock wait must not consume the submitted update window"
    );
}

#[sqlx::test]
async fn creation_preserves_existing_priority_when_legacy_storage_has_global_first(pool: PgPool) {
    use serde_json::json;
    use tsz_rust::speech::library::dto::SaveDubbingGroupRequest;
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.global_dubbing',$1)").bind(actor).execute(&pool).await.unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let repo = VoiceLibraryRepository::new(pool.clone());
    let global = repo.groups(actor).await.unwrap().groups.remove(0);
    let mut a = group("historical A", &["word"]);
    a.revision = 1;
    let mut b = group("historical B", &["word"]);
    b.revision = 1;
    let recommendation = |name| {
        serde_json::from_value(json!({"id":Uuid::now_v7(),"name":name,"voice_alias":"en-gb-sonianeural","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}})).unwrap()
    };
    a.uk.push(recommendation("A"));
    b.uk.push(recommendation("B"));
    sqlx::query("UPDATE speech.dubbing_groups SET groups=$1 WHERE singleton")
        .bind(json!([global, a, b]))
        .execute(&pool)
        .await
        .unwrap();
    let original = repo.groups_v3(actor).await.unwrap();
    let same = repo
        .save_order(
            actor,
            0,
            tsz_rust::speech::library::dto::SaveDubbingGroupOrderRequest {
                group_ids: original.groups.iter().map(|group| group.id).collect(),
                base_revision: original.order_revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(original.revision, same.revision);
    assert_eq!(original.order_revision, same.order_revision);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM speech.dubbing_audio_updates")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let mut c = group("new C", &["word"]);
    c.uk.push(recommendation("C"));
    repo.save_group(
        actor,
        0,
        SaveDubbingGroupRequest {
            group: c.clone(),
            base_revision: 0,
        },
        true,
    )
    .await
    .unwrap();
    let stored = repo.groups(actor).await.unwrap();
    assert_eq!(
        stored.groups.iter().map(|g| g.id).collect::<Vec<_>>(),
        vec![a.id, b.id, c.id, global.id]
    );
    let read = repo.groups_v3(actor).await.unwrap();
    assert_eq!(
        read.groups.iter().map(|g| g.id).collect::<Vec<_>>(),
        vec![a.id, b.id, c.id, global.id]
    );
    let profile =
        serde_json::from_value(json!({"voices":[],"dubbing":{"mode":"inherit","area":"word"}}))
            .unwrap();
    let content =
        serde_json::from_value(json!({"version":2,"text":"cat","annotations":[]})).unwrap();
    let resolved = tsz_rust::speech::dubbing::resolve_in(
        &mut pool.acquire().await.unwrap(),
        &profile,
        "gb",
        &content,
        None,
    )
    .await
    .unwrap();
    assert_eq!(resolved[0].version_id, Some(a.uk[0].id));
}
