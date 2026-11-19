//! Sections of CLR metadata written here, for exe-recons/cil (tests/wasm.rs and
//! tests/alike_misc.rs).

#![allow(dead_code)]

pub const ORIGIN: u32 = 0x2000;

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// A section of metadata (root at `meta`, after `head` bytes of method bodies): tables of the
/// `heap_sizes` flags (bit 0, 1, 2: 4-byte indices to #Strings, #GUID, #Blob), a MethodDef
/// table of `rows` rows (each RVA that of the body when `bodies`), the stream `#~` named
/// `tables_name` ("#~", or "#-" for an uncompressed one), and a fat method body with `eh_fat`
/// exception clauses where >0 (the fat form 0x41 of a section's flags).
pub fn section(heap_sizes: u8, rows: u32, bodies: bool, tables_name: &[u8], eh_fat: usize) -> (Vec<u8>, usize) {
    let idx = |bit: u8| if heap_sizes & bit != 0 { 4 } else { 2 };
    let (s, g, b) = (idx(1), idx(2), idx(4));
    let mut head = vec![0u8; 0x40];
    // a tiny body at 0x10 (ldstr, call, ret), a fat one at 0x20 with its exception table
    let code_a = [0x72, 1, 0, 0, 0x70, 0x28, 2, 0, 0, 0x0a, 0x2a];
    head[0x10] = (code_a.len() << 2 | 2) as u8;
    head[0x11..0x11 + code_a.len()].copy_from_slice(&code_a);
    let mut fat = vec![0u8; 12];
    let code_b = [0x16u8, 0x2a];
    fat[0..2].copy_from_slice(&(0x3003u16 | if eh_fat > 0 { 8 } else { 0 }).to_le_bytes());
    fat[2..4].copy_from_slice(&8u16.to_le_bytes());
    fat[4..8].copy_from_slice(&le32(code_b.len() as u32));
    fat[8..12].copy_from_slice(&le32(0x1100_0001));
    fat.extend(code_b);
    if eh_fat > 0 {
        while !fat.len().is_multiple_of(4) {
            fat.push(0);
        }
        let size = 4 + 24 * eh_fat;
        fat.extend([0x41, size as u8, (size >> 8) as u8, (size >> 16) as u8]);
        for i in 0..eh_fat {
            let v = [i as u32 % 2, 0, 1, 1, 1, 0x0100_0001];
            fat.extend(v.iter().flat_map(|x| x.to_le_bytes()));
        }
    }
    head.splice(0x20..0x20, fat.iter().copied());
    let head_len = head.len().next_multiple_of(4);
    head.resize(head_len, 0);

    let mut tables = vec![0, 0, 0, 0, 2, 0, heap_sizes, 1];
    tables.extend(((1u64 << 0) | (1 << 6)).to_le_bytes());
    tables.extend(0u64.to_le_bytes());
    tables.extend(le32(1).into_iter().chain(le32(rows)));
    // Module: generation, name, mvid, encid, encbaseid
    tables.extend([0, 0]);
    tables.extend(&le32(1)[..s]);
    tables.extend(&le32(1)[..g]);
    tables.extend(&le32(0)[..g]);
    tables.extend(&le32(0)[..g]);
    // MethodDef: RVA, impl flags, flags, name, signature, param list
    for i in 0..rows {
        let rva = if bodies { ORIGIN + if i % 2 == 0 { 0x10 } else { 0x20 } } else { 0 };
        tables.extend(le32(rva));
        tables.extend([0u8, 0, 0x86, 0]);
        tables.extend(&le32(1 + i % 3)[..s]);
        tables.extend(&le32(1)[..b]);
        tables.extend(1u16.to_le_bytes());
    }
    while tables.len() % 4 != 0 {
        tables.push(0);
    }
    let names: [&[u8]; 5] = [tables_name, b"#Strings", b"#US", b"#GUID", b"#Blob"];
    let heaps: [&[u8]; 5] = [&tables, b"\0Foo\0Bar\0Baz\0", b"\0\x05h\0i\0\0\0", &[0x5a; 16], b"\0\x03abc\0\0\0"];
    let mut m = Vec::new();
    m.extend(b"BSJB");
    m.extend([1, 0, 1, 0, 0, 0, 0, 0]);
    m.extend(le32(12));
    m.extend(b"v4.0.30319\0\0");
    m.extend([0, 0, 5, 0]);
    let header_len = m.len() + names.iter().map(|n| 8 + ((n.len() + 4) & !3)).sum::<usize>();
    let mut at = header_len;
    for (name, h) in names.iter().zip(heaps) {
        m.extend(le32(at as u32));
        m.extend(le32(h.len() as u32));
        m.extend(*name);
        m.extend(vec![0; ((name.len() + 4) & !3) - name.len()]);
        at += h.len();
    }
    for h in heaps {
        m.extend(h);
    }
    let meta = head.len();
    let mut d = head;
    d.extend(m);
    (d, meta)
}
