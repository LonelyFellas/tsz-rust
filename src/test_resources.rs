//! Opt-in online acceptance resources. Ordinary requests retain their original storage roots.
use std::{collections::BTreeMap, time::Duration};

use axum::http::HeaderMap;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    platform::storage::{
        ObjectKey, ObjectStorageConfig, OssAdapterConfig, SecretString, StorageRegistry,
        test_key_parts, valid_test_run,
    },
    state::AppState,
};

#[cfg(test)]
mod lifecycle_tests;
mod object_versions;
use object_versions::{object_keys_with_versions, purge_object_versions};

pub const HEADER: &str = "x-tsz-test-run";

pub(crate) fn scope_conflict() -> AppError {
    AppError::conflict(
        ErrorCode::RevisionConflict,
        None,
        "测试资源批次不匹配或已封存",
    )
}

pub(crate) fn require_same_run(
    expected: Option<&str>,
    actual: Option<&str>,
) -> Result<(), AppError> {
    if expected != actual {
        return Err(scope_conflict());
    }
    Ok(())
}

/// A shared row lock fences generation and other writes until their last side effect.
pub(crate) async fn lock_run(
    conn: &mut PgConnection,
    actor: Uuid,
    run: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO lexicon_next.test_resource_runs(run_id,actor_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
        .bind(run).bind(actor).execute(&mut *conn).await.map_err(AppError::internal)?;
    let (owner, purge): (Uuid, Option<Uuid>) = sqlx::query_as(
        "SELECT actor_id,purge_request_id FROM lexicon_next.test_resource_runs WHERE run_id=$1 FOR SHARE",
    )
    .bind(run)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)?;
    if owner != actor || purge.is_some() {
        return Err(scope_conflict());
    }
    Ok(())
}

pub(crate) async fn start_run_write(
    pool: &PgPool,
    actor: Uuid,
    run: Option<&str>,
) -> Result<Option<Transaction<'static, Postgres>>, AppError> {
    let Some(run) = run else { return Ok(None) };
    // The registration must be visible to writes performed on another connection.
    sqlx::query("INSERT INTO lexicon_next.test_resource_runs(run_id,actor_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
        .bind(run).bind(actor).execute(pool).await.map_err(AppError::internal)?;
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_run(&mut tx, actor, run).await?;
    Ok(Some(tx))
}

pub(crate) async fn entry_run(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar("SELECT run_id FROM lexicon_next.test_resource_entries WHERE entry_id=$1")
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(AppError::internal)
}

pub(crate) async fn sentence_run(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT run_id FROM lexicon_next.test_resource_sentences WHERE sentence_id=$1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
    .map_err(AppError::internal)
}

pub(crate) async fn register_sentence(
    conn: &mut PgConnection,
    run: &str,
    sentence: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO lexicon_next.test_resource_sentences(sentence_id,run_id) VALUES ($1,$2)",
    )
    .bind(sentence)
    .bind(run)
    .execute(conn)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

pub(crate) fn scoped_write(
    state: &AppState,
    headers: &HeaderMap,
    actor: Uuid,
) -> Result<(AppState, Option<String>), AppError> {
    let (scoped, run) = scoped_state(state, headers, actor, true)?;
    if run.is_none()
        && state.test_resource_purge_enabled
        && state
            .test_resources
            .as_ref()
            .is_some_and(|config| config.admins.contains(&actor))
    {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "专用测试管理员必须提供测试批次",
        ));
    }
    Ok((scoped, run))
}

pub async fn register_entry(
    conn: &mut PgConnection,
    run: &str,
    actor: Uuid,
    entry_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO lexicon_next.test_resource_entries(run_id,entry_id,actor_id) VALUES($1,$2,$3)",
    )
    .bind(run)
    .bind(entry_id)
    .bind(actor)
    .execute(conn)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct TestResourcesConfig {
    adapter: OssAdapterConfig,
    spaces: ObjectStorageConfig,
    admins: Vec<Uuid>,
    users: Vec<Uuid>,
    #[cfg(test)]
    cleanup_operator_for_test: Option<opendal::Operator>,
}

impl TestResourcesConfig {
    pub fn from_pairs(
        pairs: &[(String, String)],
        spaces: &ObjectStorageConfig,
    ) -> Result<Option<Self>, String> {
        let mut values = BTreeMap::new();
        let fields = [
            "OSS_ENDPOINT",
            "OSS_REGION",
            "OSS_ACCESS_KEY_ID",
            "OSS_ACCESS_KEY_SECRET",
            "ADMIN_IDS",
            "USER_IDS",
        ];
        for (name, value) in pairs {
            if let Some(field) = name.strip_prefix("TEST_RESOURCES_")
                && (!fields.contains(&field) || values.insert(field, value).is_some())
            {
                return Err("unknown or duplicate TEST_RESOURCES field".into());
            }
        }
        if values.is_empty() {
            return Ok(None);
        }
        let get = |field: &str| {
            values
                .get(field)
                .map(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("missing TEST_RESOURCES_{field}"))
        };
        let ids = |field: &str| -> Result<Vec<Uuid>, String> {
            match values.get(field).filter(|v| !v.is_empty()) {
                None => Ok(Vec::new()),
                Some(value) => value
                    .split(',')
                    .map(|id| {
                        Uuid::parse_str(id.trim())
                            .map_err(|_| format!("invalid TEST_RESOURCES_{field}"))
                    })
                    .collect(),
            }
        };
        let admins = ids("ADMIN_IDS")?;
        let users = ids("USER_IDS")?;
        if (admins.is_empty() && users.is_empty()) || spaces.is_empty() {
            return Err(
                "TEST_RESOURCES requires explicit actor IDs and configured storage spaces".into(),
            );
        }
        let credential = |field| {
            SecretString::new(get(field)?.to_owned())
                .map_err(|_| format!("invalid TEST_RESOURCES_{field}"))
        };
        let adapter = OssAdapterConfig::new(
            get("OSS_ENDPOINT")?,
            get("OSS_REGION")?,
            "tshb-test-assets",
            "/temporary-tests",
            credential("OSS_ACCESS_KEY_ID")?,
            credential("OSS_ACCESS_KEY_SECRET")?,
        )
        .map_err(|_| "invalid TEST_RESOURCES OSS configuration".to_owned())?;
        Ok(Some(Self {
            adapter,
            spaces: spaces.clone(),
            admins,
            users,
            #[cfg(test)]
            cleanup_operator_for_test: None,
        }))
    }

    fn cleanup_operator(&self) -> opendal::Result<opendal::Operator> {
        #[cfg(test)]
        if let Some(operator) = &self.cleanup_operator_for_test {
            return Ok(operator.clone());
        }
        self.adapter.operator()
    }

    pub(crate) async fn purge_audio<'a>(
        &self,
        run: &str,
        keys: impl Iterator<Item = &'a ObjectKey>,
    ) -> Result<(), AppError> {
        let unavailable =
            || AppError::unavailable(ErrorCode::ServiceUnavailable, "audio object cleanup failed");
        if self.spaces.policy("audio").is_none() {
            return Err(unavailable());
        }
        let operator = self.cleanup_operator().map_err(|_| unavailable())?;
        for key in keys {
            let Some((owner, "audio", inner)) = test_key_parts(key.as_str()) else {
                return Err(unavailable());
            };
            if owner != run {
                return Err(unavailable());
            }
            let physical_key = format!("{run}/audio/{inner}");
            tokio::time::timeout(
                Duration::from_secs(30),
                purge_object_versions(&operator, &physical_key),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        }
        Ok(())
    }

    pub(crate) async fn purge_run_objects<'a>(
        &self,
        run: &str,
        keys: impl Iterator<Item = &'a ObjectKey>,
    ) -> Result<(), AppError> {
        use std::collections::BTreeSet;
        let unavailable =
            || AppError::unavailable(ErrorCode::ServiceUnavailable, "test object cleanup failed");
        let operator = self.cleanup_operator().map_err(|_| unavailable())?;
        let mut exact = BTreeSet::new();
        for key in keys {
            let Some((owner, space @ ("audio" | "speech"), inner)) = test_key_parts(key.as_str())
            else {
                return Err(scope_conflict());
            };
            if owner != run || (space == "speech" && !inner.starts_with("previews/")) {
                return Err(scope_conflict());
            }
            exact.insert(format!("{run}/{space}/{inner}"));
        }

        let prefixes = [
            format!("{run}/audio/assets/generated/"),
            format!("{run}/speech/previews/"),
        ];
        // A missing DB registration is not permission to delete an object.
        for prefix in &prefixes {
            let listed = tokio::time::timeout(
                Duration::from_secs(30),
                object_keys_with_versions(&operator, prefix),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
            if listed.iter().any(|key| !exact.contains(key)) {
                return Err(AppError::conflict(
                    ErrorCode::RevisionConflict,
                    None,
                    "测试存储存在未登记对象",
                ));
            }
        }
        for key in exact {
            tokio::time::timeout(
                Duration::from_secs(30),
                purge_object_versions(&operator, &key),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        }
        for prefix in &prefixes {
            let listed = tokio::time::timeout(
                Duration::from_secs(30),
                object_keys_with_versions(&operator, prefix),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
            if !listed.is_empty() {
                return Err(unavailable());
            }
        }
        Ok(())
    }

    pub fn registry(
        &self,
        run: Option<&str>,
    ) -> Result<StorageRegistry, crate::platform::storage::StorageError> {
        #[cfg(test)]
        if let Some(operator) = &self.cleanup_operator_for_test {
            use crate::platform::storage::{ObjectStore, OssAdapter, TestRoutedStore};
            return StorageRegistry::from_stores(["audio", "speech"].into_iter().filter_map(
                |name| {
                    let policy = self.spaces.policy(name)?.clone();
                    let store = OssAdapter::object_store_using_operator(
                        name.parse().unwrap(),
                        policy,
                        self.adapter.clone(),
                        operator.clone(),
                    );
                    Some(std::sync::Arc::new(TestRoutedStore {
                        normal: store.clone(),
                        temporary: store,
                        run: run.map(str::to_owned),
                    }) as std::sync::Arc<dyn ObjectStore>)
                },
            ));
        }
        self.spaces.build_test_routed_registry(&self.adapter, run)
    }
}

/// Header validation occurs after the handler's usual realm authentication; it never grants business permissions.
pub fn scoped_state(
    state: &AppState,
    headers: &HeaderMap,
    actor: Uuid,
    admin: bool,
) -> Result<(AppState, Option<String>), AppError> {
    let Some(value) = headers.get(HEADER) else {
        return Ok((state.clone(), None));
    };
    let run = value.to_str().ok().filter(|value| valid_test_run(value));
    if headers.get_all(HEADER).iter().count() != 1 || run.is_none() {
        return Err(AppError::bad_request(
            ErrorCode::InvalidRequestBody,
            "invalid test resource run",
        ));
    }
    let config = state
        .test_resources
        .as_ref()
        .ok_or_else(|| AppError::forbidden(ErrorCode::Forbidden, "test resources are disabled"))?;
    if !(if admin { &config.admins } else { &config.users }).contains(&actor) {
        return Err(AppError::forbidden(
            ErrorCode::Forbidden,
            "test resource actor is not allowed",
        ));
    }
    let run = run.expect("validated above");
    let mut scoped = state.clone();
    scoped.object_storage = config.registry(Some(run)).map_err(|_| {
        AppError::unavailable(
            ErrorCode::ServiceUnavailable,
            "test resource storage unavailable",
        )
    })?;
    Ok((scoped, Some(run.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    fn fixture() -> (Vec<(String, String)>, ObjectStorageConfig, Uuid) {
        let storage = ObjectStorageConfig::from_pairs(
            [
                ("OBJECT_STORAGE_SPACES", "speech"),
                ("OBJECT_STORAGE_SPEECH_BACKEND", "oss"),
                (
                    "OBJECT_STORAGE_SPEECH_OSS_ENDPOINT",
                    "https://oss-cn-heyuan.aliyuncs.com",
                ),
                ("OBJECT_STORAGE_SPEECH_OSS_REGION", "cn-heyuan"),
                ("OBJECT_STORAGE_SPEECH_OSS_BUCKET", "tshb-test-assets"),
                ("OBJECT_STORAGE_SPEECH_OSS_ROOT", "/speech"),
                ("OBJECT_STORAGE_SPEECH_OSS_ACCESS_KEY_ID", "normal-id"),
                (
                    "OBJECT_STORAGE_SPEECH_OSS_ACCESS_KEY_SECRET",
                    "normal-secret",
                ),
                ("OBJECT_STORAGE_SPEECH_PRIVACY", "private"),
                ("OBJECT_STORAGE_SPEECH_MAX_OBJECT_SIZE_BYTES", "1024"),
                ("OBJECT_STORAGE_SPEECH_PRESIGN_TTL_SECONDS", "60"),
                ("OBJECT_STORAGE_SPEECH_CACHE_CONTROL", "none"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
        )
        .unwrap();
        let actor = Uuid::now_v7();
        let mut pairs = vec![
            (
                "TEST_RESOURCES_OSS_ENDPOINT",
                "https://oss-cn-heyuan.aliyuncs.com",
            ),
            ("TEST_RESOURCES_OSS_REGION", "cn-heyuan"),
            ("TEST_RESOURCES_OSS_ACCESS_KEY_ID", "test-id"),
            ("TEST_RESOURCES_OSS_ACCESS_KEY_SECRET", "test-secret"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<Vec<_>>();
        pairs.push(("TEST_RESOURCES_ADMIN_IDS".into(), actor.to_string()));
        (pairs, storage, actor)
    }

    #[tokio::test]
    async fn purge_versions_removes_only_the_exact_key_and_verifies_absence() {
        use axum::{
            Router,
            extract::State,
            http::{Method, StatusCode, Uri},
        };
        use opendal::{Operator, services::Oss};
        use std::sync::Mutex;

        // The second key shares the requested prefix and must survive the cleanup.
        struct Inventory {
            versions: Vec<(&'static str, &'static str, &'static str)>,
            retain_deleted: bool,
        }
        async fn respond(
            State(state): State<Arc<Mutex<Inventory>>>,
            method: Method,
            uri: Uri,
        ) -> (StatusCode, String) {
            let mut inventory = state.lock().unwrap();
            if method == Method::GET {
                assert!(
                    uri.query()
                        .unwrap()
                        .split('&')
                        .any(|param| param.trim_end_matches('=') == "versions")
                );
                let entries = inventory.versions.iter().map(|(key, id, kind)| {
                    format!("<{kind}><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>false</IsLatest><Size>2</Size><LastModified>2026-11-22T00:00:00Z</LastModified></{kind}>")
                }).collect::<String>();
                let body = format!(
                    "<ListVersionsResult><IsTruncated>false</IsTruncated>{entries}</ListVersionsResult>"
                );
                return (StatusCode::OK, body);
            }
            assert_eq!(method, Method::DELETE);
            let version_id = uri.query().unwrap().strip_prefix("versionId=").unwrap();
            if !inventory.retain_deleted {
                let path = uri.path().trim_start_matches('/');
                inventory
                    .versions
                    .retain(|(key, id, _)| *key != path || *id != version_id);
            }
            (StatusCode::NO_CONTENT, String::new())
        }
        let inventory = Arc::new(Mutex::new(Inventory {
            versions: vec![
                ("clip", "v1", "Version"),
                ("clip", "v2", "Version"),
                ("clip", "marker", "DeleteMarker"),
                ("clip-other", "keep", "Version"),
            ],
            retain_deleted: true,
        }));
        let server = Router::new()
            .fallback(respond)
            .with_state(inventory.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        opendal::install_default();
        let operator = Operator::new(
            Oss::default()
                .bucket("fixture")
                .endpoint(&endpoint)
                .addressing_style("cname")
                .access_key_id("fixture")
                .access_key_secret("fixture"),
        )
        .unwrap();
        assert!(purge_object_versions(&operator, "clip").await.is_err());
        inventory.lock().unwrap().retain_deleted = false;
        purge_object_versions(&operator, "clip").await.unwrap();
        assert_eq!(
            inventory.lock().unwrap().versions.as_slice(),
            &[("clip-other", "keep", "Version")]
        );
        server.abort();
    }

    #[sqlx::test]
    async fn concurrent_purge_returns_one_receipt_and_cleans_only_exclusive_commands(
        pool: sqlx::PgPool,
    ) {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use serde_json::{Value, json};
        use tower::ServiceExt;

        let (pairs, storage, actor) = fixture();
        let mut state = AppState::for_test(pool.clone());
        state.test_resource_purge_enabled = true;
        state.test_resources = TestResourcesConfig::from_pairs(&pairs, &storage)
            .unwrap()
            .map(Arc::new);
        sqlx::query("INSERT INTO admins(id,phone,display_name,password_hash,role,must_change_password) VALUES($1,'purge-test','Test','unused','super_admin',false)")
            .bind(actor).execute(&pool).await.unwrap();
        let token = state
            .admin_token_manager
            .generate(actor, "super_admin")
            .unwrap();
        let run = "purge-regression";
        let call = |path: &'static str, body: Value| {
            let request = Request::post(format!("/api/v1/admin/lexicon-next/{path}"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .header(HEADER, run)
                .body(Body::from(body.to_string()))
                .unwrap();
            let router = crate::router(state.clone());
            async move {
                let response = router.oneshot(request).await.unwrap();
                let status = response.status();
                let result: Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                (status, result)
            }
        };
        let mut entries = Vec::new();
        for word in ["alpha", "bravo"] {
            let (status, entry) = call(
                "entries",
                json!({"target_language":"en","kind":"word","headword":word}),
            )
            .await;
            assert!(status.is_success(), "{entry}");
            entries.push(entry["id"].clone());
        }
        // The reviewed fixture was the only learning source for this dictionary spelling.
        let entry: Uuid = serde_json::from_value(entries[0].clone()).unwrap();
        let publication = Uuid::now_v7();
        sqlx::query("INSERT INTO lexicon_next.publications(id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot) VALUES ($1,$2,1,1,$3,'{}')").bind(publication).bind(entry).bind(actor).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO learning_source.reviewed_publications(id,entry_id,snapshot) VALUES($1,$2,'{}')").bind(publication).bind(entry).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO learning_source.next_choice_candidates(publication_id,entry_id,pos_id,sense_id,definition_id,form_id,answer_profile,level,part_of_speech,spelling,definition,accepted_keys,definition_keys) VALUES ($1,$2,gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),'BrE','A1','noun','alpha','甲',ARRAY['alpha'],ARRAY['甲'])").bind(publication).bind(entry).execute(&pool).await.unwrap();
        let dataset:i64=sqlx::query_scalar("INSERT INTO dictionary.datasets(version,source_name,source_version,rules_version,terms_sha256,regions_sha256,status) VALUES('purge','Kaikki','test','test','test','test','active') RETURNING id").fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO dictionary.entry_contents(dataset_id,source_key,normalized_term,pos,senses,source_locator) VALUES($1,'alpha','alpha','noun','[]','test')").bind(dataset).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO dictionary.choice_semantics(dataset_id,source_key,normalized_term,pos,domains,equivalents,antonyms,meanings) VALUES($1,'alpha','alpha','noun','{}','{}','{}','[]')").bind(dataset).execute(&pool).await.unwrap();
        let command = |action, ids: &[Value], lifecycle| {
            json!({"action":action,"request_id":Uuid::now_v7(),
            "items":ids.iter().map(|id| json!({"entry_id":id,"base_revision":1,"base_lifecycle_revision":lifecycle})).collect::<Vec<_>>()})
        };
        let mixed = command("archive", &entries, 1);
        let exclusive = command("archive", &entries[..1], 3);
        for request in [
            mixed.clone(),
            command("restore", &entries[..1], 2),
            exclusive,
        ] {
            let (status, result) = call("workspace/commands", request).await;
            assert!(status.is_success(), "{result}");
        }
        let input = json!({"request_id":Uuid::now_v7(),"entry_ids":&entries[..1]});
        let (a, b) = tokio::join!(
            call("test-resources/purge", input.clone()),
            call("test-resources/purge", input.clone())
        );
        let receipts: Vec<Uuid> = sqlx::query_scalar(
            "SELECT request_id FROM lexicon_next.list_commands WHERE actor_id=$1",
        )
        .bind(actor)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            (a.0, b.0, receipts.len()),
            (StatusCode::OK, StatusCode::OK, 1),
            "{a:?} {b:?} {receipts:?}"
        );
        assert_eq!(receipts[0].to_string(), mixed["request_id"]);
        let retained:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM dictionary.entry_contents WHERE dataset_id=$1),(SELECT count(*) FROM dictionary.choice_semantics WHERE dataset_id=$1)").bind(dataset).fetch_one(&pool).await.unwrap();
        assert_eq!(retained, (1, 0));
        assert_eq!(a, b);
        assert_eq!(a, call("test-resources/purge", input).await);
    }

    #[tokio::test]
    async fn header_requires_explicit_configuration_matching_realm_and_valid_batch() {
        let (pairs, storage, actor) = fixture();
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/not_connected")
            .unwrap();
        let mut state = AppState::for_test(pool);
        let mut headers = HeaderMap::new();
        assert!(
            scoped_state(&state, &headers, actor, true)
                .unwrap()
                .1
                .is_none()
        );
        headers.insert(HEADER, "fixture-20261017".parse().unwrap());
        assert_eq!(
            scoped_state(&state, &headers, actor, true)
                .err()
                .unwrap()
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        let config = TestResourcesConfig::from_pairs(&pairs, &storage)
            .unwrap()
            .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("test-secret"));
        assert!(!debug.contains("normal-secret"));
        state.object_storage = config.registry(None).unwrap();
        state.test_resources = Some(Arc::new(config));
        for (id, admin) in [(Uuid::now_v7(), true), (actor, false)] {
            assert_eq!(
                scoped_state(&state, &headers, id, admin)
                    .err()
                    .unwrap()
                    .into_response()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        let (scoped, run) = scoped_state(&state, &headers, actor, true).unwrap();
        assert_eq!(run.as_deref(), Some("fixture-20261017"));
        let store = scoped
            .object_storage
            .get(&"speech".parse().unwrap())
            .unwrap();
        let key = store
            .qualify_key(crate::platform::storage::ObjectKey::parse("previews/a.mp3").unwrap())
            .unwrap();
        assert_eq!(
            key.as_str(),
            "temporary-tests/fixture-20261017/speech/previews/a.mp3"
        );
        let signed = store.presign_read(&key).await.unwrap();
        assert!(signed.url().starts_with("https://tshb-test-assets.oss-cn-heyuan.aliyuncs.com/temporary-tests/fixture-20261017/speech/previews/a.mp3?"));
        let normal = state
            .object_storage
            .get(&"speech".parse().unwrap())
            .unwrap();
        assert!(
            normal
                .presign_read(
                    &crate::platform::storage::ObjectKey::parse("previews/a.mp3").unwrap()
                )
                .await
                .unwrap()
                .url()
                .contains("/speech/previews/a.mp3?")
        );
        headers.insert(HEADER, "../escape".parse().unwrap());
        assert_eq!(
            scoped_state(&state, &headers, actor, true)
                .err()
                .unwrap()
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        headers.insert(HEADER, "fixture-20261017".parse().unwrap());
        headers.append(HEADER, "another-20261017".parse().unwrap());
        assert_eq!(
            scoped_state(&state, &headers, actor, true)
                .err()
                .unwrap()
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn test_configuration_fails_closed_and_does_not_accept_arbitrary_bucket_or_root() {
        let (pairs, storage, _) = fixture();
        assert!(
            TestResourcesConfig::from_pairs(&[], &storage)
                .unwrap()
                .is_none()
        );
        for field in [
            "TEST_RESOURCES_OSS_ACCESS_KEY_SECRET",
            "TEST_RESOURCES_ADMIN_IDS",
        ] {
            let incomplete = pairs
                .iter()
                .filter(|(k, _)| k != field)
                .cloned()
                .collect::<Vec<_>>();
            assert!(TestResourcesConfig::from_pairs(&incomplete, &storage).is_err());
        }
        for field in ["TEST_RESOURCES_OSS_BUCKET", "TEST_RESOURCES_OSS_ROOT"] {
            let mut bad = pairs.clone();
            bad.push((field.into(), "anything".into()));
            assert!(TestResourcesConfig::from_pairs(&bad, &storage).is_err());
        }
    }
}
