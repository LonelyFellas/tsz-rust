use std::{collections::BTreeMap, fmt, str::FromStr, time::Duration, time::SystemTime};

use thiserror::Error;
use uuid::Uuid;

pub(crate) const MAX_OBJECT_KEY_BYTES: usize = 512;
const MAX_SPACE_NAME_BYTES: usize = 64;
const MAX_HEADER_VALUE_BYTES: usize = 256;
const MAX_PRESIGN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ObjectKeyError {
    #[error("object key must not be empty")]
    Empty,
    #[error("object key exceeds {MAX_OBJECT_KEY_BYTES} bytes")]
    TooLong,
    #[error("object key must be relative")]
    Absolute,
    #[error("object key contains an empty, current-directory, or parent-directory segment")]
    UnsafeSegment,
    #[error("object key contains a character outside the portable allowlist")]
    InvalidCharacter,
    #[error("object key extension must contain only lowercase ASCII letters and digits")]
    InvalidExtension,
}

/// 经过校验、相对于空间 root 的逻辑对象键。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, ObjectKeyError> {
        let value = value.into();
        validate_object_key(&value)?;
        Ok(Self(value))
    }

    /// 生成服务端对象键。`namespace` 必须是安全的相对路径，扩展名不含点。
    pub fn generate(namespace: &str, extension: Option<&str>) -> Result<Self, ObjectKeyError> {
        validate_object_key(namespace)?;
        let extension = match extension {
            Some(extension)
                if !extension.is_empty()
                    && extension
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()) =>
            {
                format!(".{extension}")
            }
            Some(_) => return Err(ObjectKeyError::InvalidExtension),
            None => String::new(),
        };
        Self::parse(format!("{namespace}/{}{}", Uuid::now_v7(), extension))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ObjectKey").field(&self.0).finish()
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ObjectKey {
    type Error = ObjectKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for ObjectKey {
    type Error = ObjectKeyError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

fn validate_object_key(value: &str) -> Result<(), ObjectKeyError> {
    if value.is_empty() {
        return Err(ObjectKeyError::Empty);
    }
    if value.len() > MAX_OBJECT_KEY_BYTES {
        return Err(ObjectKeyError::TooLong);
    }
    if value.starts_with('/') || value.starts_with('\\') {
        return Err(ObjectKeyError::Absolute);
    }
    if value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ObjectKeyError::UnsafeSegment);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(ObjectKeyError::InvalidCharacter);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageSpaceError {
    #[error("storage space name must not be empty")]
    Empty,
    #[error("storage space name exceeds {MAX_SPACE_NAME_BYTES} bytes")]
    TooLong,
    #[error(
        "storage space name must start with a lowercase letter or digit and contain only lowercase letters, digits, '-' or '_'"
    )]
    InvalidCharacter,
}

/// registry 中稳定、可序列化到配置的空间标识。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorageSpace(String);

impl StorageSpace {
    pub fn parse(value: impl Into<String>) -> Result<Self, StorageSpaceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(StorageSpaceError::Empty);
        }
        if value.len() > MAX_SPACE_NAME_BYTES {
            return Err(StorageSpaceError::TooLong);
        }
        let mut bytes = value.bytes();
        let first = bytes.next().expect("已校验非空");
        if !(first.is_ascii_lowercase() || first.is_ascii_digit())
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(StorageSpaceError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StorageSpace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StorageSpace")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for StorageSpace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for StorageSpace {
    type Err = StorageSpaceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePrivacy {
    Private,
    PublicRead,
}

impl FromStr for StoragePrivacy {
    type Err = StoragePolicyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "private" => Ok(Self::Private),
            "public-read" => Ok(Self::PublicRead),
            _ => Err(StoragePolicyError::InvalidPrivacy),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StoragePolicyError {
    #[error("storage privacy must be 'private' or 'public-read'")]
    InvalidPrivacy,
    #[error("maximum object size must be greater than zero")]
    ZeroMaxObjectSize,
    #[error("presign TTL must be between 1 second and 24 hours")]
    InvalidPresignTtl,
    #[error("cache-control must be a non-empty visible ASCII header value")]
    InvalidCacheControl,
    #[error("content type must be a visible ASCII media type")]
    InvalidContentType,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CacheControl(String);

impl CacheControl {
    pub fn parse(value: impl Into<String>) -> Result<Self, StoragePolicyError> {
        let value = value.into();
        if !is_safe_header_value(&value) {
            return Err(StoragePolicyError::InvalidCacheControl);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CacheControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CacheControl")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ObjectContentType(String);

impl ObjectContentType {
    pub fn parse(value: impl Into<String>) -> Result<Self, StoragePolicyError> {
        let value = value.into();
        let media_type = value.split(';').next().unwrap_or_default();
        let valid_media_type = media_type
            .split_once('/')
            .is_some_and(|(kind, subtype)| is_http_token(kind) && is_http_token(subtype));
        if !is_safe_header_value(&value) || !valid_media_type {
            return Err(StoragePolicyError::InvalidContentType);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

impl fmt::Debug for ObjectContentType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ObjectContentType")
            .field(&self.0)
            .finish()
    }
}

fn is_safe_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_HEADER_VALUE_BYTES
        && value.bytes().all(|byte| matches!(byte, 0x20..=0x7e))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePolicy {
    privacy: StoragePrivacy,
    max_object_size: u64,
    presign_ttl: Duration,
    cache_control: Option<CacheControl>,
}

impl StoragePolicy {
    pub fn new(
        privacy: StoragePrivacy,
        max_object_size: u64,
        presign_ttl: Duration,
        cache_control: Option<CacheControl>,
    ) -> Result<Self, StoragePolicyError> {
        if max_object_size == 0 {
            return Err(StoragePolicyError::ZeroMaxObjectSize);
        }
        if presign_ttl.is_zero() || presign_ttl > MAX_PRESIGN_TTL {
            return Err(StoragePolicyError::InvalidPresignTtl);
        }
        Ok(Self {
            privacy,
            max_object_size,
            presign_ttl,
            cache_control,
        })
    }

    pub fn privacy(&self) -> StoragePrivacy {
        self.privacy
    }

    pub fn max_object_size(&self) -> u64 {
        self.max_object_size
    }

    pub fn presign_ttl(&self) -> Duration {
        self.presign_ttl
    }

    pub fn cache_control(&self) -> Option<&CacheControl> {
        self.cache_control.as_ref()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PutOptions {
    content_type: Option<ObjectContentType>,
}

impl PutOptions {
    pub fn new(content_type: Option<ObjectContentType>) -> Self {
        Self { content_type }
    }

    pub fn content_type(&self) -> Option<&ObjectContentType> {
        self.content_type.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub content_length: u64,
    pub content_type: Option<String>,
    pub cache_control: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
}

/// 可直接交给 HTTP client 的预签名请求。`Debug` 永不暴露 URL 或 header 值。
#[derive(Clone, PartialEq, Eq)]
pub struct PresignedRequest {
    method: String,
    url: String,
    pub(crate) headers: BTreeMap<String, String>,
    expires_in: Duration,
}

impl PresignedRequest {
    pub(crate) fn new(
        method: impl Into<String>,
        url: impl Into<String>,
        headers: BTreeMap<String, String>,
        expires_in: Duration,
    ) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers,
            expires_in,
        }
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    /// URL 含签名秘密；调用方不得记录返回值。
    pub fn url(&self) -> &str {
        &self.url
    }

    /// header 值可能参与签名；调用方不得记录返回值。
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn expires_in(&self) -> Duration {
        self.expires_in
    }
}

impl fmt::Debug for PresignedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PresignedRequest")
            .field("method", &self.method)
            .field("url", &"[REDACTED]")
            .field("header_count", &self.headers.len())
            .field("expires_in", &self.expires_in)
            .finish()
    }
}
