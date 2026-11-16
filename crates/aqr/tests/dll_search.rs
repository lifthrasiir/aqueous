//! The libraries `aqr.exe` imports are taken from System32 alone (`/DEPENDENTLOADFLAG` of
//! `LOAD_LIBRARY_SEARCH_SYSTEM32`, `.cargo/config.toml`), not from beside it, where one of the
//! same name may be planted (a folder of downloads).
#![cfg(windows)]

fn u16_at(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}

fn u32_at(b: &[u8], o: usize) -> usize {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize
}

/// The `DependentLoadFlags` of PE image `b` (64-bit), of its load configuration directory.
fn dependent_load_flags(b: &[u8]) -> Option<usize> {
    let pe = u32_at(b, 0x3c);
    let opt = pe + 24;
    let sections = opt + u16_at(b, pe + 20);
    // (data directory 10: the load configuration)
    let rva = u32_at(b, opt + 112 + 10 * 8);
    (0..u16_at(b, pe + 6)).map(|k| sections + 40 * k).find_map(|s| {
        let (va, size, raw) = (u32_at(b, s + 12), u32_at(b, s + 16), u32_at(b, s + 20));
        let at = raw + rva.checked_sub(va).filter(|&d| d < size)?;
        // (DependentLoadFlags: a 16-bit field at 78, where the directory is that long)
        (u32_at(b, at) >= 80).then(|| u16_at(b, at + 78))
    })
}

#[test]
fn imports_from_system32() {
    let b = std::fs::read(env!("CARGO_BIN_EXE_aqr")).unwrap();
    assert_eq!(dependent_load_flags(&b), Some(0x800));
}
