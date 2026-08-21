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
