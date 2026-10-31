//! Packs `aqr.exe` and `aqr-win.exe` of `target/dist` (or of the directory `AQR_WIN_INSTALL_FROM`
//! names) into the payload (`src/payload.rs`), checked by unpacking it; and embeds the manifest and
//! the icon. Where the two are not there, the payload is empty and the installer says so: building
//! them is up to whoever builds the installer (`cargo build --profile dist -p aqr -p aqr-win`).

#[path = "src/payload.rs"]
#[allow(dead_code)]
mod payload;

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_exe::arch::x86;
use aqr_exe::format::pe;
use aqr_exe::reloc;

const FILES: [&str; 2] = ["aqr.exe", "aqr-win.exe"];

/// zstd's level for every frame.
const LEVEL: i32 = 19;

fn main() {
    println!("cargo:rerun-if-changed=aqr-win-install.manifest");
    println!("cargo:rerun-if-changed=aqr-win-install.rc");
    println!("cargo:rerun-if-changed=../../droplet.ico");
    println!("cargo:rerun-if-changed=src/payload.rs");
    println!("cargo:rerun-if-env-changed=AQR_WIN_INSTALL_FROM");
    embed_resource::compile("aqr-win-install.rc", embed_resource::NONE).manifest_optional().unwrap();
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|o| o == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if windows && msvc {
        let manifest = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("aqr-win-install.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("payload.bin");
    let from = source();
    for name in FILES {
        println!("cargo:rerun-if-changed={}", from.join(name).display());
    }
    let files: Option<Vec<(&str, Vec<u8>)>> =
        FILES.iter().map(|&name| Some((name, std::fs::read(from.join(name)).ok()?))).collect();
    let Some(files) = files else {
        println!(
            "cargo:warning=no {} in {}: the installer is built without them (cargo build --profile dist -p aqr -p aqr-win)",
            FILES.join(" and "),
            from.display()
        );
        std::fs::write(&out, b"").unwrap();
        return;
    };
    let start = Instant::now();
    let packed = pack(&files);
    let back = payload::unpack(&packed).unwrap_or_else(|e| panic!("the payload does not unpack: {e}"));
    assert!(back.len() == files.len() && back.iter().zip(&files).all(|(b, f)| b.name == f.0 && b.data == f.1));
    let raw: usize = files.iter().map(|f| f.1.len()).sum();
    println!(
        "cargo:warning=packed {} from {}: {raw} -> {} bytes in {:.1} s",
        FILES.join(" and "),
        from.display(),
        packed.len(),
        start.elapsed().as_secs_f64()
    );
    std::fs::write(&out, packed).unwrap();
}

/// `AQR_WIN_INSTALL_FROM` (relative to the workspace's root), else `dist` in the target
/// directory (`OUT_DIR` being `TARGET/PROFILE/build/CRATE-HASH/out`, or
/// `TARGET/TRIPLE/PROFILE/...`).
fn source() -> PathBuf {
    if let Some(d) = std::env::var_os("AQR_WIN_INSTALL_FROM") {
        return Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..").join(d);
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).unwrap();
    let up = profile.parent().unwrap();
    let triple = std::env::var("TARGET").unwrap();
    let target = if up.file_name().is_some_and(|n| n == triple.as_str()) { up.parent().unwrap() } else { up };
    target.join("dist")
}

fn pack(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut p = payload::MAGIC.to_vec();
    p.push(files.len() as u8);
    for (name, data) in files {
        p.push(name.len() as u8);
        p.extend_from_slice(name.as_bytes());
        p.extend_from_slice(&(data.len() as u64).to_le_bytes());
        let regions = regions(data);
        p.push(regions.len() as u8);
        let mut rest = Vec::with_capacity(data.len());
        let mut at = 0;
        for r in &regions {
            rest.extend_from_slice(&data[at..r.off]);
            at = r.off + r.len;
        }
        rest.extend_from_slice(&data[at..]);
        // (the frames compressed at once, each on a thread of its own)
        std::thread::scope(|s| {
            let rest = s.spawn(|| frame(&rest));
            let frames: Vec<Vec<_>> =
                regions.iter().map(|r| r.streams.iter().map(|st| s.spawn(move || frame(st))).collect()).collect();
            for (r, f) in regions.iter().zip(frames) {
                p.extend_from_slice(&r.head);
                f.into_iter().for_each(|f| p.extend(f.join().unwrap()));
            }
            p.extend(rest.join().unwrap());
        });
    }
    p
}

fn frame(d: &[u8]) -> Vec<u8> {
    let mut z = zstd::bulk::Compressor::new(LEVEL).unwrap();
    // (a window as large as the data, and no larger than the decoder takes by default)
    let log = (usize::BITS - d.len().max(1).leading_zeros()).clamp(10, 27);
    z.set_parameter(zstd::zstd_safe::CParameter::WindowLog(log)).unwrap();
    z.set_parameter(zstd::zstd_safe::CParameter::ContentSizeFlag(false)).unwrap();
    z.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(false)).unwrap();
    let z = z.compress(d).unwrap();
    let mut f = Vec::with_capacity(8 + z.len());
    f.extend_from_slice(&(d.len() as u32).to_le_bytes());
    f.extend_from_slice(&(z.len() as u32).to_le_bytes());
    f.extend(z);
    f
}

struct Region {
    off: usize,
    len: usize,
    /// The kind, offset, length and parameters, as written.
    head: Vec<u8>,
    streams: Vec<Vec<u8>>,
}

/// The code sections (x86 or x86-64) and the base relocations of the PE image `d`, in order and
/// apart; none where it is not one.
fn regions(d: &[u8]) -> Vec<Region> {
    let mut regions = Vec::new();
    let head = |kind: u8, off: usize, len: usize| {
        let mut h = vec![kind];
        h.extend_from_slice(&(off as u64).to_le_bytes());
        h.extend_from_slice(&(len as u64).to_le_bytes());
        h
    };
    if let Ok(code) = pe::locate_code(&mut std::io::Cursor::new(d))
        && matches!(code.machine, 0x14c | 0x8664)
    {
        for (va, off, size) in code.sections {
            let off = off as usize;
            let Some(section) = d.get(off..off.saturating_add(size)) else { continue };
            let Some(st) = x86::encode(section, va, code.is64) else { continue };
            let mut h = head(payload::X86, off, size);
            h.extend_from_slice(&st.origin().to_le_bytes());
            h.extend([st.sentinel_size(), code.is64 as u8]);
            regions.push(Region { off, len: size, head: h, streams: st.streams().to_vec() });
        }
    }
    for t in pe::tables(d) {
        if t.kind != pe::TableKind::Reloc {
            continue;
        }
        let section = &d[t.off..t.off + t.len];
        // (8 first, as aqr-compress: blocks padded to 8 bytes are valid padded to 4)
        let Some((align, streams)) = [3, 2].into_iter().find_map(|a| Some((a, reloc::encode(section, a)?))) else {
            continue;
        };
        let mut h = head(payload::RELOC, t.off, t.len);
        h.push(align);
        regions.push(Region { off: t.off, len: t.len, head: h, streams: streams.to_vec() });
    }
    regions.sort_by_key(|r| r.off);
    // (the first of those that overlap)
    let mut end = 0;
    regions.retain(|r| {
        let keep = r.off >= end && r.len > 0;
        if keep {
            end = r.off + r.len;
        }
        keep
    });
    regions
}
