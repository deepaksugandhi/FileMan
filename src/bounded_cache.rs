use std::{borrow::Borrow, collections::HashMap, hash::Hash};

/// LRU storage shared by shell textures and directory-tree listings.
pub struct BoundedCache<K, V> {
    entries: HashMap<K, (V, usize, u64)>,
    bytes: usize,
    clock: u64,
    max_entries: usize,
    max_bytes: usize,
}

impl<K: Eq + Hash + Clone, V> BoundedCache<K, V> {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        assert!(max_entries > 0);
        Self {
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
            max_entries,
            max_bytes,
        }
    }

    pub fn get<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.clock += 1;
        self.entries.get_mut(key).map(|(value, _, used)| {
            *used = self.clock;
            &*value
        })
    }

    pub fn remove<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        self.entries.remove(key).map(|(value, bytes, _)| {
            self.bytes -= bytes;
            value
        })
    }

    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.remove(&key);
        // Keep one oversized directory usable without rescanning it every frame.
        // ponytail: eviction scans at most 256 entries; use an LRU list if raised.
        while !self.entries.is_empty()
            && (self.entries.len() >= self.max_entries
                || self.bytes.saturating_add(bytes) > self.max_bytes)
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, used))| used)
                .unwrap()
                .0
                .clone();
            self.remove(&oldest);
        }
        self.clock += 1;
        self.bytes += bytes;
        self.entries.insert(key, (value, bytes, self.clock));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicts_by_recency_and_bytes_and_accounts_for_replacements() {
        let mut cache = BoundedCache::new(2, 10);
        cache.insert("a", 1, 4);
        cache.insert("b", 2, 4);
        assert_eq!(cache.get("a"), Some(&1));
        cache.insert("c", 3, 4);
        assert_eq!(cache.get("b"), None);
        cache.insert("c", 4, 7);
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.bytes, 7);
        cache.insert("large", 5, 20);
        assert_eq!(cache.entries.len(), 1);
        cache.remove("large");
        assert_eq!(cache.bytes, 0);
    }
}
