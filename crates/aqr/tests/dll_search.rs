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

/// The libraries of the import directory of PE image `b` (64-bit), lowercase.
fn imports(b: &[u8]) -> Vec<String> {
    let pe = u32_at(b, 0x3c);
    let opt = pe + 24;
    let sections = opt + u16_at(b, pe + 20);
    let off = |rva: usize| {
        (0..u16_at(b, pe + 6)).map(|k| sections + 40 * k).find_map(|s| {
            let (va, vsize, raw) = (u32_at(b, s + 12), u32_at(b, s + 8).max(u32_at(b, s + 16)), u32_at(b, s + 20));
            rva.checked_sub(va).filter(|&d| d < vsize).map(|d| raw + d)
        })
    };
    let mut d = off(u32_at(b, opt + 112 + 8)).unwrap();
    let mut out = Vec::new();
    while b[d..d + 20].iter().any(|&x| x != 0) {
        let n = off(u32_at(b, d + 12)).unwrap();
        let end = n + b[n..].iter().position(|&c| c == 0).unwrap();
        out.push(String::from_utf8_lossy(&b[n..end]).to_lowercase());
        d += 20;
    }
    out
}

/// What `aqr.exe` imports is what its own sandbox workers (aqr.exe, of a lowbox token) load as
/// they start: libraries a worker was seen to start with. One of the others (USERENV.dll,
/// which `std::env::home_dir` links) stops a worker before it runs (STATUS_DLL_NOT_FOUND).
#[test]
fn imports_a_worker_loads() {
    let b = std::fs::read(env!("CARGO_BIN_EXE_aqr")).unwrap();
    let known = ["kernel32.dll", "advapi32.dll", "ntdll.dll", "bcryptprimitives.dll", "ws2_32.dll"];
    for lib in imports(&b) {
        assert!(lib.starts_with("api-ms-win-") || known.contains(&&lib[..]), "{lib}");
    }
}
