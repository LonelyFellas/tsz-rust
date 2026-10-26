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
async fn single_groups_preserve_other_groups_and_reject_stale_or_occupied_writes(pool: PgPool) {
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
        .pop()
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
        .pop()
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
    assert_eq!(
        repo.save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: group("C", &["word"]),
                base_revision: 0
            },
            true
        )
        .await
        .unwrap_err()
        .code(),
        ErrorCode::RevisionConflict
    );
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
    assert_eq!(repo.groups(actor).await.unwrap().groups.len(), 3);
}

#[sqlx::test]
async fn concurrent_group_writes_are_independent_and_serialize_area_ownership(pool: PgPool) {
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
        .pop()
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
        .pop()
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
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        repo.groups(actor)
            .await
            .unwrap()
            .groups
            .iter()
            .filter(|group| group.areas.iter().any(|area| area == "word"))
            .count(),
        1
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
        .pop()
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
        .pop()
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
