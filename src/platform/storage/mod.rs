mod config;
mod error;
mod memory;
mod model;
mod oss;
mod registry;
mod service;

pub use config::{ObjectStorageConfig, StorageConfigError};
pub use error::{BackendErrorKind, StorageError, StorageOperation};
pub use memory::MemoryAdapter;
pub use model::{
    CacheControl, MAX_PRESIGN_TTL, ObjectContentType, ObjectKey, ObjectKeyError, ObjectMetadata,
    PresignedRequest, PutOptions, StoragePolicy, StoragePolicyError, StoragePrivacy, StorageSpace,
    StorageSpaceError,
};
pub use oss::{OssAdapter, OssAdapterConfig, OssConfigError, SecretString};
pub use registry::StorageRegistry;
pub use service::ObjectStore;
