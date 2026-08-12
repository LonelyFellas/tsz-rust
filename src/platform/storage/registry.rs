use std::{collections::HashMap, sync::Arc};

use super::{error::StorageError, model::StorageSpace, service::ObjectStore};

/// 启动时构建、运行期只读的空间注册表。
#[derive(Clone, Default)]
pub struct StorageRegistry {
    stores: Arc<HashMap<StorageSpace, Arc<dyn ObjectStore>>>,
}

impl StorageRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_stores<I>(stores: I) -> Result<Self, StorageError>
    where
        I: IntoIterator<Item = Arc<dyn ObjectStore>>,
    {
        let mut by_space = HashMap::new();
        for store in stores {
            let space = store.space().clone();
            if by_space.insert(space.clone(), store).is_some() {
                return Err(StorageError::DuplicateSpace(space));
            }
        }
        Ok(Self {
            stores: Arc::new(by_space),
        })
    }

    pub fn get(&self, space: &StorageSpace) -> Result<Arc<dyn ObjectStore>, StorageError> {
        self.stores
            .get(space)
            .cloned()
            .ok_or_else(|| StorageError::SpaceNotConfigured(space.clone()))
    }

    pub fn len(&self) -> usize {
        self.stores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }
}
