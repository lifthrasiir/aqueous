//! The global object table: ids in first-occurrence order, and lookups by id.

use crate::obj::Id;

/// Ids, 20 bytes each, back to back (the `sha` stream).
#[derive(Clone, Copy)]
pub struct Ids<'a>(pub &'a [u8]);

impl<'a> Ids<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len() / 20
    }
    #[inline]
    pub fn get(&self, j: usize) -> &'a Id {
        self.0[20 * j..20 * j + 20].try_into().unwrap()
    }
}

const FILTER_BITS: u32 = 24;
const EMPTY: u32 = u32::MAX;

/// The ids, and an open-addressing index over them (4 bytes a slot, at most half full).
pub struct Table {
    pub ids: Vec<u8>,
    slots: Vec<u32>,
    /// One bit per hash of an id's first 8 bytes, to scan binary data for ids.
    filter: Vec<u64>,
}

#[inline]
fn fhash(prefix: u64) -> usize {
    (prefix.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - FILTER_BITS)) as usize
}

/// Ids are uniformly distributed: their first 8 bytes are a hash already.
#[inline]
fn slot_hash(id: &[u8]) -> usize {
    u64::from_le_bytes(id[..8].try_into().unwrap()) as usize
}

impl Table {
    pub fn new() -> Self {
        Table { ids: vec![], slots: vec![EMPTY; 1024], filter: vec![0; 1 << (FILTER_BITS - 6)] }
    }

    pub fn len(&self) -> usize {
        self.ids.len() / 20
    }

    pub fn id(&self, j: u32) -> &[u8] {
        &self.ids[20 * j as usize..20 * j as usize + 20]
    }

    /// The slot of `id`, or of the empty slot where it would go.
    #[inline]
    fn probe(&self, id: &[u8]) -> usize {
        let mask = self.slots.len() - 1;
        let mut s = slot_hash(id) & mask;
        loop {
            let j = self.slots[s];
            if j == EMPTY || self.id(j) == id {
                return s;
            }
            s = (s + 1) & mask;
        }
    }

    /// The index of `id`, added if new; and whether it was new.
    pub fn add(&mut self, id: &Id) -> (u32, bool) {
        let s = self.probe(id);
        if self.slots[s] != EMPTY {
            return (self.slots[s], false);
        }
        let j = self.len() as u32;
        self.ids.extend_from_slice(id);
        self.slots[s] = j;
        let h = fhash(u64::from_le_bytes(id[..8].try_into().unwrap()));
        self.filter[h >> 6] |= 1 << (h & 63);
        if self.len() * 2 > self.slots.len() {
            let n = self.slots.len() * 2;
            self.slots = vec![EMPTY; n];
            for j in 0..self.len() as u32 {
                let s = self.probe(self.id(j));
                self.slots[s] = j;
            }
        }
        (j, true)
    }

    #[inline]
    pub fn find(&self, id: &[u8]) -> Option<u32> {
        if id.len() != 20 {
            return None;
        }
        let j = self.slots[self.probe(id)];
        (j != EMPTY).then_some(j)
    }

    /// `find` for 20 bytes of arbitrary data, filtered first.
    #[inline]
    pub fn find_window(&self, w: &[u8]) -> Option<u32> {
        let h = fhash(u64::from_le_bytes(w[..8].try_into().unwrap()));
        if self.filter[h >> 6] & 1 << (h & 63) == 0 {
            return None;
        }
        self.find(&w[..20])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_find() {
        let mut t = Table::new();
        let ids: Vec<Id> = (0..5000u32).map(|i| crate::obj::sha1(&[&i.to_le_bytes()])).collect();
        for (k, id) in ids.iter().enumerate() {
            assert_eq!(t.add(id), (k as u32, true));
        }
        for (k, id) in ids.iter().enumerate() {
            assert_eq!(t.add(id), (k as u32, false));
            assert_eq!(t.find(id), Some(k as u32));
            assert_eq!(t.find_window(id), Some(k as u32));
        }
        assert_eq!(t.find(&[7; 20]), None);
    }
}
