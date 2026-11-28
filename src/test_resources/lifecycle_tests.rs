use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{Method, Request, StatusCode, Uri},
};
use http_body_util::BodyExt;
use opendal::{Operator, services::Oss};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::{HEADER, TestResourcesConfig};
use crate::{
    platform::storage::ObjectStorageConfig,
    speech::{
        audio_updates,
        dubbing::replace_dependencies,
        library::{
            VoiceLibraryRepository,
            dto::{DubbingGroup, SaveDubbingGroupRequest},
        },
    },
    state::AppState,
};

const RUN: &str = "lifecycle-fixture";
const OTHER_RUN: &str = "lifecycle-neighbor";
const PURGE: &str = "/test-resources/purge";
const ENDPOINT: &str = "https://oss-cn-heyuan.aliyuncs.com";

async fn admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO admins(id,phone,display_name,password_hash,role,must_change_password) VALUES($1,$2,'Lifecycle test','unused','super_admin',false)")
        .bind(id).bind(format!("lifecycle-{id}")).execute(pool).await.unwrap();
    id
}

fn app(pool: &PgPool, actors: &[Uuid], operator: Operator) -> AppState {
    let mut pairs = vec![(
        "OBJECT_STORAGE_SPACES".to_owned(),
        "audio,speech".to_owned(),
    )];
    for space in ["audio", "speech"] {
        let root = format!("/{space}");
        pairs.extend(
            [
                ("BACKEND", "oss"),
                ("OSS_ENDPOINT", ENDPOINT),
                ("OSS_REGION", "cn-heyuan"),
                ("OSS_BUCKET", "tshb-test-assets"),
                ("OSS_ROOT", root.as_str()),
                ("OSS_ACCESS_KEY_ID", "fixture-id"),
                ("OSS_ACCESS_KEY_SECRET", "fixture-secret"),
                ("PRIVACY", "private"),
                ("MAX_OBJECT_SIZE_BYTES", "1024"),
                ("PRESIGN_TTL_SECONDS", "60"),
                ("CACHE_CONTROL", "none"),
            ]
            .map(|(key, value)| {
                (
                    format!("OBJECT_STORAGE_{}_{key}", space.to_uppercase()),
                    value.to_owned(),
                )
            }),
        );
    }
    let storage = ObjectStorageConfig::from_pairs(pairs).unwrap();
    let mut pairs = [
        ("OSS_ENDPOINT", ENDPOINT),
        ("OSS_REGION", "cn-heyuan"),
        ("OSS_ACCESS_KEY_ID", "test-id"),
        ("OSS_ACCESS_KEY_SECRET", "test-secret"),
    ]
    .map(|(key, value)| (format!("TEST_RESOURCES_{key}"), value.to_owned()))
    .to_vec();
    pairs.push((
        "TEST_RESOURCES_ADMIN_IDS".to_owned(),
        actors
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(","),
    ));
    let mut config = TestResourcesConfig::from_pairs(&pairs, &storage)
        .unwrap()
        .unwrap();
    config.cleanup_operator_for_test = Some(operator);
    let mut state = AppState::for_test(pool.clone());
    state.test_resources = Some(Arc::new(config));
    state.test_resource_purge_enabled = true;
    state
}

struct Client<'a> {
    state: &'a AppState,
    actor: Uuid,
    run: Option<&'a str>,
}

impl<'a> Client<'a> {
    fn new(state: &'a AppState, actor: Uuid, run: Option<&'a str>) -> Self {
        Self { state, actor, run }
    }

    async fn call(&self, method: Method, path: &str, body: Value, expected: StatusCode) -> Value {
        let token = self
            .state
            .admin_token_manager
            .generate(self.actor, "super_admin")
            .unwrap();
        let prefix = if path.starts_with("/speech/") {
            "/api/v1/admin"
        } else {
            "/api/v1/admin/lexicon-next"
        };
        let mut request = Request::builder()
            .method(method.clone())
            .uri(format!("{prefix}{path}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("x-tsz-next-sentences", "v1")
            .header("x-tsz-next-sentence-details", "v1")
            .header("x-tsz-next-sentence-drafts", "v1")
            .header("x-tsz-next-dialect-pairs", "v1")
            .header("x-tsz-voice-versions", "v6")
            .header("x-tsz-voice-locales", "v1")
            .header("x-tsz-voice-models", "v1");
        if let Some(run) = self.run {
            request = request.header(HEADER, run);
        }
        let result = crate::router(self.state.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = result.status();
        let bytes = result.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            status,
            expected,
            "{method} {path}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn entry_input(word: &str) -> Value {
    json!({"target_language":"en","kind":"word","headword":word})
}

fn sentence_variant(text: &str) -> Value {
    // Same sentence request shape and negotiated capabilities as tests/lexicon_next.rs.
    json!({
        "id":Uuid::now_v7(),"locale":"en-GB","level":"A2",
        "content":{"version":2,"text":text,"annotations":[]},
        "translations":[{"id":Uuid::now_v7(),"locale":"zh-Hans","style":"balanced_fluency",
            "content":{"version":2,"text":"例句译文","annotations":[]}}]
    })
}

fn sentence_input(id: Uuid, variant: Value) -> Value {
    json!({"id":id,"target_language":"en","target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant})
}

fn draft(word: &str, sense: Uuid, sentences: &[Uuid]) -> Value {
    json!({"schema_version":1,"headword":word,"forms":[],"senses":[{
        "id":sense,"part_of_speech":"noun","definitions":[],"equivalents":[],"sentence_ids":sentences
    }]})
}

fn id(value: &Value) -> Uuid {
    serde_json::from_value(value["id"].clone()).unwrap()
}

fn purge_input(entries: &[Uuid], sentences: &[Uuid]) -> Value {
    json!({"request_id":Uuid::now_v7(),"entry_ids":entries,"sentence_ids":sentences})
}

#[derive(Default)]
struct Bucket {
    // Object key (including the OSS root), version ID, and XML record kind.
    versions: Vec<(String, &'static str, &'static str)>,
    blocked_version: Option<&'static str>,
    deletes: Vec<(String, String)>,
}

async fn oss(
    State(bucket): State<Arc<Mutex<Bucket>>>,
    method: Method,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
) -> (StatusCode, String) {
    let mut bucket = bucket.lock().unwrap();
    let path = uri.path().trim_start_matches('/');
    if method == Method::PUT {
        bucket.versions.push((path.to_owned(), "put", "Version"));
        return (StatusCode::OK, String::new());
    }
    if method == Method::DELETE {
        let Some(version) = query.get("versionId") else {
            bucket
                .versions
                .push((path.to_owned(), "delete", "DeleteMarker"));
            return (StatusCode::NO_CONTENT, String::new());
        };
        bucket.deletes.push((path.to_owned(), version.clone()));
        if bucket.blocked_version == Some(version.as_str()) {
            return (
                StatusCode::FORBIDDEN,
                "<Error><Code>AccessDenied</Code></Error>".to_owned(),
            );
        }
        bucket
            .versions
            .retain(|(key, id, _)| key != path || *id != version.as_str());
        return (StatusCode::NO_CONTENT, String::new());
    }
    assert_eq!(method, Method::GET);
    assert!(query.contains_key("versions"));
    let prefix = &query["prefix"];
    let mut xml = String::from("<ListVersionsResult><IsTruncated>false</IsTruncated>");
    for (key, version, kind) in &bucket.versions {
        if key.starts_with(prefix) {
            let size = if *kind == "Version" {
                "<Size>2</Size>"
            } else {
                ""
            };
            xml.push_str(&format!("<{kind}><Key>{key}</Key><VersionId>{version}</VersionId><IsLatest>false</IsLatest>{size}<LastModified>2026-11-26T00:00:00Z</LastModified></{kind}>"));
        }
    }
    xml.push_str("</ListVersionsResult>");
    (StatusCode::OK, xml)
}

async fn versioned_bucket() -> (Operator, Arc<Mutex<Bucket>>) {
    let bucket = Arc::new(Mutex::new(Bucket::default()));
    let router = Router::new().fallback(oss).with_state(bucket.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    opendal::install_default();
    let operator = Operator::new(
        Oss::default()
            .bucket("tshb-test-assets")
            .root("/temporary-tests")
            .endpoint(&endpoint)
            .addressing_style("cname")
            .access_key_id("test-id")
            .access_key_secret("test-secret"),
    )
    .unwrap();
    (operator, bucket)
}

#[sqlx::test]
async fn direct_and_inline_sentences_require_their_exact_batch_to_purge(pool: PgPool) {
    let actor = admin(&pool).await;
    let second = admin(&pool).await;
    let normal = admin(&pool).await;
    let (operator, _) = versioned_bucket().await;
    let state = app(&pool, &[actor, second], operator);
    let owner = Client::new(&state, actor, Some(RUN));
    let no_header = Client::new(&state, actor, None);
    let other_batch = Client::new(&state, actor, Some(OTHER_RUN));
    let other_owner = Client::new(&state, second, Some(RUN));
    let not_allowed = Client::new(&state, normal, Some(RUN));
    let word = "centre";
    let direct = Uuid::now_v7();
    let inline = Uuid::now_v7();
    let foreign = Uuid::now_v7();
    let sentence = sentence_input(direct, sentence_variant("A direct example."));
    for (path, body) in [
        ("/entries", entry_input(word)),
        ("/sentences", sentence.clone()),
    ] {
        no_header
            .call(Method::POST, path, body, StatusCode::FORBIDDEN)
            .await;
    }
    not_allowed
        .call(
            Method::POST,
            "/sentences",
            sentence.clone(),
            StatusCode::FORBIDDEN,
        )
        .await;
    let entry = owner
        .call(
            Method::POST,
            "/entries",
            entry_input(word),
            StatusCode::CREATED,
        )
        .await;
    let entry_id = id(&entry);
    let entry_path = format!("/entries/{entry_id}");
    let direct_path = format!("/sentences/{direct}");
    let created = owner
        .call(Method::POST, "/sentences", sentence, StatusCode::CREATED)
        .await;
    let mut variant = created["draft"]["variants"][0].clone();
    variant["content"]["text"] = json!("An edited direct example.");
    let update = json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant});
    for (writer, run, status) in [
        (actor, None, StatusCode::FORBIDDEN),
        (actor, Some(OTHER_RUN), StatusCode::CONFLICT),
        (second, Some(RUN), StatusCode::CONFLICT),
        (actor, Some(RUN), StatusCode::OK),
    ] {
        Client::new(&state, writer, run)
            .call(Method::PUT, &direct_path, update.clone(), status)
            .await;
    }
    other_batch
        .call(
            Method::POST,
            "/sentences",
            sentence_input(foreign, sentence_variant("A different batch.")),
            StatusCode::CREATED,
        )
        .await;

    let sense = Uuid::now_v7();
    let inline_change = json!({"id":inline,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":sentence_variant("An inline example.")});
    let save = |sentences: &[Uuid]| json!({"expected_revision":1,"draft":draft(word, sense, sentences),"sentence_changes":[inline_change]});
    // A foreign reference rejects the entire save, including the new inline sentence.
    owner
        .call(
            Method::PUT,
            &entry_path,
            save(&[direct, inline, foreign]),
            StatusCode::CONFLICT,
        )
        .await;
    let inline_path = format!("/sentences/{inline}");
    owner
        .call(
            Method::GET,
            &inline_path,
            Value::Null,
            StatusCode::NOT_FOUND,
        )
        .await;
    let save = save(&[direct, inline]);
    no_header
        .call(
            Method::PUT,
            &entry_path,
            save.clone(),
            StatusCode::FORBIDDEN,
        )
        .await;
    other_batch
        .call(Method::PUT, &entry_path, save.clone(), StatusCode::CONFLICT)
        .await;
    let saved = owner
        .call(Method::PUT, &entry_path, save, StatusCode::OK)
        .await;
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["sentence_resources"].as_array().unwrap().len(), 2);
    let registered: Vec<Uuid> = sqlx::query_scalar("SELECT sentence_id FROM lexicon_next.test_resource_sentences WHERE run_id=$1 ORDER BY sentence_id")
        .bind(RUN).fetch_all(&pool).await.unwrap();
    let mut ids = vec![direct, inline];
    ids.sort_unstable();
    assert_eq!(registered, ids);

    owner.call(Method::POST,
        "/workspace/commands",
        json!({
            "request_id":Uuid::now_v7(),"action":"archive","items":[{
                "entry_id":entry_id,"base_revision":saved["revision"],"base_lifecycle_revision":1
            }]
        }),
        StatusCode::OK,
    )
    .await;
    for (entries, sentences) in [
        (vec![], ids.clone()),
        (vec![entry_id], vec![direct]),
        (vec![entry_id], vec![direct, inline, foreign]),
    ] {
        owner
            .call(
                Method::POST,
                PURGE,
                purge_input(&entries, &sentences),
                StatusCode::FORBIDDEN,
            )
            .await;
    }
    let input = purge_input(&[entry_id], &ids.iter().copied().rev().collect::<Vec<_>>());
    no_header
        .call(Method::POST, PURGE, input.clone(), StatusCode::FORBIDDEN)
        .await;
    other_owner
        .call(Method::POST, PURGE, input.clone(), StatusCode::CONFLICT)
        .await;
    let receipt = owner
        .call(Method::POST, PURGE, input.clone(), StatusCode::OK)
        .await;
    assert_eq!(receipt["entry_ids"], json!([entry_id]));
    assert_eq!(receipt["sentence_ids"], json!(ids));
    assert_eq!(
        receipt,
        owner.call(Method::POST, PURGE, input, StatusCode::OK).await
    );
    for path in [&entry_path, &direct_path, &inline_path] {
        owner
            .call(Method::GET, path, Value::Null, StatusCode::NOT_FOUND)
            .await;
    }
    other_batch
        .call(
            Method::GET,
            &format!("/sentences/{foreign}"),
            Value::Null,
            StatusCode::OK,
        )
        .await;
    let leftover: (i64, i64, i64) = sqlx::query_as(
        "SELECT
        (SELECT count(*) FROM lexicon_next.test_resource_entries WHERE run_id=$1),
        (SELECT count(*) FROM lexicon_next.test_resource_sentences WHERE run_id=$1),
        (SELECT count(*) FROM lexicon_next.sentence_references WHERE sentence_id=ANY($2))",
    )
    .bind(RUN)
    .bind(&ids)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leftover, (0, 0, 0));
}

#[sqlx::test]
async fn purge_refuses_external_live_and_historical_sentence_references(pool: PgPool) {
    let actor = admin(&pool).await;
    let normal = admin(&pool).await;
    let (operator, bucket) = versioned_bucket().await;
    let state = app(&pool, &[actor], operator);
    let owner = Client::new(&state, actor, Some(RUN));
    let outside = Client::new(&state, normal, None);
    let sentence = Uuid::now_v7();
    let created = owner
        .call(
            Method::POST,
            "/sentences",
            sentence_input(sentence, sentence_variant("An externally cited example.")),
            StatusCode::CREATED,
        )
        .await;
    let external = outside
        .call(
            Method::POST,
            "/entries",
            entry_input("outside"),
            StatusCode::CREATED,
        )
        .await;
    let external_id = id(&external);
    let sense = Uuid::now_v7();
    let path = format!("/entries/{external_id}");
    outside
        .call(
            Method::PUT,
            &path,
            json!({"expected_revision":1,"draft":draft("outside", sense, &[sentence])}),
            StatusCode::CONFLICT,
        )
        .await;
    let saved = outside
        .call(
            Method::PUT,
            &path,
            json!({"expected_revision":1,"draft":draft("outside", sense, &[])}),
            StatusCode::OK,
        )
        .await;

    // Legacy references predate write-time isolation, but must still prevent destructive cleanup.
    sqlx::query("INSERT INTO lexicon_next.sentence_references(entry_id,sense_id,sentence_id,position) VALUES($1,$2,$3,0)")
        .bind(external_id).bind(sense).bind(sentence).execute(&pool).await.unwrap();
    let input = purge_input(&[], &[sentence]);
    owner
        .call(Method::POST, PURGE, input.clone(), StatusCode::CONFLICT)
        .await;
    sqlx::query(
        "DELETE FROM lexicon_next.sentence_references WHERE entry_id=$1 AND sentence_id=$2",
    )
    .bind(external_id)
    .bind(sentence)
    .execute(&pool)
    .await
    .unwrap();
    // The latest snapshot and current draft no longer contain the sentence; only history does.
    let older = Uuid::now_v7();
    let mut historical_draft = saved["draft"].clone();
    historical_draft["senses"][0]["sentence_ids"] = json!([sentence]);
    sqlx::query("INSERT INTO lexicon_next.publications(id,entry_id,revision,lifecycle_revision,published_by_admin_id,snapshot)
        VALUES($1,$2,2,2,$3,$4),(gen_random_uuid(),$2,2,3,$3,$5)")
        .bind(older).bind(external_id).bind(normal)
        .bind(json!({"draft":historical_draft,"sentence_resources":[created],"relation_targets":[]}))
        .bind(json!({"draft":saved["draft"],"sentence_resources":[],"relation_targets":[]}))
        .execute(&pool).await.unwrap();
    owner
        .call(Method::POST, PURGE, input, StatusCode::CONFLICT)
        .await;
    let (sealed, retained): (Option<Uuid>, i64) = sqlx::query_as(
        "SELECT purge_request_id,
        (SELECT count(*) FROM lexicon_next.test_resource_sentences WHERE sentence_id=$2)
        FROM lexicon_next.test_resource_runs WHERE run_id=$1",
    )
    .bind(RUN)
    .bind(sentence)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((sealed, retained), (None, 1));
    assert!(bucket.lock().unwrap().deletes.is_empty());
}

struct Provider(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl crate::speech::SpeechProvider for Provider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        _: &crate::speech::SynthesisRequest,
    ) -> Result<crate::speech::SynthesizedAudio, crate::speech::SpeechError> {
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
            return Err(crate::speech::SpeechError::new(
                crate::speech::SpeechErrorKind::Unavailable,
                None,
            ));
        }
        Ok(crate::speech::SynthesizedAudio {
            bytes: b"ID3-local-test".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn generated_renditions_and_failed_request_are_recovered_from_one_batch(pool: PgPool) {
    let actor = admin(&pool).await;
    let (operator, bucket) = versioned_bucket().await;
    let mut state = app(&pool, &[actor], operator);
    let provider = Arc::new(Provider(std::sync::atomic::AtomicUsize::new(0)));
    state.speech_provider = Some(provider.clone());
    sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version)
        VALUES(gen_random_uuid(),'sonia','azure','en-GB-SoniaNeural','en-GB','female','[]','v1')")
        .execute(&pool).await.unwrap();

    let owner = Client::new(&state, actor, Some(RUN));
    let other = Client::new(&state, actor, Some(OTHER_RUN));
    let text = "An example with audio.";
    let mut variant = sentence_variant(text);
    let mut setting = json!({"voice_id":"sonia","version_id":Uuid::now_v7(),"version_name":"Sonia","enabled":true,"locale":"en-GB","rate_percent":0});
    let request = |setting: &Value, content: &Value| json!({"request_id":Uuid::now_v7(),"source":{"kind":"text","content":content},"setting":setting});
    let path = "/speech/audio";
    let first = request(&setting, &variant["content"]);
    let result = owner
        .call(Method::POST, path, first.clone(), StatusCode::CREATED)
        .await;
    assert_eq!(
        result,
        owner
            .call(Method::POST, path, first.clone(), StatusCode::CREATED)
            .await
    );
    other
        .call(Method::POST, path, first.clone(), StatusCode::CONFLICT)
        .await;
    Client::new(&state, actor, None)
        .call(Method::POST, path, first, StatusCode::FORBIDDEN)
        .await;

    let old = id(&result["asset"]);
    setting["audio_asset_id"] = json!(old);
    variant["voice_profile"] = json!({"voices":[setting]});
    let sentence = Uuid::now_v7();
    owner
        .call(
            Method::POST,
            "/sentences",
            sentence_input(sentence, variant.clone()),
            StatusCode::CREATED,
        )
        .await;
    let replacement = owner
        .call(
            Method::POST,
            path,
            request(&setting, &variant["content"]),
            StatusCode::CREATED,
        )
        .await;
    let new = id(&replacement["asset"]);
    assert_ne!(old, new);
    setting["audio_asset_id"] = json!(new);
    variant["voice_profile"] = json!({"voices":[setting]});
    let sentence_path = format!("/sentences/{sentence}");
    owner.call(Method::PUT, &sentence_path, json!({"expected_revision":1,"target_locale":"en-GB","medium_locale":"zh-Hans","variant":variant}), StatusCode::OK).await;

    let changed = sentence_variant("A separately failing rendition.");
    let failed = request(&setting, &changed["content"]);
    owner
        .call(Method::POST, path, failed, StatusCode::SERVICE_UNAVAILABLE)
        .await;
    // The text-only write still succeeds after a failed formal rendition.
    let text_only = Uuid::now_v7();
    owner
        .call(
            Method::POST,
            "/sentences",
            sentence_input(text_only, changed),
            StatusCode::CREATED,
        )
        .await;
    let before: Vec<(Uuid, String)> = sqlx::query_as("SELECT asset_id,state FROM lexicon.audio_generations WHERE test_run_id=$1 ORDER BY asset_id")
        .bind(RUN).fetch_all(&pool).await.unwrap();
    assert_eq!(before.len(), 3);
    assert_eq!(
        (&before[0].1[..], &before[1].1[..], &before[2].1[..]),
        ("ready", "ready", "failed")
    );
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(bucket.lock().unwrap().versions.len(), 3);

    let receipt = owner
        .call(
            Method::POST,
            PURGE,
            purge_input(&[], &[sentence, text_only]),
            StatusCode::OK,
        )
        .await;
    assert_eq!(
        receipt["audio_asset_ids"],
        json!(before.iter().map(|(id, _)| id).collect::<Vec<_>>())
    );
    assert!(bucket.lock().unwrap().versions.is_empty());
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_generations WHERE test_run_id=$1")
            .bind(RUN)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retained, 0);
}

#[sqlx::test]
async fn purge_keeps_registrations_on_oss_failure_and_retries_original_empty_selection(
    pool: PgPool,
) {
    let actor = admin(&pool).await;
    let (operator, bucket) = versioned_bucket().await;
    let state = app(&pool, &[actor], operator);
    let owner = Client::new(&state, actor, Some(RUN));
    let asset = Uuid::now_v7();
    let generation = Uuid::now_v7();
    let asset_key = format!("temporary-tests/{RUN}/audio/assets/generated/{asset}.mp3");
    let preview_key = format!("temporary-tests/{RUN}/speech/previews/clip.mp3");
    let stale_key = format!("temporary-tests/{RUN}/speech/previews/stale.mp3");
    let normal_key = "previews/normal.mp3";
    let other_key = format!("temporary-tests/{OTHER_RUN}/speech/previews/clip.mp3");
    sqlx::query(
        "INSERT INTO lexicon_next.test_resource_runs(run_id,actor_id) VALUES($1,$2),($3,$2)",
    )
    .bind(RUN)
    .bind(actor)
    .bind(OTHER_RUN)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id)
        VALUES($1,$2,$3,'audio/mpeg',1,'en-GB','female','voice.mp3',$4)")
        .bind(asset).bind(&asset_key).bind(format!("generated/{actor}/{generation}"))
        .bind(actor).execute(&pool).await.unwrap();
    // Simulate a provider failure that left a discoverable asset row and a versioned object.
    sqlx::query("INSERT INTO lexicon.audio_generations(asset_id,request_id,created_by_admin_id,input_hash,source,voice,resolved_content,state,test_run_id)
        VALUES($1,$2,$3,$4,'{}','{}','{}','failed',$5)")
        .bind(asset).bind(generation).bind(actor).bind(vec![1u8; 32]).bind(RUN).execute(&pool).await.unwrap();
    let voice: Uuid = sqlx::query_scalar("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version)
        VALUES(gen_random_uuid(),'sonia','azure','en-GB-SoniaNeural','en-GB','female','[]','v1') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    // Unique fingerprints keep Redis fixture keys isolated even when the tests share a server.
    let [preview_hash, other_hash, normal_hash] =
        ["preview", "other", "normal"].map(|kind| Sha256::digest(format!("{generation}:{kind}")));
    let lock_keys = [&preview_hash, &other_hash, &normal_hash].map(|hash| {
        let hex = hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("");
        format!("speech:preview:lock:{hex}")
    });
    let mut redis = state
        .redis
        .get()
        .await
        .expect("connect using TEST_REDIS_URL");
    for key in &lock_keys {
        let _: () = redis::cmd("SETEX")
            .arg(key)
            .arg(60)
            .arg("fixture")
            .query_async(&mut redis)
            .await
            .unwrap();
    }
    for (run, key, hash) in [
        (RUN, &preview_key, &preview_hash),
        (RUN, &stale_key, &preview_hash),
        (OTHER_RUN, &other_key, &other_hash),
    ] {
        sqlx::query("INSERT INTO lexicon_next.test_resource_previews(object_key,run_id,request_hash) VALUES($1,$2,$3)")
            .bind(key).bind(run).bind(hash.as_slice()).execute(&pool).await.unwrap();
    }
    for (key, hash) in [
        (preview_key.as_str(), &preview_hash),
        (other_key.as_str(), &other_hash),
        (normal_key, &normal_hash),
    ] {
        sqlx::query("INSERT INTO speech.preview_cache(request_hash,voice_id,content_hash,object_key,mime_type,size_bytes,expires_at)
            VALUES($1,$2,$3,$4,'audio/mpeg',1,now()+interval '1 day')")
            .bind(hash.as_slice()).bind(voice).bind(vec![5u8; 32]).bind(key).execute(&pool).await.unwrap();
    }
    let outside = [
        (format!("speech/{normal_key}"), "normal", "Version"),
        (other_key.clone(), "other", "Version"),
    ];
    {
        let mut bucket = bucket.lock().unwrap();
        bucket.versions = vec![
            (asset_key.clone(), "asset", "Version"),
            (preview_key.clone(), "preview-old", "Version"),
            (preview_key.clone(), "preview-new", "Version"),
            (preview_key.clone(), "marker", "DeleteMarker"),
            (stale_key, "stale", "Version"),
        ];
        bucket.versions.extend(outside.clone());
        bucket.blocked_version = Some("marker");
    }
    let input = purge_input(&[], &[]);
    let command_id: Uuid = serde_json::from_value(input["request_id"].clone()).unwrap();
    let write = super::start_run_write(&pool, actor, Some(RUN))
        .await
        .unwrap()
        .unwrap();
    owner
        .call(Method::POST, PURGE, input.clone(), StatusCode::CONFLICT)
        .await;
    assert!(bucket.lock().unwrap().deletes.is_empty());
    write.rollback().await.unwrap();
    owner
        .call(
            Method::POST,
            PURGE,
            input.clone(),
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
    {
        let mut bucket = bucket.lock().unwrap();
        assert!(
            bucket
                .deletes
                .contains(&(preview_key.clone(), "marker".to_owned()))
        );
        assert!(
            bucket
                .versions
                .contains(&(preview_key.clone(), "marker", "DeleteMarker"))
        );
        bucket.blocked_version = None;
    }
    let registrations = || async {
        sqlx::query_as::<_, (i64, i64, i64, i64, i64)>("SELECT
            (SELECT count(*) FROM lexicon.audio_assets WHERE id=$2),
            (SELECT count(*) FROM lexicon.audio_generations WHERE asset_id=$2 AND test_run_id=$1 AND state='failed'),
            (SELECT count(*) FROM lexicon_next.test_resource_previews WHERE run_id=$1),
            (SELECT count(*) FROM speech.preview_cache WHERE object_key=$3),
            (SELECT count(*) FROM lexicon_next.test_resource_purge_commands WHERE request_id=$4)")
            .bind(RUN).bind(asset).bind(&preview_key).bind(command_id).fetch_one(&pool).await.unwrap()
    };
    assert_eq!(registrations().await, (1, 1, 2, 1, 0));
    let exists: bool = redis::cmd("EXISTS")
        .arg(&lock_keys[0])
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!(exists);
    owner
        .call(
            Method::POST,
            "/sentences",
            sentence_input(Uuid::now_v7(), sentence_variant("Too late for this batch.")),
            StatusCode::CONFLICT,
        )
        .await;
    owner
        .call(
            Method::POST,
            PURGE,
            purge_input(&[], &[]),
            StatusCode::CONFLICT,
        )
        .await;
    Client::new(&state, actor, Some(OTHER_RUN))
        .call(Method::POST, PURGE, input.clone(), StatusCode::CONFLICT)
        .await;
    let receipt = owner
        .call(Method::POST, PURGE, input.clone(), StatusCode::OK)
        .await;
    assert_eq!(receipt["entry_ids"], json!([]));
    assert_eq!(receipt["sentence_ids"], json!([]));
    assert_eq!(receipt["audio_asset_ids"], json!([asset]));
    assert_eq!(registrations().await, (0, 0, 0, 0, 1));
    let surviving_cache: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM speech.preview_cache WHERE object_key=ANY($1) ORDER BY object_key",
    )
    .bind(vec![normal_key.to_owned(), other_key.clone()])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(surviving_cache, [normal_key.to_owned(), other_key]);
    let exists: bool = redis::cmd("EXISTS")
        .arg(&lock_keys[0])
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!(!exists);
    let preserved: i64 = redis::cmd("DEL")
        .arg(&lock_keys[1..])
        .query_async(&mut redis)
        .await
        .unwrap();
    assert_eq!(preserved, 2);
    let deletes = bucket.lock().unwrap().deletes.len();
    assert_eq!(
        receipt,
        owner.call(Method::POST, PURGE, input, StatusCode::OK).await
    );
    let bucket = bucket.lock().unwrap();
    assert_eq!(bucket.versions, outside);
    assert_eq!(bucket.deletes.len(), deletes);
}

fn dubbing_group(id: Uuid, version: Uuid) -> DubbingGroup {
    serde_json::from_value(json!({"id":id,"name":"examples","areas":["example"],"revision":0,"is_global":false,
        "uk":[{"id":version,"name":"British","voice_alias":"sonia","selected":true,"parameters":{"rate_percent":-12,"pitch_semitones":0,"style":null}}],
        "us":[{"id":Uuid::from_u128(version.as_u128()+1),"name":"American","voice_alias":"jenny","selected":true,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}]}))
        .unwrap()
}

#[sqlx::test]
async fn dialect_pairing_rejects_a_target_from_another_batch(pool: PgPool) {
    let actor = admin(&pool).await;
    let (operator, _) = versioned_bucket().await;
    let state = app(&pool, &[actor], operator);
    let owner = Client::new(&state, actor, Some(RUN));
    let other = Client::new(&state, actor, Some(OTHER_RUN));
    let source = id(&owner
        .call(
            Method::POST,
            "/entries",
            entry_input("centre"),
            StatusCode::CREATED,
        )
        .await);
    let target = id(&other
        .call(
            Method::POST,
            "/entries",
            entry_input("center"),
            StatusCode::CREATED,
        )
        .await);
    let source_sense = Uuid::now_v7();
    let target_sense = Uuid::now_v7();
    let saved_source = owner
        .call(
            Method::PUT,
            &format!("/entries/{source}"),
            json!({"expected_revision":1,"draft":draft("centre",source_sense,&[])}),
            StatusCode::OK,
        )
        .await;
    let saved_target = other
        .call(
            Method::PUT,
            &format!("/entries/{target}"),
            json!({"expected_revision":1,"draft":draft("center",target_sense,&[])}),
            StatusCode::OK,
        )
        .await;
    let pairing = json!({"expected_revision":saved_source["revision"],
        "draft":draft("centre",source_sense,&[]),
        "dialect_changes":[{"sense_id":source_sense,"usage":"en-GB","no_equivalent":false,"note":"",
            "target":{"entry_id":target,"sense_id":target_sense,"revision":saved_target["revision"]},
            "previous_target_revision":null}]});
    owner
        .call(
            Method::PUT,
            &format!("/entries/{source}"),
            pairing,
            StatusCode::CONFLICT,
        )
        .await;
    let pairs: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon_next.dialect_pairs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pairs, 0);
}

#[sqlx::test]
async fn worker_retry_reuses_scoped_generation_after_content_rollback(pool: PgPool) {
    let actor = admin(&pool).await;
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) SELECT $1,unnest(ARRAY['words.access','words.edit','speech.generate','speech.global_dubbing','sentences.access','sentences.edit']),$1")
        .bind(actor).execute(&pool).await.unwrap();
    for (alias, provider, locale) in [
        ("sonia", "en-GB-SoniaNeural", "en-GB"),
        ("jenny", "en-US-JennyNeural", "en-US"),
    ] {
        sqlx::query("INSERT INTO speech.voices(id,alias,provider,provider_voice_id,locale,gender,styles,provider_version) VALUES($1,$2,'azure',$3,$4,'female','[]','v1')")
            .bind(Uuid::now_v7()).bind(alias).bind(provider).bind(locale).execute(&pool).await.unwrap();
    }
    let (operator, bucket) = versioned_bucket().await;
    let mut state = app(&pool, &[actor], operator);
    let provider = Arc::new(Provider(std::sync::atomic::AtomicUsize::new(0)));
    state.speech_provider = Some(provider.clone());
    let group = Uuid::now_v7();
    let version = Uuid::now_v7();
    let sentence = Uuid::now_v7();
    let target = Uuid::now_v7();
    let original = json!({"mode":"distinguish","source_dialect":"uk","uk":{"state":"ready","variant":{"id":target,"origin":"manual","value":{"version":2,"text":"Hello","annotations":[]},"voice_profile":{"voices":[],"dubbing":{"area":"example","uk":{"mode":"inherit"},"us":{"mode":"inherit"}}}}},"us":{"state":"missing"}});
    sqlx::query("INSERT INTO lexicon.shared_sentences(id,content,create_digest,created_by_admin_id) VALUES($1,$2,'fixture',$3)")
        .bind(sentence).bind(json!({"id":sentence,"level":"A1","en_text":original,"links":[]})).bind(actor).execute(&pool).await.unwrap();
    replace_dependencies(
        &mut pool.acquire().await.unwrap(),
        "sentence",
        sentence,
        1,
        &original,
    )
    .await
    .unwrap();
    VoiceLibraryRepository::new(pool.clone())
        .save_group(
            actor,
            0,
            SaveDubbingGroupRequest {
                group: dubbing_group(group, version),
                base_revision: 0,
            },
            true,
        )
        .await
        .unwrap();
    let update: Uuid =
        sqlx::query_scalar("SELECT id FROM speech.dubbing_audio_updates WHERE group_id=$1")
            .bind(group)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE speech.dubbing_audio_updates SET test_run=$2,due_at=now()-interval '1 second' WHERE id=$1")
        .bind(update).bind(RUN).execute(&pool).await.unwrap();
    audio_updates::sweep_in(&pool, Some(provider.clone()), None, Some(&state))
        .await
        .unwrap();
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (asset, run, key): (Uuid, Option<String>, String) = sqlx::query_as(
        "SELECT a.id,g.test_run_id,a.object_key FROM lexicon.audio_generations g JOIN lexicon.audio_assets a ON a.id=g.asset_id WHERE g.created_by_admin_id=$1",
    ).bind(actor).fetch_one(&pool).await.unwrap();
    assert_eq!(run.as_deref(), Some(RUN));
    assert!(
        key.starts_with(&format!("temporary-tests/{RUN}/audio/assets/generated/")),
        "{key}"
    );
    // Simulate a crash after synthesis but before the sentence draft was committed.
    sqlx::query("UPDATE lexicon.shared_sentences SET content=$2 WHERE id=$1")
        .bind(sentence)
        .bind(json!({"id":sentence,"level":"A1","en_text":original,"links":[]}))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE speech.dubbing_audio_update_targets SET state='pending',attempts=1,lease_token=NULL,lease_until=NULL,retry_at=now()-interval '1 second',error_code=NULL WHERE update_id=$1")
        .bind(update).execute(&pool).await.unwrap();
    sqlx::query("UPDATE speech.dubbing_audio_updates SET state='running',due_at=now()-interval '1 second' WHERE id=$1")
        .bind(update).execute(&pool).await.unwrap();
    audio_updates::sweep_in(&pool, Some(provider.clone()), None, Some(&state))
        .await
        .unwrap();
    assert_eq!(
        provider.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "ready rendition must be replayed, not synthesized or rejected"
    );
    let target_state: (String, Option<String>) = sqlx::query_as(
        "SELECT state,error_code FROM speech.dubbing_audio_update_targets WHERE update_id=$1",
    )
    .bind(update)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(target_state, ("completed".to_owned(), None));
    let stored: Uuid = sqlx::query_scalar("SELECT (content->'en_text'->'uk'->'variant'->'voice_profile'->'voices'->0->>'audio_asset_id')::uuid FROM lexicon.shared_sentences WHERE id=$1")
        .bind(sentence).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, asset);
    assert!(
        bucket
            .lock()
            .unwrap()
            .versions
            .iter()
            .any(|(key, _, _)| key.ends_with(&format!("{asset}.mp3")))
    );
}
