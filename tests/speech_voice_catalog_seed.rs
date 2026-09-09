use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;

/// 与 ops 目录里那份人工执行的种子是同一个文件，避免测试对着副本跑绿。
const SEED: &str = include_str!("../ops/speech-voice-catalog/seed.sql");

#[derive(sqlx::FromRow)]
struct VoiceRow {
    locale: String,
    gender: String,
    styles: Value,
    enabled: bool,
    updated_at: DateTime<Utc>,
}

async fn apply_seed(pool: &PgPool) {
    sqlx::raw_sql(SEED)
        .execute(pool)
        .await
        .expect("voice catalog seed should apply");
}

async fn aliases(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT alias FROM speech.voices ORDER BY alias")
        .fetch_all(pool)
        .await
        .expect("catalog read should succeed")
}

/// 按 alias 取而不是按下标：种子以后加发音人时，下标会静默指到另一行。
async fn voice(pool: &PgPool, alias: &str) -> VoiceRow {
    sqlx::query_as(
        "SELECT locale, gender, styles, enabled, updated_at FROM speech.voices WHERE alias = $1",
    )
    .bind(alias)
    .fetch_one(pool)
    .await
    .expect("voice should exist")
}

#[sqlx::test]
async fn seed_populates_both_locales_and_reruns_without_writing(pool: PgPool) {
    apply_seed(&pool).await;
    assert_eq!(
        aliases(&pool).await,
        ["en-gb-sonia", "en-us-aria", "en-us-davis"]
    );

    let sonia = voice(&pool, "en-gb-sonia").await;
    assert_eq!(sonia.locale, "en-GB");
    assert_eq!(sonia.gender, "female");
    // styles 逐个发音人取自 Azure；Sonia 只有这两个，照抄 Aria 会宣称不存在的能力。
    assert_eq!(sonia.styles, json!(["cheerful", "sad"]));
    assert!(sonia.enabled, "seed 出来的发音人默认可用");

    apply_seed(&pool).await;
    assert_eq!(aliases(&pool).await.len(), 3, "重跑不得重复建行");
    assert_eq!(
        voice(&pool, "en-gb-sonia").await.updated_at,
        sonia.updated_at,
        "目录事实没变时重跑不应该写库"
    );
}

#[sqlx::test]
async fn seed_repairs_drift_but_leaves_disabled_voices_disabled(pool: PgPool) {
    apply_seed(&pool).await;
    // davis 必须**同时**能力漂移并被停用：只改 enabled 的话种子的 WHERE 直接短路，
    // 整条 UPDATE 不执行，就测不到「触发了 UPDATE 也不写 enabled」这条保证。
    sqlx::raw_sql(
        r#"UPDATE speech.voices SET styles = '["chat"]' WHERE alias = 'en-gb-sonia';
           UPDATE speech.voices SET styles = '["chat"]', enabled = false WHERE alias = 'en-us-davis';"#,
    )
    .execute(&pool)
    .await
    .expect("drift setup should succeed");

    apply_seed(&pool).await;

    assert_eq!(
        voice(&pool, "en-gb-sonia").await.styles,
        json!(["cheerful", "sad"]),
        "漂移的能力应被收敛"
    );
    let davis = voice(&pool, "en-us-davis").await;
    assert_ne!(
        davis.styles,
        json!(["chat"]),
        "停用的行同样收敛能力，说明 UPDATE 分支确实执行了"
    );
    assert!(!davis.enabled, "运维停用的发音人不能被重跑种子悄悄启用");
}

#[sqlx::test]
async fn azure_catalog_sync_preserves_identity_and_operator_settings(pool: PgPool) {
    use tsz_rust::speech::{CatalogVoice, Voice, preview::PreviewRepository};
    apply_seed(&pool).await;
    sqlx::query("UPDATE speech.voices SET enabled = false, min_rate_percent = -10 WHERE alias = 'en-us-aria'")
        .execute(&pool).await.unwrap();
    let old_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM speech.voices WHERE alias = 'en-us-aria'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let catalog = vec![
        CatalogVoice {
            voice: Voice::new("azure", "en-US-AriaNeural", "en-US", ["chat".into()]).unwrap(),
            gender: "female".into(),
        },
        CatalogVoice {
            voice: Voice::new("azure", "en-GB-RyanNeural", "en-GB", []).unwrap(),
            gender: "male".into(),
        },
    ];
    let repository = PreviewRepository::new(pool.clone());
    let (first, second) = tokio::join!(
        repository.sync_voices(&catalog),
        repository.sync_voices(&catalog)
    );
    assert_eq!(first.unwrap(), ["en-us-aria", "en-gb-ryanneural"]);
    assert_eq!(second.unwrap(), ["en-us-aria", "en-gb-ryanneural"]);
    let aria = repository.voice_by_alias("en-us-aria").await.unwrap();
    assert!(
        aria.is_none(),
        "operator-disabled voice must remain unavailable"
    );
    let row: (uuid::Uuid, i16, Value) = sqlx::query_as(
        "SELECT id, min_rate_percent, styles FROM speech.voices WHERE alias = 'en-us-aria'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (old_id, -10, json!(["chat"])));
    let ryan = repository
        .voice_by_alias("en-gb-ryanneural")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ryan.voice.provider_voice_id(), "en-GB-RyanNeural");
    assert_eq!(ryan.voice.locale(), "en-GB");
    let updated = voice(&pool, "en-gb-ryanneural").await.updated_at;
    repository.sync_voices(&catalog).await.unwrap();
    assert_eq!(voice(&pool, "en-gb-ryanneural").await.updated_at, updated);
    assert_eq!(aliases(&pool).await.len(), 4);
    repository.sync_voices(&[]).await.unwrap();
    assert_eq!(
        aliases(&pool).await.len(),
        4,
        "historical rows must not be deleted"
    );
}

struct OfflineCatalogProvider;

#[async_trait::async_trait]
impl tsz_rust::speech::SpeechProvider for OfflineCatalogProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn list_voices(
        &self,
    ) -> Result<Option<Vec<tsz_rust::speech::CatalogVoice>>, tsz_rust::speech::SpeechError> {
        panic!("opening the product voice directory must not contact Azure")
    }
    async fn synthesize(
        &self,
        _: &tsz_rust::speech::SynthesisRequest,
    ) -> Result<tsz_rust::speech::SynthesizedAudio, tsz_rust::speech::SpeechError> {
        unreachable!()
    }
}

#[sqlx::test]
async fn product_catalog_reads_locally_when_azure_is_unavailable(pool: PgPool) {
    use tsz_rust::speech::preview::{PreviewRepository, PreviewService};
    apply_seed(&pool).await;
    let repository = PreviewRepository::new(pool.clone());
    repository.ensure_catalog_voices().await.unwrap();
    let before = voice(&pool, "en-us-aria").await.updated_at;
    repository.ensure_catalog_voices().await.unwrap();
    assert_eq!(voice(&pool, "en-us-aria").await.updated_at, before);
    assert_eq!(
        aliases(&pool).await.len(),
        tsz_rust::speech::catalog_snapshot().len()
    );
    let redis = deadpool_redis::Config::from_url("redis://127.0.0.1:1")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let service = PreviewService::new(
        PreviewRepository::new(pool.clone()),
        redis,
        Some(std::sync::Arc::new(OfflineCatalogProvider)),
        None,
    );
    let listed = service.list_voices().await.unwrap();
    assert_eq!(
        listed.items.len(),
        tsz_rust::speech::catalog_snapshot().len()
    );
    assert_eq!(listed.items.iter().filter(|v| v.is_common).count(), 8);
    assert_eq!(listed.items[0].alias, "en-gb-sonia");
    assert_eq!(listed.items[0].display_name, "Sonia 索尼娅");
    repository
        .sync_voices(&[tsz_rust::speech::CatalogVoice {
            voice: tsz_rust::speech::Voice::new("azure", "en-US-JennyNeural", "en-US", []).unwrap(),
            gender: "female".into(),
        }])
        .await
        .unwrap();
    let listed = service.list_voices().await.unwrap();
    assert_eq!(
        listed.items.len(),
        tsz_rust::speech::catalog_snapshot().len()
    );
    assert!(
        listed
            .items
            .iter()
            .any(|v| v.alias == "en-us-jennyneural" && !v.is_common && v.display_name == "Jenny")
    );
    assert!(
        repository
            .voice_by_alias("en-us-jennyneural")
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("UPDATE speech.voices SET enabled = false WHERE alias = 'en-us-aria'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        service.list_voices().await.unwrap().items.len(),
        tsz_rust::speech::catalog_snapshot().len() - 1
    );
}
