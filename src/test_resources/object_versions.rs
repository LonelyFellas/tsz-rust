use std::collections::BTreeSet;

use opendal::{Entry, Error, ErrorKind, Operator, Result};

async fn list_versions(operator: &Operator, prefix: &str) -> Result<Vec<Entry>> {
    operator
        .list_with(prefix)
        .versions(true)
        .deleted(true)
        .recursive(true)
        .await
}

fn is_canonical_path(path: &str) -> bool {
    path.trim() == path
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .strip_suffix('/')
            .unwrap_or(path)
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

pub(super) async fn purge_object_versions(operator: &Operator, key: &str) -> Result<()> {
    if !is_canonical_path(key) {
        return Err(Error::new(ErrorKind::ConfigInvalid, "invalid object key"));
    }

    // Finish listing before deleting, since an OSS page cursor can name a version we will delete.
    // Longer keys sharing this prefix are unrelated to this cleanup.
    for entry in list_versions(operator, key).await? {
        if entry.path() == key {
            let version = entry
                .metadata()
                .version()
                .filter(|version| !version.is_empty())
                .ok_or_else(|| Error::new(ErrorKind::Unexpected, "missing object version"))?;
            operator.delete_with(key).version(version).await?;
        }
    }

    if list_versions(operator, key)
        .await?
        .iter()
        .any(|entry| entry.path() == key)
    {
        return Err(Error::new(ErrorKind::Unexpected, "object versions remain"));
    }
    Ok(())
}

/// List full keys relative to the operator root, under a directory already scoped by the caller.
pub(super) async fn object_keys_with_versions(
    operator: &Operator,
    dir: &str,
) -> Result<Vec<String>> {
    if !dir.ends_with('/') || !is_canonical_path(dir) {
        return Err(Error::new(
            ErrorKind::ConfigInvalid,
            "invalid object version directory",
        ));
    }

    let mut keys = BTreeSet::new();
    for entry in list_versions(operator, dir).await? {
        let path = entry.path();
        if !path.starts_with(dir) || !is_canonical_path(path) {
            return Err(Error::new(
                ErrorKind::Unexpected,
                "object version listing returned a key outside the directory or unfit for reuse",
            ));
        }
        match entry.metadata().version() {
            // An OSS CommonPrefixes entry describes a directory, not an object version.
            None if entry.metadata().is_dir() => continue,
            Some(version) if !version.is_empty() && path != dir => {
                keys.insert(path.to_owned());
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Unexpected,
                    "invalid entry in object version directory",
                ));
            }
        }
    }
    Ok(keys.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc, sync::Mutex};

    use axum::{
        Router,
        extract::{Query, State},
        http::{Method, StatusCode, Uri},
    };
    use opendal::services::Oss;

    use super::*;

    const DIR: &str = "fixture-20261126/audio/";
    const WIRE_DIR: &str = "temporary-tests/fixture-20261126/audio/";
    const KEY: &str = "fixture-20261126/audio/clip";
    const WIRE_KEY: &str = "temporary-tests/fixture-20261126/audio/clip";
    const NEIGHBOR: &str = "temporary-tests/fixture-20261126/audio/clip-other";
    const OTHER_DIR: &str = "temporary-tests/fixture-20261126/audio-other/clip";
    const NESTED: &str = "temporary-tests/fixture-20261126/audio/previews/marker-only";

    type Version = (&'static str, &'static str, &'static str);

    #[derive(Default)]
    struct Bucket {
        versions: Vec<Version>,
        retain_version: Option<&'static str>,
        ignore_prefix: bool,
        listed_prefixes: Vec<String>,
        deleted_versions: Vec<String>,
    }

    async fn oss_response(
        State(bucket): State<Arc<Mutex<Bucket>>>,
        method: Method,
        uri: Uri,
        Query(query): Query<HashMap<String, String>>,
    ) -> (StatusCode, String) {
        let mut bucket = bucket.lock().unwrap();
        if method == Method::DELETE {
            let path = uri.path().trim_start_matches('/');
            let version = &query["versionId"];
            bucket.deleted_versions.push(version.clone());
            if bucket.retain_version != Some(version.as_str()) {
                bucket
                    .versions
                    .retain(|(key, id, _)| *key != path || *id != version);
            }
            return (StatusCode::NO_CONTENT, String::new());
        }

        assert_eq!(method, Method::GET);
        assert!(query.contains_key("versions"));
        assert!(!query.contains_key("delimiter"));
        let prefix = &query["prefix"];
        bucket.listed_prefixes.push(prefix.clone());
        let versions: Vec<_> = bucket
            .versions
            .iter()
            .filter(|(key, _, _)| bucket.ignore_prefix || key.starts_with(prefix))
            .collect();
        // Force a separate page for every record, including records with the same key.
        let index = if let Some(key_marker) = query.get("key-marker") {
            let version_marker = &query["version-id-marker"];
            let Some(index) = versions
                .iter()
                .position(|(key, id, _)| *key == key_marker && *id == version_marker)
            else {
                return (StatusCode::BAD_REQUEST, "unknown page marker".to_owned());
            };
            index + 1
        } else {
            assert!(!query.contains_key("version-id-marker"));
            0
        };
        let Some(&(key, id, kind)) = versions.get(index).copied() else {
            return (
                StatusCode::OK,
                "<ListVersionsResult><IsTruncated>false</IsTruncated></ListVersionsResult>".into(),
            );
        };
        let truncated = index + 1 < versions.len();
        let next = if truncated {
            format!(
                "<NextKeyMarker>{key}</NextKeyMarker><NextVersionIdMarker>{id}</NextVersionIdMarker>"
            )
        } else {
            String::new()
        };
        let size = if kind == "Version" {
            "<Size>2</Size>"
        } else {
            ""
        };
        (
            StatusCode::OK,
            format!(
                "<ListVersionsResult><IsTruncated>{truncated}</IsTruncated>{next}\
                 <{kind}><Key>{key}</Key><VersionId>{id}</VersionId><IsLatest>false</IsLatest>\
                 {size}<LastModified>2026-11-26T00:00:00Z</LastModified></{kind}></ListVersionsResult>"
            ),
        )
    }

    async fn fixture(bucket: Bucket) -> (Operator, Arc<Mutex<Bucket>>) {
        let bucket = Arc::new(Mutex::new(bucket));
        let router = Router::new()
            .fallback(oss_response)
            .with_state(bucket.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        opendal::install_default();
        let operator = Operator::new(
            Oss::default()
                .bucket("fixture")
                .root("/temporary-tests")
                .endpoint(&endpoint)
                .addressing_style("cname")
                .access_key_id("fixture")
                .access_key_secret("fixture"),
        )
        .unwrap();
        (operator, bucket)
    }

    #[tokio::test]
    async fn purge_checks_all_pages_and_retries_remaining_delete_marker() {
        let (operator, bucket) = fixture(Bucket {
            versions: vec![
                (WIRE_KEY, "v1", "Version"),
                (WIRE_KEY, "v2", "Version"),
                (WIRE_KEY, "marker", "DeleteMarker"),
                (NEIGHBOR, "keep", "Version"),
            ],
            retain_version: Some("marker"),
            ..Bucket::default()
        })
        .await;

        let error = purge_object_versions(&operator, KEY).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unexpected);
        assert_eq!(error.message(), "object versions remain");
        {
            let mut bucket = bucket.lock().unwrap();
            assert_eq!(bucket.deleted_versions, ["v1", "v2", "marker"]);
            assert_eq!(bucket.listed_prefixes, [WIRE_KEY; 6]);
            assert_eq!(
                bucket.versions,
                [
                    (WIRE_KEY, "marker", "DeleteMarker"),
                    (NEIGHBOR, "keep", "Version"),
                ]
            );
            bucket.retain_version = None;
        }

        purge_object_versions(&operator, KEY).await.unwrap();
        let bucket = bucket.lock().unwrap();
        assert_eq!(bucket.deleted_versions, ["v1", "v2", "marker", "marker"]);
        assert_eq!(bucket.listed_prefixes, [WIRE_KEY; 9]);
        assert_eq!(bucket.versions, [(NEIGHBOR, "keep", "Version")]);
    }

    #[tokio::test]
    async fn directory_lists_full_deduplicated_keys_across_version_pages_without_deleting() {
        let (operator, bucket) = fixture(Bucket {
            versions: vec![
                (WIRE_KEY, "v1", "Version"),
                (WIRE_KEY, "v2", "Version"),
                (NESTED, "marker", "DeleteMarker"),
                (OTHER_DIR, "keep", "Version"),
            ],
            ..Bucket::default()
        })
        .await;

        assert_eq!(
            object_keys_with_versions(&operator, DIR).await.unwrap(),
            [KEY, "fixture-20261126/audio/previews/marker-only"]
        );
        let bucket = bucket.lock().unwrap();
        assert_eq!(bucket.listed_prefixes, [WIRE_DIR; 3]);
        assert!(bucket.deleted_versions.is_empty());
    }

    #[tokio::test]
    async fn directory_rejects_out_of_scope_and_unsafe_results() {
        for key in [
            OTHER_DIR,
            "temporary-tests/fixture-20261126/audio/../audio-other/clip",
        ] {
            let (operator, bucket) = fixture(Bucket {
                versions: vec![(WIRE_KEY, "v1", "Version"), (key, "unexpected", "Version")],
                ignore_prefix: true,
                ..Bucket::default()
            })
            .await;

            let error = object_keys_with_versions(&operator, DIR).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Unexpected);
            let bucket = bucket.lock().unwrap();
            assert_eq!(bucket.listed_prefixes, [WIRE_DIR; 2]);
            assert!(bucket.deleted_versions.is_empty());
        }
    }

    #[tokio::test]
    async fn directory_rejects_unscoped_or_unnormalized_input_before_listing() {
        let (operator, bucket) = fixture(Bucket::default()).await;
        for dir in [
            "",
            "/",
            "fixture-20261126/audio",
            "/fixture-20261126/audio/",
            "fixture-20261126//audio/",
            "fixture-20261126/../audio/",
        ] {
            let error = object_keys_with_versions(&operator, dir).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ConfigInvalid);
        }
        let bucket = bucket.lock().unwrap();
        assert!(bucket.listed_prefixes.is_empty());
        assert!(bucket.deleted_versions.is_empty());
    }
}
