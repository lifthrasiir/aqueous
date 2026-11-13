//! The dispatch table of the modules merged into a stub (`aqr-ld merge`), and the anchor that
//! tells where it is. The merger writes both; the stub, `aqr-sfx` and `aqr-dll-shim` read them.
//!
//! The table is the count (`u32`), an entry each ([`ENTRY_LEN`] bytes: six `u32`, in the order of
//! [`Entry`]'s fields, the last the offset of the name in the table), then the names (each its
//! length, `u32`, and its bytes).

/// The magic of [`Anchor`], by which `aqr-ld merge` finds it in a stub's file.
pub const ANCHOR_MAGIC: &[u8; 16] = b"\x89AQRPEMERGEv2\x00\x00\x00";

/// The size of an entry of the table, in bytes.
pub const ENTRY_LEN: usize = 24;

/// What `aqr-ld merge` fills: the dispatch table's place in the image (its magic, then RVA and
/// length). A stub defines one static of it, `AQR_PEMERGE_ANCHOR` (unmangled, `#[used]`), empty
/// until something is merged in.
#[repr(C)]
pub struct Anchor {
    pub magic: [u8; 16],
    pub dispatch_rva: u32,
    pub dispatch_len: u32,
}

impl Anchor {
    pub const EMPTY: Anchor = Anchor { magic: *ANCHOR_MAGIC, dispatch_rva: 0, dispatch_len: 0 };

    /// The RVA and length of the table, read as the merge may have written them after the
    /// compiler saw the zeros (volatile); `None` while nothing is merged in.
    pub fn table(&self) -> Option<(u32, u32)> {
        // SAFETY: references to the fields' own memory
        let (rva, len) =
            unsafe { (std::ptr::read_volatile(&self.dispatch_rva), std::ptr::read_volatile(&self.dispatch_len)) };
        (rva != 0 && len >= 4).then_some((rva, len))
    }
}

/// A module merged, as the table records it.
pub struct Entry<'a> {
    pub apply_rva: u32,
    pub info_rva: u32,
    /// Where its image is (RVA and length), and the RVA where its RVA 0 lands: for the program to
    /// register it (`register_image`), its panics' type information being relative to its RVA 0.
    pub image_rva: u32,
    pub image_len: u32,
    pub mod_base: u32,
    pub name: &'a [u8],
}

/// The table of `entries`.
pub fn write(entries: &[Entry]) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let names_at = 4 + entries.len() * ENTRY_LEN;
    let mut names = Vec::new();
    for e in entries {
        let name_at = (names_at + names.len()) as u32;
        for v in [e.apply_rva, e.info_rva, e.image_rva, e.image_len, e.mod_base, name_at] {
            table.extend_from_slice(&v.to_le_bytes());
        }
        names.extend_from_slice(&(e.name.len() as u32).to_le_bytes());
        names.extend_from_slice(e.name);
    }
    table.extend_from_slice(&names);
    table
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().unwrap()))
}

/// The entries of `table`, up to the first that does not lie within it.
pub fn entries(table: &[u8]) -> impl Iterator<Item = Entry<'_>> {
    let count = u32_at(table, 0).unwrap_or(0) as usize;
    (0..count).map_while(move |k| {
        let at = 4 + k * ENTRY_LEN;
        let word = |i: usize| u32_at(table, at + i * 4);
        let name_at = word(5)? as usize;
        let n = u32_at(table, name_at)? as usize;
        let name = table.get(name_at.checked_add(4)?..name_at.checked_add(4)?.checked_add(n)?)?;
        Some(Entry {
            apply_rva: word(0)?,
            info_rva: word(1)?,
            image_rva: word(2)?,
            image_len: word(3)?,
            mod_base: word(4)?,
            name,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read() {
        let names: [&[u8]; 3] = [b"add", b"", b"a-longer/name"];
        let entries: Vec<Entry> = names
            .iter()
            .enumerate()
            .map(|(i, &name)| {
                let i = i as u32;
                Entry { apply_rva: i, info_rva: 10 + i, image_rva: 20 + i, image_len: 30 + i, mod_base: 40 + i, name }
            })
            .collect();
        let table = write(&entries);
        let read: Vec<Entry> = super::entries(&table).collect();
        assert_eq!(read.len(), 3);
        for (a, b) in entries.iter().zip(&read) {
            assert_eq!(
                [a.apply_rva, a.info_rva, a.image_rva, a.image_len, a.mod_base],
                [b.apply_rva, b.info_rva, b.image_rva, b.image_len, b.mod_base]
            );
            assert_eq!(a.name, b.name);
        }
        // the names come right after the entries
        assert_eq!(u32_at(&table, 4 + 5 * 4), Some(4 + 3 * ENTRY_LEN as u32));
        // a truncated table ends the iteration where it is cut
        assert_eq!(super::entries(&table[..table.len() - 1]).count(), 2);
        assert_eq!(super::entries(&[]).count(), 0);
    }
}
