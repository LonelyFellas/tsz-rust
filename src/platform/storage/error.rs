use thiserror::Error;

use super::model::{ObjectKey, StorageSpace};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageOperation {
    Put,
    Read,
    Stat,
    PresignRead,
    PresignWrite,
    Copy,
    Delete,
    Configure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendErrorKind {
    AccessDenied,
    RateLimited,
    TemporarilyUnavailable,
    Unexpected,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    #[error("storage space '{0}' is not configured")]
    SpaceNotConfigured(StorageSpace),
    #[error("storage space '{0}' is configured more than once")]
    DuplicateSpace(StorageSpace),
    #[error("object '{key}' was not found in storage space '{space}'")]
    ObjectNotFound { space: StorageSpace, key: ObjectKey },
    #[error("object size {actual} exceeds the {max} byte limit for storage space '{space}'")]
    ObjectTooLarge {
        space: StorageSpace,
        max: u64,
        actual: u64,
    },
    #[error("storage operation {operation:?} is not supported in space '{space}'")]
    Unsupported {
        space: StorageSpace,
        operation: StorageOperation,
    },
    #[error("storage backend rejected operation {operation:?} in space '{space}'")]
    Backend {
        space: StorageSpace,
        operation: StorageOperation,
        kind: BackendErrorKind,
    },
}

impl StorageError {
    pub(crate) fn not_found(space: &StorageSpace, key: &ObjectKey) -> Self {
        Self::ObjectNotFound {
            space: space.clone(),
            key: key.clone(),
        }
    }

    pub(crate) fn backend(
        space: &StorageSpace,
        operation: StorageOperation,
        kind: BackendErrorKind,
    ) -> Self {
        Self::Backend {
            space: space.clone(),
            operation,
            kind,
        }
    }
}
