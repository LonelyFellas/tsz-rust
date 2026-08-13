use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use opendal::{ErrorKind, Metadata, Operator, services::Oss};
use reqsign_aliyun_oss::{Credential, RequestSigner, SigningVersion, StaticCredentialProvider};
use reqsign_core::{Context, Signer};
use thiserror::Error;

use super::{
    error::{BackendErrorKind, StorageError, StorageOperation},
    model::{
        MAX_OBJECT_KEY_BYTES, ObjectKey, ObjectMetadata, PresignedRequest, StoragePolicy,
        StorageSpace,
    },
    service::{
        BackendReadResult, BackendWriteOptions, ObjectStore, ObjectStoreBackend, StorageService,
    },
};

const MAX_OSS_OBJECT_NAME_BYTES: usize = 1023;
const MAX_OSS_ROOT_BYTES: usize = MAX_OSS_OBJECT_NAME_BYTES - MAX_OBJECT_KEY_BYTES;

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Result<Self, OssConfigError> {
        let value = value.into();
        if value.is_empty() {
            return Err(OssConfigError::EmptyCredential);
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OssConfigError {
    #[error("OSS endpoint must be a lowercase HTTPS hostname")]
    InvalidEndpoint,
    #[error("OSS bucket and endpoint exceed the virtual-host DNS length limit")]
    VirtualHostTooLong,
    #[error("OSS region is invalid")]
    InvalidRegion,
    #[error("OSS bucket name is invalid")]
    InvalidBucket,
    #[error("OSS root must be an absolute, normalized object prefix")]
    InvalidRoot,
    #[error("OSS credentials must not be empty")]
    EmptyCredential,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OssAdapterConfig {
    endpoint: String,
    region: String,
    bucket: String,
    root: String,
    access_key_id: SecretString,
    access_key_secret: SecretString,
}

impl OssAdapterConfig {
    pub fn new(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        bucket: impl Into<String>,
        root: impl Into<String>,
        access_key_id: SecretString,
        access_key_secret: SecretString,
    ) -> Result<Self, OssConfigError> {
        let endpoint = endpoint.into();
        let region = region.into();
        let bucket = bucket.into();
        let root = root.into();
        validate_endpoint(&endpoint)?;
        validate_region(&region)?;
        validate_bucket(&bucket)?;
        validate_virtual_host(&endpoint, &bucket)?;
        validate_root(&root)?;
        Ok(Self {
            endpoint,
            region,
            bucket,
            root,
            access_key_id,
            access_key_secret,
        })
    }
}

impl fmt::Debug for OssAdapterConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OssAdapterConfig")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("root", &self.root)
            .field("access_key_id", &"[REDACTED]")
            .field("access_key_secret", &"[REDACTED]")
            .finish()
    }
}

impl OssAdapterConfig {
    pub(crate) fn bucket(&self) -> &str {
        &self.bucket
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), OssConfigError> {
    let authority = endpoint
        .strip_prefix("https://")
        .ok_or(OssConfigError::InvalidEndpoint)?;
    if authority.len() > 253
        || authority.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || label.starts_with('-')
                || label.ends_with('-')
        })
    {
        return Err(OssConfigError::InvalidEndpoint);
    }
    Ok(())
}

fn validate_region(region: &str) -> Result<(), OssConfigError> {
    let bytes = region.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(OssConfigError::InvalidRegion);
    }
    Ok(())
}

fn validate_virtual_host(endpoint: &str, bucket: &str) -> Result<(), OssConfigError> {
    let authority = endpoint
        .strip_prefix("https://")
        .expect("endpoint 已完成 HTTPS 校验");
    if bucket.len() + 1 + authority.len() > 253 {
        return Err(OssConfigError::VirtualHostTooLong);
    }
    Ok(())
}

fn validate_bucket(bucket: &str) -> Result<(), OssConfigError> {
    let bytes = bucket.as_bytes();
    if !(3..=63).contains(&bytes.len())
        || !bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(OssConfigError::InvalidBucket);
    }
    Ok(())
}

fn validate_root(root: &str) -> Result<(), OssConfigError> {
    if !root.starts_with('/')
        || (root.len() > 1 && root.ends_with('/'))
        || root.len() > MAX_OSS_ROOT_BYTES
        || !root
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        || (root != "/"
            && root
                .split('/')
                .skip(1)
                .any(|segment| segment.is_empty() || segment == "." || segment == ".."))
    {
        return Err(OssConfigError::InvalidRoot);
    }
    Ok(())
}

pub struct OssAdapter {
    space: StorageSpace,
    operator: Operator,
    presign_write_endpoint: String,
    root: String,
    presign_write_signer: Signer<Credential>,
}

impl OssAdapter {
    pub fn object_store(
        space: StorageSpace,
        policy: StoragePolicy,
        config: OssAdapterConfig,
    ) -> Result<Arc<dyn ObjectStore>, StorageError> {
        // OpenDAL 0.58 no longer installs an HTTP transport through service construction.
        // Installation is process-global, idempotent (first-installed wins), and lazy, so do it
        // at the OSS boundary before any operator can issue a request.
        opendal::install_default();
        let presign_write_endpoint = format!(
            "https://{}.{}",
            config.bucket,
            config
                .endpoint
                .strip_prefix("https://")
                .expect("构造配置已校验 HTTPS endpoint")
        );
        let credential_provider = StaticCredentialProvider::new(
            config.access_key_id.expose(),
            config.access_key_secret.expose(),
        );
        let request_signer = RequestSigner::new(&config.bucket)
            .with_region(&config.region)
            .with_signing_version(SigningVersion::V4);
        let presign_write_signer = Signer::new(Context::new(), credential_provider, request_signer);
        let builder = Oss::default()
            .root(&config.root)
            .bucket(&config.bucket)
            .endpoint(&config.endpoint)
            .access_key_id(config.access_key_id.expose())
            .access_key_secret(config.access_key_secret.expose());
        let operator = Operator::new(builder)
            .map_err(|error| map_opendal_error(&space, StorageOperation::Configure, None, error))?;
        let backend = Arc::new(Self {
            space: space.clone(),
            operator,
            presign_write_endpoint,
            root: config.root,
            presign_write_signer,
        });
        Ok(Arc::new(StorageService::new(space, policy, backend)))
    }

    fn map_error(
        &self,
        operation: StorageOperation,
        key: Option<&ObjectKey>,
        error: opendal::Error,
    ) -> StorageError {
        map_opendal_error(&self.space, operation, key, error)
    }

    fn presign_write_url(&self, key: &ObjectKey) -> String {
        if self.root == "/" {
            format!("{}/{}", self.presign_write_endpoint, key.as_str())
        } else {
            format!(
                "{}{}/{}",
                self.presign_write_endpoint,
                self.root,
                key.as_str()
            )
        }
    }
}

fn map_opendal_error(
    space: &StorageSpace,
    operation: StorageOperation,
    key: Option<&ObjectKey>,
    error: opendal::Error,
) -> StorageError {
    let temporary = error.is_temporary();
    match error.kind() {
        ErrorKind::NotFound if key.is_some() => {
            StorageError::not_found(space, key.expect("match guard 已检查"))
        }
        ErrorKind::PermissionDenied => {
            StorageError::backend(space, operation, BackendErrorKind::AccessDenied)
        }
        ErrorKind::RateLimited => {
            StorageError::backend(space, operation, BackendErrorKind::RateLimited)
        }
        ErrorKind::Unsupported => StorageError::Unsupported {
            space: space.clone(),
            operation,
        },
        _ if temporary => {
            StorageError::backend(space, operation, BackendErrorKind::TemporarilyUnavailable)
        }
        _ => StorageError::backend(space, operation, BackendErrorKind::Unexpected),
    }
}

fn convert_metadata(metadata: &Metadata) -> ObjectMetadata {
    ObjectMetadata {
        content_length: metadata.content_length(),
        content_type: metadata.content_type().map(str::to_owned),
        cache_control: metadata.cache_control().map(str::to_owned),
        etag: metadata.etag().map(str::to_owned),
        // OpenDAL 0.58 使用 jiff::Timestamp。底座不为可选时间字段引入泄漏到公共 API 的依赖；
        // 领域若需要权威时间，应存自己的数据库元数据。
        last_modified: None,
    }
}

fn convert_write_metadata(metadata: &Metadata, options: &BackendWriteOptions) -> ObjectMetadata {
    let mut converted = convert_metadata(metadata);
    // OSS PutObject 成功响应不回传长度和写入 headers；这些值已经由底座校验并实际发往后端。
    converted.content_length = options.content_length;
    converted.content_type.clone_from(&options.content_type);
    converted.cache_control.clone_from(&options.cache_control);
    converted
}

fn convert_presigned_request(
    request: opendal::raw::PresignedRequest,
    expires_in: Duration,
) -> Result<PresignedRequest, BackendErrorKind> {
    let mut headers = BTreeMap::new();
    for (name, value) in request.header() {
        let value = value.to_str().map_err(|_| BackendErrorKind::Unexpected)?;
        headers.insert(name.as_str().to_owned(), value.to_owned());
    }
    Ok(PresignedRequest::new(
        request.method().as_str(),
        request.uri().to_string(),
        headers,
        expires_in,
    ))
}

#[async_trait]
impl ObjectStoreBackend for OssAdapter {
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: &BackendWriteOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        let mut write = self.operator.write_with(key.as_str(), body);
        if let Some(content_type) = &options.content_type {
            write = write.content_type(content_type);
        }
        if let Some(cache_control) = &options.cache_control {
            write = write.cache_control(cache_control);
        }
        write
            .await
            .map(|metadata| convert_write_metadata(&metadata, options))
            .map_err(|error| self.map_error(StorageOperation::Put, Some(key), error))
    }

    async fn read(&self, key: &ObjectKey, limit: u64) -> Result<BackendReadResult, StorageError> {
        let reader = self
            .operator
            .reader(key.as_str())
            .await
            .map_err(|error| self.map_error(StorageOperation::Read, Some(key), error))?;
        // 不配置 chunk，OpenDAL 会直接发起一个全量流式 GET，不会先 stat 固化读取范围。
        let mut stream = reader
            .into_stream(..)
            .await
            .map_err(|error| self.map_error(StorageOperation::Read, Some(key), error))?;
        // metadata() 打开同一个 GET；随后 stream 继续消费该响应，正文和元数据属于同一版本。
        let metadata = stream
            .metadata()
            .await
            .map_err(|error| self.map_error(StorageOperation::Read, Some(key), error))?;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut body = Vec::new();
        while body.len() < limit {
            let Some(buffer) = stream.next().await else {
                break;
            };
            let buffer =
                buffer.map_err(|error| self.map_error(StorageOperation::Read, Some(key), error))?;
            for chunk in buffer {
                let remaining = limit - body.len();
                if remaining == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
        }
        Ok(BackendReadResult {
            body,
            metadata: convert_metadata(&metadata),
        })
    }

    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        self.operator
            .stat(key.as_str())
            .await
            .map(|metadata| convert_metadata(&metadata))
            .map_err(|error| self.map_error(StorageOperation::Stat, Some(key), error))
    }

    async fn presign_read(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedRequest, StorageError> {
        let request = self
            .operator
            .presign_read(key.as_str(), expires_in)
            .await
            .map_err(|error| self.map_error(StorageOperation::PresignRead, Some(key), error))?;
        convert_presigned_request(request, expires_in)
            .map_err(|kind| StorageError::backend(&self.space, StorageOperation::PresignRead, kind))
    }

    async fn presign_write(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
        options: &BackendWriteOptions,
    ) -> Result<PresignedRequest, StorageError> {
        let mut request = http::Request::put(self.presign_write_url(key))
            .header(CONTENT_LENGTH, options.content_length);
        if let Some(content_type) = &options.content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        if let Some(cache_control) = &options.cache_control {
            request = request.header(CACHE_CONTROL, cache_control);
        }
        let request = request.body(()).map_err(|_| {
            StorageError::backend(
                &self.space,
                StorageOperation::PresignWrite,
                BackendErrorKind::Unexpected,
            )
        })?;
        let (mut parts, ()) = request.into_parts();
        self.presign_write_signer
            .sign(&mut parts, Some(expires_in))
            .await
            .map_err(|_| {
                StorageError::backend(
                    &self.space,
                    StorageOperation::PresignWrite,
                    BackendErrorKind::Unexpected,
                )
            })?;

        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| {
                value
                    .to_str()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
                    .map_err(|_| {
                        StorageError::backend(
                            &self.space,
                            StorageOperation::PresignWrite,
                            BackendErrorKind::Unexpected,
                        )
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(PresignedRequest::new(
            parts.method.as_str(),
            parts.uri.to_string(),
            headers,
            expires_in,
        ))
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        self.operator
            .delete(key.as_str())
            .await
            .map_err(|error| self.map_error(StorageOperation::Delete, Some(key), error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> StorageSpace {
        StorageSpace::parse("avatars").expect("固定空间合法")
    }

    #[test]
    fn oss_configuration_validates_boundaries_and_redacts_credentials() {
        let config = OssAdapterConfig::new(
            "https://oss-cn-hangzhou.aliyuncs.com",
            "cn-hangzhou",
            "tsz-avatars",
            "/",
            SecretString::new("access-key-id").expect("凭据非空"),
            SecretString::new("access-key-secret").expect("凭据非空"),
        )
        .expect("独占 bucket 可使用根路径");
        let debug = format!("{config:?}");
        assert!(!debug.contains("access-key-id"));
        assert!(!debug.contains("access-key-secret"));
        assert!(debug.contains("[REDACTED]"));

        assert!(matches!(
            OssAdapterConfig::new(
                "http://oss-cn-hangzhou.aliyuncs.com",
                "cn-hangzhou",
                "tsz-avatars",
                "/avatars",
                SecretString::new("id").expect("凭据非空"),
                SecretString::new("secret").expect("凭据非空"),
            ),
            Err(OssConfigError::InvalidEndpoint)
        ));
        assert!(matches!(
            OssAdapterConfig::new(
                "https://oss-cn-hangzhou.aliyuncs.com",
                "cn-hangzhou",
                "tsz-avatars",
                "/avatars/../speech",
                SecretString::new("id").expect("凭据非空"),
                SecretString::new("secret").expect("凭据非空"),
            ),
            Err(OssConfigError::InvalidRoot)
        ));

        let maximum_endpoint = format!(
            "https://{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(57)
        );
        OssAdapterConfig::new(
            maximum_endpoint,
            "cn-hangzhou",
            "abc",
            "/avatars",
            SecretString::new("id").expect("凭据非空"),
            SecretString::new("secret").expect("凭据非空"),
        )
        .expect("组合虚拟主机 253 字节必须允许");
        let oversized_endpoint = format!(
            "https://{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(58)
        );
        assert!(matches!(
            OssAdapterConfig::new(
                oversized_endpoint,
                "cn-hangzhou",
                "abc",
                "/avatars",
                SecretString::new("id").expect("凭据非空"),
                SecretString::new("secret").expect("凭据非空"),
            ),
            Err(OssConfigError::VirtualHostTooLong)
        ));
        assert!(matches!(
            OssAdapterConfig::new(
                "https://oss-cn-hangzhou.aliyuncs.com",
                "CN-Hangzhou",
                "tsz-avatars",
                "/avatars",
                SecretString::new("id").expect("凭据非空"),
                SecretString::new("secret").expect("凭据非空"),
            ),
            Err(OssConfigError::InvalidRegion)
        ));

        let maximum_root = format!("/{}", "a".repeat(MAX_OSS_ROOT_BYTES - 1));
        OssAdapterConfig::new(
            "https://oss-cn-hangzhou.aliyuncs.com",
            "cn-hangzhou",
            "tsz-avatars",
            maximum_root,
            SecretString::new("id").expect("凭据非空"),
            SecretString::new("secret").expect("凭据非空"),
        )
        .expect("root 与最长 ObjectKey 组合后不超过 OSS 上限");
        let oversized_root = format!("/{}", "a".repeat(MAX_OSS_ROOT_BYTES));
        assert!(matches!(
            OssAdapterConfig::new(
                "https://oss-cn-hangzhou.aliyuncs.com",
                "cn-hangzhou",
                "tsz-avatars",
                oversized_root,
                SecretString::new("id").expect("凭据非空"),
                SecretString::new("secret").expect("凭据非空"),
            ),
            Err(OssConfigError::InvalidRoot)
        ));
    }

    #[test]
    fn put_metadata_uses_validated_request_values_missing_from_oss_response() {
        let upstream = Metadata::default().with_etag("etag-from-oss".to_owned());
        let options = BackendWriteOptions {
            content_type: Some("image/webp".to_owned()),
            cache_control: Some("private, max-age=60".to_owned()),
            content_length: 42,
        };

        let metadata = convert_write_metadata(&upstream, &options);

        assert_eq!(metadata.content_length, 42);
        assert_eq!(metadata.content_type.as_deref(), Some("image/webp"));
        assert_eq!(
            metadata.cache_control.as_deref(),
            Some("private, max-age=60")
        );
        assert_eq!(metadata.etag.as_deref(), Some("etag-from-oss"));
    }

    #[test]
    fn opendal_not_found_maps_to_stable_object_error() {
        let key = ObjectKey::parse("users/missing.webp").expect("固定键合法");
        let error = opendal::Error::new(ErrorKind::NotFound, "secret backend detail");

        assert_eq!(
            map_opendal_error(&space(), StorageOperation::Read, Some(&key), error),
            StorageError::ObjectNotFound {
                space: space(),
                key,
            }
        );
    }

    #[test]
    fn opendal_error_mapping_drops_backend_details() {
        let error = opendal::Error::new(
            ErrorKind::PermissionDenied,
            "AccessKeySecret=must-never-escape",
        );
        let mapped = map_opendal_error(&space(), StorageOperation::Put, None, error);

        assert_eq!(
            mapped,
            StorageError::Backend {
                space: space(),
                operation: StorageOperation::Put,
                kind: BackendErrorKind::AccessDenied,
            }
        );
        assert!(!mapped.to_string().contains("must-never-escape"));
    }

    #[test]
    fn opendal_rate_limit_and_unsupported_errors_remain_actionable() {
        let rate_limited = map_opendal_error(
            &space(),
            StorageOperation::Read,
            None,
            opendal::Error::new(ErrorKind::RateLimited, "slow down"),
        );
        let unsupported = map_opendal_error(
            &space(),
            StorageOperation::Copy,
            None,
            opendal::Error::new(ErrorKind::Unsupported, "not available"),
        );

        assert!(matches!(
            rate_limited,
            StorageError::Backend {
                kind: BackendErrorKind::RateLimited,
                ..
            }
        ));
        assert!(matches!(
            unsupported,
            StorageError::Unsupported {
                operation: StorageOperation::Copy,
                ..
            }
        ));
    }
}
