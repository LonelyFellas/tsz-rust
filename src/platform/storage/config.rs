use std::{collections::BTreeMap, fmt, str::FromStr, time::Duration};

use thiserror::Error;

use super::{
    model::{CacheControl, StoragePolicy, StoragePrivacy, StorageSpace},
    oss::{OssAdapter, OssAdapterConfig, SecretString},
    registry::StorageRegistry,
};

const SPACE_FIELDS: [&str; 11] = [
    "BACKEND",
    "OSS_ENDPOINT",
    "OSS_REGION",
    "OSS_BUCKET",
    "OSS_ROOT",
    "OSS_ACCESS_KEY_ID",
    "OSS_ACCESS_KEY_SECRET",
    "PRIVACY",
    "MAX_OBJECT_SIZE_BYTES",
    "PRESIGN_TTL_SECONDS",
    "CACHE_CONTROL",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StorageConfigError {
    #[error("OBJECT_STORAGE_SPACES is required when any OBJECT_STORAGE_* variable is configured")]
    MissingSpaceList,
    #[error("OBJECT_STORAGE_SPACES must contain at least one space")]
    EmptySpaceList,
    #[error("storage space name '{name}' is invalid")]
    InvalidSpaceName { name: String },
    #[error("storage space '{space}' is listed more than once")]
    DuplicateSpace { space: String },
    #[error("storage spaces '{first}' and '{second}' map to the same environment prefix")]
    EnvironmentPrefixCollision { first: String, second: String },
    #[error("storage variable '{name}' is configured more than once")]
    DuplicateVariable { name: String },
    #[error("storage variable '{name}' does not belong to a configured space or known field")]
    UnknownVariable { name: String },
    #[error("storage space '{space}' is missing required field '{field}'")]
    MissingField { space: String, field: &'static str },
    #[error("storage space '{space}' has an invalid value for field '{field}'")]
    InvalidField { space: String, field: &'static str },
    #[error("storage space '{space}' uses unsupported backend '{backend}'")]
    UnsupportedBackend { space: String, backend: String },
    #[error("storage spaces '{first}' and '{second}' map to overlapping OSS bucket roots")]
    OverlappingOssRoot { first: String, second: String },
}

#[derive(Clone)]
struct OssBinding {
    space: StorageSpace,
    policy: StoragePolicy,
    adapter: OssAdapterConfig,
}

impl fmt::Debug for OssBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OssBinding")
            .field("space", &self.space)
            .field("policy", &self.policy)
            .field("adapter", &self.adapter)
            .finish()
    }
}

/// 经过 all-or-nothing 校验的对象存储配置。默认值为空、不会创建远端 client。
#[derive(Clone, Default)]
pub struct ObjectStorageConfig {
    bindings: Vec<OssBinding>,
}

impl fmt::Debug for ObjectStorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectStorageConfig")
            .field("bindings", &self.bindings)
            .finish()
    }
}

impl ObjectStorageConfig {
    pub fn from_pairs<I>(pairs: I) -> Result<Self, StorageConfigError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut variables = BTreeMap::new();
        for (name, value) in pairs {
            let name = name.to_ascii_uppercase();
            if !name.starts_with("OBJECT_STORAGE_") {
                continue;
            }
            if variables.insert(name.clone(), value).is_some() {
                return Err(StorageConfigError::DuplicateVariable { name });
            }
        }
        if variables.is_empty() {
            return Ok(Self::default());
        }

        let space_list = variables
            .get("OBJECT_STORAGE_SPACES")
            .ok_or(StorageConfigError::MissingSpaceList)?;
        let names: Vec<&str> = space_list.split(',').map(str::trim).collect();
        if names.is_empty() || names.iter().any(|name| name.is_empty()) {
            return Err(StorageConfigError::EmptySpaceList);
        }

        let mut spaces = Vec::with_capacity(names.len());
        let mut prefixes = BTreeMap::new();
        for name in names {
            let space =
                StorageSpace::from_str(name).map_err(|_| StorageConfigError::InvalidSpaceName {
                    name: name.to_owned(),
                })?;
            if spaces
                .iter()
                .any(|existing: &StorageSpace| existing == &space)
            {
                return Err(StorageConfigError::DuplicateSpace {
                    space: name.to_owned(),
                });
            }
            let prefix = environment_prefix(&space);
            if let Some(first) = prefixes.insert(prefix, space.clone()) {
                return Err(StorageConfigError::EnvironmentPrefixCollision {
                    first: first.to_string(),
                    second: space.to_string(),
                });
            }
            spaces.push(space);
        }

        let allowed_variables = spaces
            .iter()
            .flat_map(|space| {
                let prefix = environment_prefix(space);
                SPACE_FIELDS.map(move |field| format!("{prefix}_{field}"))
            })
            .chain(std::iter::once("OBJECT_STORAGE_SPACES".to_owned()))
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(name) = variables
            .keys()
            .find(|name| !allowed_variables.contains(*name))
        {
            return Err(StorageConfigError::UnknownVariable { name: name.clone() });
        }

        let mut bindings = Vec::with_capacity(spaces.len());
        for space in spaces {
            bindings.push(parse_binding(&variables, space)?);
        }
        reject_overlapping_oss_roots(&bindings)?;
        Ok(Self { bindings })
    }

    pub fn build_registry(&self) -> Result<StorageRegistry, super::error::StorageError> {
        let stores = self
            .bindings
            .iter()
            .map(|binding| {
                OssAdapter::object_store(
                    binding.space.clone(),
                    binding.policy.clone(),
                    binding.adapter.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        StorageRegistry::from_stores(stores)
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

fn reject_overlapping_oss_roots(bindings: &[OssBinding]) -> Result<(), StorageConfigError> {
    for (index, first) in bindings.iter().enumerate() {
        for second in &bindings[index + 1..] {
            if first.adapter.bucket() == second.adapter.bucket()
                && roots_overlap(first.adapter.root(), second.adapter.root())
            {
                return Err(StorageConfigError::OverlappingOssRoot {
                    first: first.space.to_string(),
                    second: second.space.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn roots_overlap(first: &str, second: &str) -> bool {
    first == second
        || first == "/"
        || second == "/"
        || first
            .strip_prefix(second)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || second
            .strip_prefix(first)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn parse_binding(
    variables: &BTreeMap<String, String>,
    space: StorageSpace,
) -> Result<OssBinding, StorageConfigError> {
    let prefix = environment_prefix(&space);
    let get = |field: &'static str| {
        variables.get(&format!("{prefix}_{field}")).ok_or_else(|| {
            StorageConfigError::MissingField {
                space: space.to_string(),
                field,
            }
        })
    };

    let backend = get("BACKEND")?;
    if backend != "oss" {
        return Err(StorageConfigError::UnsupportedBackend {
            space: space.to_string(),
            backend: backend.clone(),
        });
    }

    let privacy = get("PRIVACY")?.parse::<StoragePrivacy>().map_err(|_| {
        StorageConfigError::InvalidField {
            space: space.to_string(),
            field: "PRIVACY",
        }
    })?;
    let max_object_size = parse_u64(
        get("MAX_OBJECT_SIZE_BYTES")?,
        &space,
        "MAX_OBJECT_SIZE_BYTES",
    )?;
    let ttl_seconds = parse_u64(get("PRESIGN_TTL_SECONDS")?, &space, "PRESIGN_TTL_SECONDS")?;
    let cache_control = match get("CACHE_CONTROL")?.as_str() {
        "none" => None,
        value => {
            Some(
                CacheControl::parse(value).map_err(|_| StorageConfigError::InvalidField {
                    space: space.to_string(),
                    field: "CACHE_CONTROL",
                })?,
            )
        }
    };
    let policy = StoragePolicy::new(
        privacy,
        max_object_size,
        Duration::from_secs(ttl_seconds),
        cache_control,
    )
    .map_err(|_| StorageConfigError::InvalidField {
        space: space.to_string(),
        field: if max_object_size == 0 {
            "MAX_OBJECT_SIZE_BYTES"
        } else {
            "PRESIGN_TTL_SECONDS"
        },
    })?;

    let access_key_id = SecretString::new(get("OSS_ACCESS_KEY_ID")?.clone()).map_err(|_| {
        StorageConfigError::InvalidField {
            space: space.to_string(),
            field: "OSS_ACCESS_KEY_ID",
        }
    })?;
    let access_key_secret =
        SecretString::new(get("OSS_ACCESS_KEY_SECRET")?.clone()).map_err(|_| {
            StorageConfigError::InvalidField {
                space: space.to_string(),
                field: "OSS_ACCESS_KEY_SECRET",
            }
        })?;
    let adapter = OssAdapterConfig::new(
        get("OSS_ENDPOINT")?.clone(),
        get("OSS_REGION")?.clone(),
        get("OSS_BUCKET")?.clone(),
        get("OSS_ROOT")?.clone(),
        access_key_id,
        access_key_secret,
    )
    .map_err(|error| StorageConfigError::InvalidField {
        space: space.to_string(),
        field: match error {
            super::oss::OssConfigError::InvalidEndpoint => "OSS_ENDPOINT",
            super::oss::OssConfigError::VirtualHostTooLong => "OSS_ENDPOINT",
            super::oss::OssConfigError::InvalidRegion => "OSS_REGION",
            super::oss::OssConfigError::InvalidBucket => "OSS_BUCKET",
            super::oss::OssConfigError::InvalidRoot => "OSS_ROOT",
            super::oss::OssConfigError::EmptyCredential => "OSS_ACCESS_KEY_SECRET",
        },
    })?;

    Ok(OssBinding {
        space,
        policy,
        adapter,
    })
}

fn parse_u64(
    value: &str,
    space: &StorageSpace,
    field: &'static str,
) -> Result<u64, StorageConfigError> {
    value
        .parse::<u64>()
        .map_err(|_| StorageConfigError::InvalidField {
            space: space.to_string(),
            field,
        })
}

fn environment_prefix(space: &StorageSpace) -> String {
    format!(
        "OBJECT_STORAGE_{}",
        space.as_str().to_ascii_uppercase().replace('-', "_")
    )
}
