//! Opt-in online acceptance resources. Ordinary requests retain their original storage roots.
use std::collections::BTreeMap;

use axum::http::HeaderMap;
use uuid::Uuid;

use crate::{
    error::{AppError, ErrorCode},
    platform::storage::{
        ObjectStorageConfig, OssAdapterConfig, SecretString, StorageRegistry, valid_test_run,
    },
    state::AppState,
};

pub const HEADER: &str = "x-tsz-test-run";

#[derive(Clone, Debug)]
pub struct TestResourcesConfig {
    adapter: OssAdapterConfig,
    spaces: ObjectStorageConfig,
    admins: Vec<Uuid>,
    users: Vec<Uuid>,
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
        }))
    }

    pub fn registry(
        &self,
        run: Option<&str>,
    ) -> Result<StorageRegistry, crate::platform::storage::StorageError> {
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
