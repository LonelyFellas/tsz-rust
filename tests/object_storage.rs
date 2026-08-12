use std::{sync::Arc, time::Duration};

use tsz_rust::platform::storage::{
    CacheControl, MemoryAdapter, ObjectContentType, ObjectKey, ObjectKeyError, ObjectStorageConfig,
    ObjectStore, PutOptions, StorageConfigError, StorageError, StoragePolicy, StoragePrivacy,
    StorageRegistry, StorageSpace,
};

fn policy(max_object_size: u64, ttl_seconds: u64) -> StoragePolicy {
    StoragePolicy::new(
        StoragePrivacy::Private,
        max_object_size,
        Duration::from_secs(ttl_seconds),
        Some(CacheControl::parse("private, max-age=60").expect("固定缓存策略合法")),
    )
    .expect("测试策略合法")
}

fn memory_store(space: &str, max_object_size: u64) -> Arc<dyn ObjectStore> {
    MemoryAdapter::object_store(
        StorageSpace::parse(space).expect("固定空间名合法"),
        policy(max_object_size, 300),
    )
}

#[test]
fn object_key_rejects_unsafe_or_non_portable_values() {
    let invalid = [
        ("", ObjectKeyError::Empty),
        ("/avatars/a.png", ObjectKeyError::Absolute),
        ("../a.png", ObjectKeyError::UnsafeSegment),
        ("avatars/./a.png", ObjectKeyError::UnsafeSegment),
        ("avatars//a.png", ObjectKeyError::UnsafeSegment),
        ("avatars/a\\b.png", ObjectKeyError::InvalidCharacter),
        ("avatars/a.png?download=1", ObjectKeyError::InvalidCharacter),
        ("头像/a.png", ObjectKeyError::InvalidCharacter),
    ];

    for (value, expected) in invalid {
        assert_eq!(ObjectKey::parse(value), Err(expected), "应拒绝 {value:?}");
    }
    assert_eq!(
        ObjectKey::parse("a".repeat(513)),
        Err(ObjectKeyError::TooLong)
    );
    ObjectKey::parse("a".repeat(512)).expect("512 字节对象键必须可用");
}

#[test]
fn object_key_generation_is_server_owned_unique_and_validated() {
    let first = ObjectKey::generate("avatars/users", Some("webp")).expect("生成应成功");
    let second = ObjectKey::generate("avatars/users", Some("webp")).expect("生成应成功");

    assert_ne!(first, second);
    assert!(first.as_str().starts_with("avatars/users/"));
    assert!(first.as_str().ends_with(".webp"));
    assert_eq!(
        ObjectKey::generate("avatars", Some("tar.gz")),
        Err(ObjectKeyError::InvalidExtension)
    );
}

#[tokio::test]
async fn memory_adapter_supports_the_complete_safe_object_lifecycle() {
    let store = memory_store("attachments", 1024);
    let source = ObjectKey::parse("messages/source.txt").expect("固定键合法");
    let destination = ObjectKey::parse("messages/copy.txt").expect("固定键合法");
    let options = PutOptions::new(Some(
        ObjectContentType::parse("text/plain; charset=utf-8").expect("内容类型合法"),
    ));

    let metadata = store
        .put(&source, b"hello".to_vec(), options)
        .await
        .expect("内存写入应成功");
    assert_eq!(metadata.content_length, 5);
    assert_eq!(
        metadata.content_type.as_deref(),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        metadata.cache_control.as_deref(),
        Some("private, max-age=60")
    );
    assert!(metadata.etag.is_some());
    assert_eq!(store.read(&source).await.expect("读取应成功"), b"hello");
    assert_eq!(store.stat(&source).await.expect("stat 应成功"), metadata);

    let copied = store.copy(&source, &destination).await.expect("复制应成功");
    assert_eq!(copied.content_length, 5);
    assert_eq!(
        copied.content_type.as_deref(),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(copied.cache_control.as_deref(), Some("private, max-age=60"));
    assert_eq!(
        store.read(&destination).await.expect("副本应可读"),
        b"hello"
    );

    store.delete(&source).await.expect("删除应成功");
    store.delete(&source).await.expect("重复删除必须幂等");
    assert!(matches!(
        store.read(&source).await,
        Err(StorageError::ObjectNotFound { .. })
    ));
}

#[tokio::test]
async fn registry_keeps_identical_keys_isolated_between_spaces() {
    let avatars = memory_store("avatars", 1024);
    let speech = memory_store("speech", 1024);
    let registry = StorageRegistry::from_stores([avatars, speech]).expect("空间唯一");
    let key = ObjectKey::parse("users/shared.bin").expect("固定键合法");
    let avatars_space = StorageSpace::parse("avatars").expect("固定空间合法");
    let speech_space = StorageSpace::parse("speech").expect("固定空间合法");

    registry
        .get(&avatars_space)
        .expect("头像空间存在")
        .put(&key, b"avatar".to_vec(), PutOptions::default())
        .await
        .expect("头像写入成功");

    assert!(matches!(
        registry
            .get(&speech_space)
            .expect("语音空间存在")
            .read(&key)
            .await,
        Err(StorageError::ObjectNotFound { .. })
    ));
    assert!(matches!(
        registry.get(&StorageSpace::parse("missing").expect("固定空间合法")),
        Err(StorageError::SpaceNotConfigured(_))
    ));
}

#[tokio::test]
async fn size_limit_applies_to_put_and_presigned_write() {
    let store = memory_store("attachments", 4);
    let key = ObjectKey::parse("messages/a.bin").expect("固定键合法");

    assert!(matches!(
        store.put(&key, vec![0; 5], PutOptions::default()).await,
        Err(StorageError::ObjectTooLarge {
            max: 4,
            actual: 5,
            ..
        })
    ));
    assert!(matches!(
        store.presign_write(&key, 5, PutOptions::default()).await,
        Err(StorageError::ObjectTooLarge {
            max: 4,
            actual: 5,
            ..
        })
    ));
}

#[tokio::test]
async fn presigned_requests_use_space_ttl_and_redact_secrets_from_debug() {
    let space = StorageSpace::parse("speech").expect("固定空间合法");
    let store = MemoryAdapter::object_store(space, policy(1024, 123));
    let key = ObjectKey::parse("tts/a.mp3").expect("固定键合法");
    let request = store
        .presign_write(
            &key,
            12,
            PutOptions::new(Some(
                ObjectContentType::parse("audio/mpeg").expect("内容类型合法"),
            )),
        )
        .await
        .expect("预签名应成功");

    assert_eq!(request.expires_in(), Duration::from_secs(123));
    assert_eq!(request.method(), "PUT");
    assert_eq!(
        request.headers().get("content-length").map(String::as_str),
        Some("12")
    );
    assert_eq!(
        request.headers().get("content-type").map(String::as_str),
        Some("audio/mpeg")
    );
    let debug = format!("{request:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(request.url()));
    assert!(!debug.contains("audio/mpeg"));
}

fn complete_storage_pairs() -> Vec<(String, String)> {
    [
        ("OBJECT_STORAGE_SPACES", "avatars,speech"),
        ("OBJECT_STORAGE_AVATARS_BACKEND", "oss"),
        (
            "OBJECT_STORAGE_AVATARS_OSS_ENDPOINT",
            "https://oss-cn-hangzhou.aliyuncs.com",
        ),
        ("OBJECT_STORAGE_AVATARS_OSS_REGION", "cn-hangzhou"),
        ("OBJECT_STORAGE_AVATARS_OSS_BUCKET", "tsz-avatars"),
        ("OBJECT_STORAGE_AVATARS_OSS_ROOT", "/production/avatars"),
        (
            "OBJECT_STORAGE_AVATARS_OSS_ACCESS_KEY_ID",
            "avatar-access-key",
        ),
        (
            "OBJECT_STORAGE_AVATARS_OSS_ACCESS_KEY_SECRET",
            "avatar-secret",
        ),
        ("OBJECT_STORAGE_AVATARS_PRIVACY", "private"),
        ("OBJECT_STORAGE_AVATARS_MAX_OBJECT_SIZE_BYTES", "1048576"),
        ("OBJECT_STORAGE_AVATARS_PRESIGN_TTL_SECONDS", "300"),
        (
            "OBJECT_STORAGE_AVATARS_CACHE_CONTROL",
            "private, max-age=60",
        ),
        ("OBJECT_STORAGE_SPEECH_BACKEND", "oss"),
        (
            "OBJECT_STORAGE_SPEECH_OSS_ENDPOINT",
            "https://oss-cn-shanghai.aliyuncs.com",
        ),
        ("OBJECT_STORAGE_SPEECH_OSS_REGION", "cn-shanghai"),
        ("OBJECT_STORAGE_SPEECH_OSS_BUCKET", "tsz-speech"),
        ("OBJECT_STORAGE_SPEECH_OSS_ROOT", "/production/speech"),
        (
            "OBJECT_STORAGE_SPEECH_OSS_ACCESS_KEY_ID",
            "speech-access-key",
        ),
        (
            "OBJECT_STORAGE_SPEECH_OSS_ACCESS_KEY_SECRET",
            "speech-secret",
        ),
        ("OBJECT_STORAGE_SPEECH_PRIVACY", "public-read"),
        ("OBJECT_STORAGE_SPEECH_MAX_OBJECT_SIZE_BYTES", "5242880"),
        ("OBJECT_STORAGE_SPEECH_PRESIGN_TTL_SECONDS", "600"),
        (
            "OBJECT_STORAGE_SPEECH_CACHE_CONTROL",
            "public, max-age=86400",
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect()
}

#[test]
fn storage_configuration_is_disabled_when_absent() {
    let config = ObjectStorageConfig::from_pairs([(
        "DATABASE_URL".to_owned(),
        "postgres://localhost/tsz".to_owned(),
    )])
    .expect("无 OBJECT_STORAGE_* 时应默认关闭");

    assert!(config.is_empty());
    assert!(config.build_registry().expect("空配置构建成功").is_empty());
}

#[tokio::test]
async fn explicit_storage_configuration_builds_all_spaces_without_network_access() {
    let config = ObjectStorageConfig::from_pairs(complete_storage_pairs()).expect("完整配置合法");
    assert_eq!(config.len(), 2);

    let debug = format!("{config:?}");
    assert!(!debug.contains("avatar-access-key"));
    assert!(!debug.contains("avatar-secret"));
    assert!(!debug.contains("speech-access-key"));
    assert!(!debug.contains("speech-secret"));
    let registry = config.build_registry().expect("构建 client 不应联网");
    assert_eq!(registry.len(), 2);
    let avatars = registry
        .get(&StorageSpace::parse("avatars").expect("固定空间合法"))
        .expect("头像空间已注册");
    assert_eq!(avatars.policy().privacy(), StoragePrivacy::Private);
    assert_eq!(avatars.policy().max_object_size(), 1_048_576);
    assert_eq!(avatars.policy().presign_ttl(), Duration::from_secs(300));
    assert_eq!(
        avatars.policy().cache_control().map(CacheControl::as_str),
        Some("private, max-age=60")
    );
    let signed = avatars
        .presign_write(
            &ObjectKey::parse("users/a.webp").expect("固定键合法"),
            128,
            PutOptions::new(Some(
                ObjectContentType::parse("image/webp").expect("内容类型合法"),
            )),
        )
        .await
        .expect("OSS 预签名只使用本地凭据，不访问网络");
    assert_eq!(signed.method(), "PUT");
    assert_eq!(signed.expires_in(), Duration::from_secs(300));
    assert_eq!(
        signed.headers().get("content-length").map(String::as_str),
        Some("128")
    );
    assert!(
        signed
            .url()
            .contains("x-oss-signature-version=OSS4-HMAC-SHA256"),
        "OSS 预签名写必须使用 V4"
    );
    assert!(
        signed
            .url()
            .contains("x-oss-additional-headers=cache-control%3Bcontent-length%3Bhost"),
        "Content-Length 与固定 Cache-Control 必须进入 V4 签名约束"
    );
}

#[test]
fn storage_configuration_rejects_partial_or_orphaned_spaces() {
    assert!(matches!(
        ObjectStorageConfig::from_pairs([(
            "OBJECT_STORAGE_AVATARS_BACKEND".to_owned(),
            "oss".to_owned()
        )]),
        Err(StorageConfigError::MissingSpaceList)
    ));

    assert_eq!(
        ObjectStorageConfig::from_pairs([(
            "OBJECT_STORAGE_SPACES".to_owned(),
            "avatars,".to_owned()
        )])
        .expect_err("空空间项必须拒绝"),
        StorageConfigError::EmptySpaceList
    );

    assert!(matches!(
        ObjectStorageConfig::from_pairs([(
            "OBJECT_STORAGE_SPACES".to_owned(),
            "avatars".to_owned()
        )]),
        Err(StorageConfigError::MissingField {
            field: "BACKEND",
            ..
        })
    ));

    let mut pairs = complete_storage_pairs();
    pairs.push(("OBJECT_STORAGE_ORPHAN_BACKEND".to_owned(), "oss".to_owned()));
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::UnknownVariable { .. })
    ));

    let mut pairs = complete_storage_pairs();
    pairs.retain(|(name, _)| name != "OBJECT_STORAGE_SPEECH_OSS_REGION");
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::MissingField {
            field: "OSS_REGION",
            ..
        })
    ));
}

#[test]
fn storage_configuration_rejects_invalid_limits_and_ttl() {
    let mut pairs = complete_storage_pairs();
    let max = pairs
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_AVATARS_MAX_OBJECT_SIZE_BYTES")
        .expect("测试字段存在");
    max.1 = "0".to_owned();
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::InvalidField {
            field: "MAX_OBJECT_SIZE_BYTES",
            ..
        })
    ));

    let mut pairs = complete_storage_pairs();
    let ttl = pairs
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_SPEECH_PRESIGN_TTL_SECONDS")
        .expect("测试字段存在");
    ttl.1 = "86401".to_owned();
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::InvalidField {
            field: "PRESIGN_TTL_SECONDS",
            ..
        })
    ));

    let mut pairs = complete_storage_pairs();
    let root = pairs
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_AVATARS_OSS_ROOT")
        .expect("测试字段存在");
    root.1 = format!("/{}", "a".repeat(511));
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::InvalidField {
            field: "OSS_ROOT",
            ..
        })
    ));

    let mut pairs = complete_storage_pairs();
    let endpoint = pairs
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_AVATARS_OSS_ENDPOINT")
        .expect("测试字段存在");
    endpoint.1 = format!(
        "https://{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(58)
    );
    assert!(matches!(
        ObjectStorageConfig::from_pairs(pairs),
        Err(StorageConfigError::InvalidField {
            field: "OSS_ENDPOINT",
            ..
        })
    ));
}

#[test]
fn storage_configuration_rejects_overlapping_physical_roots() {
    for speech_root in [
        "/",
        "/production",
        "/production/avatars",
        "/production/avatars/speech",
    ] {
        let mut pairs = complete_storage_pairs();
        pairs
            .iter_mut()
            .find(|(name, _)| name == "OBJECT_STORAGE_SPEECH_OSS_BUCKET")
            .expect("测试字段存在")
            .1 = "tsz-avatars".to_owned();
        pairs
            .iter_mut()
            .find(|(name, _)| name == "OBJECT_STORAGE_SPEECH_OSS_ROOT")
            .expect("测试字段存在")
            .1 = speech_root.to_owned();

        assert!(matches!(
            ObjectStorageConfig::from_pairs(pairs),
            Err(StorageConfigError::OverlappingOssRoot { .. })
        ));
    }

    let mut adjacent = complete_storage_pairs();
    adjacent
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_SPEECH_OSS_BUCKET")
        .expect("测试字段存在")
        .1 = "tsz-avatars".to_owned();
    adjacent
        .iter_mut()
        .find(|(name, _)| name == "OBJECT_STORAGE_SPEECH_OSS_ROOT")
        .expect("测试字段存在")
        .1 = "/production/avatars-speech".to_owned();
    ObjectStorageConfig::from_pairs(adjacent).expect("相邻但不重叠的 root 应允许");
}

#[test]
fn registry_rejects_duplicate_space_bindings() {
    let first = memory_store("avatars", 1024);
    let second = memory_store("avatars", 2048);
    assert!(matches!(
        StorageRegistry::from_stores([first, second]),
        Err(StorageError::DuplicateSpace(_))
    ));
}
