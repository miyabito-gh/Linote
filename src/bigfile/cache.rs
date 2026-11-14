//! ブロックキャッシュ（LRU）。保持するバイト数は「ブロック長 × 最大ブロック数」を超えない。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

struct Entry {
    data: Arc<[u8]>,
    tick: u64,
}

pub(crate) struct BlockCache {
    map: HashMap<u64, Entry>,
    /// 使った順（古い順）→ ブロック番号。
    order: BTreeMap<u64, u64>,
    tick: u64,
    max_blocks: usize,
    bytes: u64,
}

impl BlockCache {
    pub(crate) fn new(max_blocks: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            max_blocks: max_blocks.max(1),
            bytes: 0,
        }
    }

    pub(crate) fn get(&mut self, index: u64) -> Option<Arc<[u8]>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(&index)?;
        self.order.remove(&entry.tick);
        entry.tick = tick;
        self.order.insert(tick, index);
        Some(Arc::clone(&entry.data))
    }

    pub(crate) fn insert(&mut self, index: u64, data: Arc<[u8]>) {
        self.remove(index);
        while self.map.len() >= self.max_blocks {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(old) = self.map.remove(&oldest) {
                self.bytes -= old.data.len() as u64;
            }
        }
        self.tick += 1;
        self.bytes += data.len() as u64;
        self.order.insert(self.tick, index);
        self.map.insert(
            index,
            Entry {
                data,
                tick: self.tick,
            },
        );
    }

    fn remove(&mut self, index: u64) {
        if let Some(old) = self.map.remove(&index) {
            self.order.remove(&old.tick);
            self.bytes -= old.data.len() as u64;
        }
    }

    /// すべて捨て、確保した領域も返す。
    pub(crate) fn clear(&mut self) {
        self.map = HashMap::new();
        self.order = BTreeMap::new();
        self.bytes = 0;
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn blocks(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(n: usize) -> Arc<[u8]> {
        vec![0u8; n].into()
    }

    #[test]
    fn least_recently_used_block_is_dropped_first() {
        let mut cache = BlockCache::new(2);
        cache.insert(1, block(10));
        cache.insert(2, block(10));
        assert!(cache.get(1).is_some());
        cache.insert(3, block(10));
        assert!(cache.get(2).is_none());
        assert!(cache.get(1).is_some() && cache.get(3).is_some());
        assert_eq!((cache.blocks(), cache.bytes()), (2, 20));
    }

    #[test]
    fn replacing_and_clearing_keep_the_byte_count_right() {
        let mut cache = BlockCache::new(4);
        cache.insert(1, block(10));
        cache.insert(1, block(7));
        assert_eq!((cache.blocks(), cache.bytes()), (1, 7));
        cache.clear();
        assert_eq!((cache.blocks(), cache.bytes()), (0, 0));
        assert!(cache.get(1).is_none());
    }
}
