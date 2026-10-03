use sqlx::PgPool;
use std::{io::Cursor, sync::Arc, time::Duration};
use tsz_rust::{
    auth::extract::AuthUser,
    avatar::{dto::AvatarUploadRequest, service},
    platform::storage::{
        MemoryAdapter, ObjectKey, ObjectStore, PutOptions, StoragePolicy, StoragePrivacy,
        StorageRegistry,
    },
    state::AppState,
};
use uuid::Uuid;

pub async fn setup(pool: &PgPool) -> (AppState, AuthUser, Arc<dyn ObjectStore>) {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id,email,password_hash,display_name) VALUES ($1,$2,'hash','Avatar')",
    )
    .bind(id)
    .bind(format!("{id}@example.test"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id,role) VALUES ($1,'student')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    let redis = deadpool_redis::Config::from_url(
        std::env::var("REDIS_URL").expect("isolated REDIS_URL is required"),
    )
    .create_pool(Some(deadpool_redis::Runtime::Tokio1))
    .unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis);
    let store = MemoryAdapter::object_store(
        "avatars".parse().unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            5242880,
            Duration::from_secs(600),
            None,
        )
        .unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store.clone()]).unwrap();
    state.avatar_public_base_url = Some("http://localhost:8396/api/v1/avatars".into());
    (
        state,
        AuthUser {
            subject: id,
            role: "student".into(),
            security_version: 0,
        },
        store,
    )
}

pub fn picture(format: image::ImageFormat) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(100, 60)
        .write_to(&mut output, format)
        .unwrap();
    output.into_inner()
}

pub async fn upload(state: &AppState, auth: &AuthUser, store: &dyn ObjectStore) -> String {
    let bytes = picture(image::ImageFormat::Png);
    let result = service::create_upload(
        state,
        auth,
        AvatarUploadRequest {
            content_type: "image/png".into(),
            size: bytes.len() as u64,
        },
    )
    .await
    .unwrap();
    store
        .put(
            &ObjectKey::parse(&result.upload.key).unwrap(),
            bytes,
            PutOptions::default(),
        )
        .await
        .unwrap();
    result.upload.key
}
