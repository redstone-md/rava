//! Positive metadata memoization, invalidated by the loader's hierarchy epoch.
use crate::{FastMap, LoadedClass};
use std::sync::Arc;

pub(crate) type Key = (usize, u16, u8, u32);

#[derive(Clone)]
pub(crate) struct Target {
    pub class: Arc<LoadedClass>,
    pub method: usize,
}

#[derive(Default)]
pub(crate) struct Cache {
    epoch: u64,
    entries: FastMap<Key, Target>,
}

impl Cache {
    pub fn get(&self, epoch: u64, key: &Key) -> Option<Target> {
        if epoch != self.epoch {
            return None;
        }
        self.entries.get(key).cloned()
    }

    pub fn insert(&mut self, epoch: u64, key: Key, target: Target) {
        if epoch != self.epoch {
            self.entries.clear();
            self.epoch = epoch;
        }
        self.entries.insert(key, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class_loader::ClassLoader;
    use rava_classfile::{ClassFile, Constant, ConstantPool};

    #[test]
    fn direct_targets_are_invalidated_when_loader_epoch_changes() {
        let loader = ClassLoader::new();
        let class = loader.define_synthetic(ClassFile::synthesize(
            ConstantPool::from_entries(vec![Constant::Unusable]),
            0x21,
            "AotDirectCacheTest".into(),
            None,
            Vec::new(),
            Vec::new(),
        ));
        let key = (1, 2, 0, 0);
        let mut cache = Cache::default();

        cache.insert(
            4,
            key,
            Target {
                class: Arc::clone(&class),
                method: 3,
            },
        );
        assert_eq!(cache.get(4, &key).unwrap().method, 3);
        assert!(cache.get(5, &key).is_none());

        cache.insert(5, key, Target { class, method: 7 });
        assert_eq!(cache.get(5, &key).unwrap().method, 7);
        assert!(cache.get(4, &key).is_none());
    }
}
