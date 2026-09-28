//! Guest maps: the index, the salt counters, and the operations over both.

use crate::heap::*;

impl Heap {
    pub fn alloc_map(&mut self) -> u32 {
        if crate::m3heap::probe_on() {
            crate::m3heap::record_alloc("map");
        }
        self.bytes.fetch_add(32, std::sync::atomic::Ordering::Relaxed);
        if let Some(id) = self.free_map.pop() {
            self.maps[id as usize] = MapObj::default();
            self.map_alive[id as usize] = true;
            id
        } else {
            let id = self.maps.len() as u32;
            self.maps.push(MapObj::default());
            self.map_alive.push(true);
            id
        }
    }

    pub fn map_get(&self, id: u32, key: &MapKey) -> Option<Value> {
        let m = self.maps.get(id as usize)?;
        let idx = *m.index.get(key)?;
        m.entries.get(idx).map(|(_, v)| v.clone())
    }

    /// The STORED key value for `key` (the canonical instance first inserted under
    /// this structural key). Used by guava's interner, whose `getEntry(sample)
    /// .getKey()` must return the canonical object, not the lookup sample.
    #[cfg(feature = "next-gen-gc")]
    pub fn map_get_key(&self, id: u32, key: &MapKey) -> Option<Value> {
        let m = self.maps.get(id as usize)?;
        let idx = *m.index.get(key)?;
        m.entries.get(idx).map(|(k, _)| k.clone())
    }

    pub fn map_contains(&self, id: u32, key: &MapKey) -> bool {
        self.maps
            .get(id as usize)
            .map(|m| m.index.contains_key(key))
            .unwrap_or(false)
    }

    /// Insert/replace; returns the previous value if any.
    ///
    /// ONE PROBE, NOT TWO. The shape this replaces asked the index `get(&key)` and then, on the
    /// miss, `insert(key, idx)` — a second hash of the same key and a second walk of the same
    /// probe sequence, on the branch that is by definition a table MISS and therefore already the
    /// expensive one (a miss must probe to the end of its run before it can conclude anything).
    /// String keys pay that twice over: FxHash folds the whole `Box<str>` both times. `entry`
    /// resolves the slot ONCE and hands back the place to write, so an insert now costs what a
    /// lookup costs. Nothing about which key is stored changes: `HashMap::entry` keeps the key
    /// already in the table on the occupied path, exactly as `insert` did.
    ///
    /// `RAVA_NO_FAST_MAPTABLE=1` restores the two-probe form below.
    pub fn map_put(&mut self, id: u32, key: MapKey, key_val: Value, val: Value) -> Option<Value> {
        if !crate::map::table::fast_maptable_enabled() {
            return self.map_put_two_probe(id, key, key_val, val);
        }
        use std::collections::hash_map::Entry;
        let added = key.entry_bytes();
        let m = self.maps.get_mut(id as usize)?;
        let (out, inserted) = match m.index.entry(key) {
            Entry::Occupied(slot) => {
                let idx = *slot.get();
                // A REPLACE still changes what a derived view holds — the entry's VALUE is part
                // of the snapshot `entrySet()` materialises — so it bumps the counter exactly
                // like an insert does.
                m.version = m.version.wrapping_add(1);
                let prev = m
                    .entries
                    .get_mut(idx)
                    .map(|(_, cur)| std::mem::replace(cur, val));
                (prev, false)
            }
            Entry::Vacant(slot) => {
                m.version = m.version.wrapping_add(1);
                let idx = m.entries.len();
                // GAP-P04: a freshly INSERTED salted key advances the map's monotonic salt
                // counter (mints do not store — only live entries define the chain length).
                if let MapKey::Str(s) = slot.key() {
                    Self::note_salt(&mut m.salt_next, s);
                }
                m.entries.push((key_val, val));
                slot.insert(idx);
                (None, true)
            }
        };
        if inserted {
            self.bytes.fetch_add(added, std::sync::atomic::Ordering::Relaxed);
        }
        out
    }

    /// The pre-`entry` put, kept verbatim so `RAVA_NO_FAST_MAPTABLE` prices the change as a
    /// same-binary A/B rather than a rebuild.
    fn map_put_two_probe(
        &mut self,
        id: u32,
        key: MapKey,
        key_val: Value,
        val: Value,
    ) -> Option<Value> {
        let added = key.entry_bytes();
        let m = self.maps.get_mut(id as usize)?;
        if let Some(&idx) = m.index.get(&key) {
            m.version = m.version.wrapping_add(1);
            return m
                .entries
                .get_mut(idx)
                .map(|(_, slot)| std::mem::replace(slot, val));
        }
        m.version = m.version.wrapping_add(1);
        let idx = m.entries.len();
        if let MapKey::Str(s) = &key {
            Self::note_salted_key(m, s);
        }
        m.entries.push((key_val, val));
        m.index.insert(key, idx);
        self.bytes.fetch_add(added, std::sync::atomic::Ordering::Relaxed);
        None
    }

    /// GAP-P04: parse a salted key string and raise the map's `(class, hc)` counter
    /// to `salt + 1` (also the restore-rebuild primitive). Non-salted keys no-op.
    pub(crate) fn note_salted_key(m: &mut MapObj, s: &str) {
        Self::note_salt(&mut m.salt_next, s);
    }

    /// Same thing against the salt field ALONE, so the insert path can hold a `VacantEntry`
    /// on `index` (a disjoint field) while it runs. Splitting the borrow is the whole reason
    /// this exists — nothing about the parse changed.
    pub(crate) fn note_salt(
        salt_next: &mut crate::FastMap<Box<str>, crate::FastMap<i32, u32>>,
        s: &str,
    ) {
        let Some(rest) = s.strip_prefix("o:gS") else { return };
        let Some((head, salt)) = rest.rsplit_once('#') else { return };
        let Some((class, hc)) = head.rsplit_once(':') else { return };
        let (Ok(salt), Ok(hc)) = (salt.parse::<u32>(), hc.parse::<i32>()) else { return };
        let per = salt_next.entry(Box::from(class)).or_default();
        let slot = per.entry(hc).or_insert(0);
        if *slot <= salt {
            *slot = salt + 1;
        }
    }

    /// Remove by key, returning the value.
    ///
    /// The renumber that follows used to be an UNCONDITIONAL walk of every bucket the index
    /// table owns — empty buckets included — because entry positions are dense and everything
    /// above `pos` slides down. It ran even when `pos` was the last entry and nothing moved.
    /// `shift_down_after` computes the exact number of slots that can change (`n - 1 - pos`)
    /// and stops there; the newest-entry delete does not touch the table at all, and a `pop`
    /// replaces the tail memmove. See `map::table` for the argument and the kill switch.
    pub fn map_remove(&mut self, id: u32, key: &MapKey) -> Option<Value> {
        let (value, removed_bytes) = {
            let m = self.maps.get_mut(id as usize)?;
            let (stored_key, pos) = m.index.remove_entry(key)?;
            m.version = m.version.wrapping_add(1);
            let n_before = m.entries.len();
            let value = if pos + 1 == n_before {
                m.entries
                    .pop()
                    .expect("index position implies a non-empty entry vector")
                    .1
            } else {
                m.entries.remove(pos).1
            };
            crate::map::table::shift_down_after(&mut m.index, pos, n_before);
            (value, stored_key.entry_bytes())
        };
        {
            let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
            self.bytes.store(cur.saturating_sub(removed_bytes), std::sync::atomic::Ordering::Relaxed);
        }
        Some(value)
    }

    /// Move an existing key's entry to the FRONT of insertion order (its value
    /// unchanged), returning that value. Backs fastutil Linked-map LRU reorder
    /// (getAndMoveToFirst / putAndMoveToFirst). Only the shifted positions are
    /// re-indexed (idx<pos → +1, moved → 0, idx>pos unchanged), so no MapKey
    /// recompute is needed. Returns None if the key is absent.
    pub fn map_move_to_first(&mut self, id: u32, key: &MapKey) -> Option<Value> {
        let m = self.maps.get_mut(id as usize)?;
        let pos = *m.index.get(key)?;
        let val = m.entries.get(pos).map(|(_, v)| v.clone());
        if pos != 0 {
            m.version = m.version.wrapping_add(1); // iteration ORDER is view-visible
            // `remove(pos)` + `insert(0, e)` is TWO memmoves over the same prefix plus a
            // temporary; rotating the prefix right by one is the same permutation, once.
            m.entries[..=pos].rotate_right(1);
            // …and this replaces the full-table walk AND the `key.clone()` that followed it —
            // that clone allocated a fresh `Box<str>` copy of a key the table already owns, on
            // every LRU touch, only to re-hash and re-probe for a slot the walk had passed.
            crate::map::table::rotate_to_front(&mut m.index, pos);
        }
        val
    }

    /// The twin of `map_move_to_first`: move an existing key's entry to the END of insertion
    /// order, value unchanged, returning that value.
    ///
    /// THIS IS THE ACCESS-ORDERED `LinkedHashMap` HIT — java's LRU moves the entry to the end on
    /// `get`, and the way that was expressed here was `map_remove` followed by `map_put`. That
    /// pair costs: a hash+probe to delete, a FULL-TABLE renumbering walk, a `Vec::remove`
    /// memmove, a `Vec::push`, a hash+probe to re-insert — and at the call site, a SECOND
    /// derivation of the `MapKey` itself, which for an object key is the expensive half of the
    /// whole operation. Here it is one budgeted walk (`n - pos` slots) and one tail rotate, with
    /// the key derived once and never re-inserted.
    ///
    /// Iteration order after the move is byte-identical to remove+put: the entry lands last,
    /// everything above it slides down one, everything below is untouched. `None` if absent.
    pub fn map_move_to_last(&mut self, id: u32, key: &MapKey) -> Option<Value> {
        let m = self.maps.get_mut(id as usize)?;
        let pos = *m.index.get(key)?;
        let n = m.entries.len();
        let val = m.entries.get(pos).map(|(_, v)| v.clone());
        if pos + 1 < n {
            m.version = m.version.wrapping_add(1); // iteration ORDER is view-visible
            m.entries[pos..].rotate_left(1);
            crate::map::table::rotate_to_back(&mut m.index, pos, n);
        }
        val
    }

    /// Read the receiver's access policy without publishing a lazy cache on a worker.
    pub(crate) fn map_access_order(&self, owner: u32, id: u32) -> bool {
        self.maps.get(id as usize).and_then(|m| m.access_order)
            .unwrap_or_else(|| self.get_field(owner, "$accessOrder").as_int() == Some(1))
    }

    /// Serial native lookup. Reordering keeps the stored key identity and uses the
    /// already resolved MapKey; it must not invoke guest hashCode/equals again.
    pub(crate) fn map_get_access(&mut self, owner: u32, id: u32, key: &MapKey) -> Option<Value> {
        let ordered = self.map_access_order(owner, id);
        if let Some(m) = self.maps.get_mut(id as usize) {
            if m.access_order.is_none() {
                m.access_order = Some(ordered);
            }
        }
        if ordered { self.map_move_to_last(id, key) } else { self.map_get(id, key) }
    }

    pub fn map_size(&self, id: u32) -> usize {
        self.maps.get(id as usize).map(MapObj::len).unwrap_or(0)
    }

    /// GAP-P04: index-only probe — the entry slot `key` occupies in map `id`, if any.
    pub(crate) fn map_index_lookup(&self, id: u32, key: &MapKey) -> Option<usize> {
        self.maps.get(id as usize)?.index.get(key).copied()
    }

    /// GAP-P04: the ORIGINAL key object stored at entry `i` of map `id` (the live
    /// equals-verification target for salted probing).
    pub(crate) fn map_entry_key(&self, id: u32, i: usize) -> Option<Value> {
        self.maps.get(id as usize)?.entries.get(i).map(|(k, _)| k.clone())
    }

    /// GAP-P04: the current next-salt for `(class, hc)` in map `id` (0 when unseen).
    pub(crate) fn map_salt_next(&self, id: u32, class: &str, hc: i32) -> u32 {
        self.maps
            .get(id as usize)
            .and_then(|m| m.salt_next.get(class))
            .and_then(|per| per.get(&hc).copied())
            .unwrap_or(0)
    }

    /// GAP-P04: rebuild a map's salt counters from its salted index keys
    /// ("o:gS{class}:{hc}#{n}") — used after restore, where `salt_next` is not on
    /// the wire. Deterministic: counters = max seen salt + 1 per (class, hc).
    pub(crate) fn rebuild_salt_counters(m: &mut MapObj) {
        let keys: Vec<Box<str>> = m
            .index
            .keys()
            .filter_map(|k| match k {
                MapKey::Str(s) if s.starts_with("o:gS") => Some(s.clone()),
                _ => None,
            })
            .collect();
        for s in keys {
            Self::note_salted_key(m, &s);
        }
    }

    pub fn map_clear(&mut self, id: u32) {
        let removed_bytes = if let Some(m) = self.maps.get_mut(id as usize) {
            let removed = m.index.keys().map(MapKey::entry_bytes).sum();
            m.version = m.version.wrapping_add(1);
            m.entries.clear();
            m.index.clear();
            removed
        } else {
            0
        };
        {
            let cur = self.bytes.load(std::sync::atomic::Ordering::Relaxed);
            self.bytes.store(cur.saturating_sub(removed_bytes), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// This map's mutation counter (see `MapObj::version`). A caller that derived something
    /// from `map_entries` and kept this number may reuse the derivation while it still matches.
    /// A missing map answers 0, which no live map's reader can be holding a match against
    /// (the counter starts at 0 but the reader also had to have READ a map to hold one).
    pub fn map_version(&self, id: u32) -> u32 {
        self.maps.get(id as usize).map(|m| m.version).unwrap_or(0)
    }

    /// (key, value) pairs in insertion order.
    pub fn map_entries(&self, id: u32) -> Vec<(Value, Value)> {
        self.maps.get(id as usize)
            .map(|m| m.entries.clone())
            .unwrap_or_default()
    }

    /// The same pairs, BORROWED — one Vec allocation and one `Value::clone` per mapping
    /// (an `Arc` bump on every ref-shaped entry) that a read-only caller never needed.
    ///
    /// `map_entries` exists because most callers hand the pairs to something that allocates
    /// guest objects and therefore cannot hold a heap borrow. The callers that only COUNT or
    /// SCAN can: `size`, `isEmpty` and `containsValue` currently materialise the entire map to
    /// read a length or run one comparison, which is the single largest per-call allocation on
    /// the native map surface and grows with the map. `map_size` already answers length without
    /// a copy; this answers the scans. See the hand-off note — the call sites live elsewhere.
    #[allow(dead_code)]
    pub fn map_entries_ref(&self, id: u32) -> &[(Value, Value)] {
        self.maps
            .get(id as usize)
            .map(|m| m.entries.as_slice())
            .unwrap_or(&[])
    }
}
