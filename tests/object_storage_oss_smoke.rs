use std::time::Duration;

use anyhow::{Context, ensure};
use tsz_rust::platform::storage::{
    ObjectContentType, ObjectKey, OssAdapter, OssAdapterConfig, PresignedRequest, PutOptions,
    SecretString, StoragePolicy, StoragePrivacy, StorageSpace,
};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("ignored OSS smoke test requires {name}"))
}

async fn execute_presigned(
    client: &reqwest::Client,
    request: &PresignedRequest,
    body: Option<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let method = reqwest::Method::from_bytes(request.method().as_bytes())
        .context("presigned method is invalid")?;
    let mut builder = client.request(method, request.url());
    for (name, value) in request.headers() {
        builder = builder.header(name, value);
    }
    if let Some(body) = body {
        builder = builder.body(body);
    }
    // 丢弃底层错误详情，避免失败输出携带签名 URL。
    let response = builder
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("presigned OSS request failed"))?;
    ensure!(
        response.status().is_success(),
        "presigned OSS request returned status {}",
        response.status()
    );
    response
        .bytes()
        .await
        .map(|body| body.to_vec())
        .map_err(|_| anyhow::anyhow!("presigned OSS response body failed"))
}

#[tokio::test]
#[ignore = "requires explicit TSZ_OSS_SMOKE_* credentials and writes temporary OSS objects"]
async fn real_oss_round_trip_smoke() {
    let space = StorageSpace::parse("oss-smoke").expect("固定空间合法");
    let policy = StoragePolicy::new(
        StoragePrivacy::Private,
        1024 * 1024,
        Duration::from_secs(60),
        None,
    )
    .expect("固定策略合法");
    let config = OssAdapterConfig::new(
        required("TSZ_OSS_SMOKE_ENDPOINT"),
        required("TSZ_OSS_SMOKE_REGION"),
        required("TSZ_OSS_SMOKE_BUCKET"),
        required("TSZ_OSS_SMOKE_ROOT"),
        SecretString::new(required("TSZ_OSS_SMOKE_ACCESS_KEY_ID")).expect("AccessKey ID 非空"),
        SecretString::new(required("TSZ_OSS_SMOKE_ACCESS_KEY_SECRET"))
            .expect("AccessKey Secret 非空"),
    )
    .expect("OSS 冒烟配置合法");
    let store = OssAdapter::object_store(space, policy, config).expect("OSS adapter 构建成功");
    let source = ObjectKey::generate("codex-smoke", Some("txt")).expect("生成键成功");
    let destination = ObjectKey::generate("codex-smoke", Some("txt")).expect("生成键成功");
    let presigned = ObjectKey::generate("codex-smoke", Some("txt")).expect("生成键成功");

    let exercise = async {
        let written = store
            .put(
                &source,
                b"object-storage-smoke".to_vec(),
                PutOptions::default(),
            )
            .await?;
        ensure!(
            written.content_length == b"object-storage-smoke".len() as u64,
            "OSS put metadata length mismatch"
        );
        let body = store.read(&source).await?;
        ensure!(body == b"object-storage-smoke", "OSS read body mismatch");
        store.copy(&source, &destination).await?;
        ensure!(
            store.stat(&destination).await?.content_length == body.len() as u64,
            "OSS copied metadata length mismatch"
        );
        ensure!(
            store.presign_read(&source).await?.expires_in() == Duration::from_secs(60),
            "OSS presign TTL mismatch"
        );

        let client = reqwest::Client::new();
        let presigned_body = b"presigned-write-smoke".to_vec();
        let write = store
            .presign_write(
                &presigned,
                presigned_body.len() as u64,
                PutOptions::new(Some(
                    ObjectContentType::parse("text/plain").expect("固定内容类型合法"),
                )),
            )
            .await?;
        execute_presigned(&client, &write, Some(presigned_body.clone())).await?;
        let read = store.presign_read(&presigned).await?;
        ensure!(
            execute_presigned(&client, &read, None).await? == presigned_body,
            "presigned OSS read body mismatch"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;

    // 无论远端步骤或校验是否失败，都尽力清理本测试创建的精确对象键；绝不做前缀删除。
    let _ = store.delete(&source).await;
    let _ = store.delete(&destination).await;
    let _ = store.delete(&presigned).await;
    exercise.expect("真实 OSS 往返应成功");
}
