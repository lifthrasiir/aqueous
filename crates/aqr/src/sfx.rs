//! Self-extracting archives (`--format sfx`, Windows x86-64): the stub (aqr-sfx-shim) with the
//! blobs merged into it that the archive's modules need (`aqr_ld::pemerge`), and the archive
//! appended, which the stub finds by its header magic.
//!
//! The stub and blobs are those `crates/aqr-sfx/pack.sh` gathered when this program was built
//! (build.rs: `sfx.pack`). The archive names a module with a native blob there by a shorthand
//! module block, its hash alone ([`wasm_of`]), and the stub carries the blob (named by the hash
//! and the module's task slots, [`aqr_sfx::native_name`]) and the shared libraries that blob links
//! against (crates/aqr-dll/layout.py: the runtime among them; the imports between them bound by
//! the merge); any other module is written in full, and the interpreter blob is merged to run it.

use std::collections::HashMap;
use std::sync::OnceLock;

use aqr_formula::Module;
use aqr_formula::aqr::{self, Wasm};

/// What `crates/aqr-sfx/pack.sh` gathered, as build.rs packed it (empty where it was not there).
static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sfx.pack"));

/// The digest of the pack's files (`pack.id`; empty where there is none), included from where
/// pack.sh wrote it so that the build's dep-info has it: `cargo aqr` keeps a binary by the files
/// of its dep-info, and the packed file in `OUT_DIR` changes only once the build script runs.
pub const PACK_ID: &str = include_str!(env!("AQR_SFX_ID"));

/// The stub and the blobs of [`PACKED`].
pub struct Pack {
    stub: Vec<u8>,
    interp: Vec<u8>,
    /// The native blob of each module by its hash, with the bytes of its task slots (the custom
    /// section `aqr_task_slots` of its Wasm module).
    natives: HashMap<[u8; 32], (Vec<u8>, Vec<u8>)>,
    /// The shared libraries the native blobs link against, by their names as libraries (what
    /// the blobs' imports name), each with the libraries it links against itself.
    libs: HashMap<String, (Vec<u8>, Vec<String>)>,
}

/// The pack, unpacked once; an error where this program carries none.
pub fn pack() -> Result<&'static Pack, String> {
    static PACK: OnceLock<Result<Pack, String>> = OnceLock::new();
    PACK.get_or_init(unpack).as_ref().map_err(String::clone)
}

fn unpack() -> Result<Pack, String> {
    if PACKED.is_empty() {
        return Err("this aqr carries no self-extracting stub (built without target/aqr-sfx/, which \
                    crates/aqr-sfx/pack.sh gathers on Windows x86-64)"
            .into());
    }
    let raw = zstd::decode_all(PACKED).map_err(|e| format!("the self-extracting pack: {e}"))?;
    let mut files = HashMap::new();
    let mut at = 0;
    let take = |at: &mut usize| -> Option<&[u8]> {
        let n = u32::from_le_bytes(raw.get(*at..*at + 4)?.try_into().ok()?) as usize;
        let d = raw.get(*at + 4..*at + 4 + n)?;
        *at += 4 + n;
        Some(d)
    };
    while at < raw.len() {
        let (Some(name), Some(data)) = (take(&mut at), take(&mut at)) else {
            return Err("the self-extracting pack is cut short".into());
        };
        files.insert(String::from_utf8_lossy(name).into_owned(), data.to_vec());
    }
    let mut file = |n: &str| files.remove(n).ok_or(format!("the self-extracting pack has no {n}"));
    let (stub, interp, list) = (file("aqr-sfx-shim.exe")?, file("aqr-interp.blob")?, file("modules.txt")?);
    let mut libs = HashMap::new();
    for l in String::from_utf8_lossy(&file("libs.txt")?).lines().filter(|l| !l.is_empty()) {
        let blob = file(&format!("libs/{l}"))?;
        let (name, _, below) = aqr_ld::pemerge::blob_facts(&blob).map_err(|e| format!("{l}: {e}"))?;
        libs.insert(name, (blob, below));
    }
    // each module's blob, for the module of the hash it was packed for if this program has it
    // still (else it was packed from other code: left out)
    let mut natives = HashMap::new();
    for line in String::from_utf8_lossy(&list).lines() {
        let Some((name, hash)) = line.split_once(' ') else { continue };
        let blob = file(&format!("modules/{name}.blob"))?;
        let Some(k) = aqr_modules::KNOWN.iter().find(|k| hex(&k.sha512_256) == hash) else { continue };
        let wasm = aqr::module_of_block_data(k.block).ok_or_else(|| format!("{name}: a module block not valid"))?;
        let slots = aqr::custom_section(&wasm, "aqr_task_slots").unwrap_or_default().to_vec();
        natives.insert(k.sha512_256, (blob, slots));
    }
    Ok(Pack { stub, interp, natives, libs })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// How module `m` is written into a self-extracting archive ([`aqr_compress::Options::modules`]):
/// by its hash alone where the pack has its native blob, else in full.
pub fn wasm_of(m: Module) -> Option<Wasm> {
    let k = aqr_decompress::built(m)?;
    match pack().ok().and_then(|p| p.natives.get(&k.sha512_256)) {
        Some((_, slots)) => Some(Wasm::Registered { hash: k.sha512_256, classes: slots.len() }),
        None => aqr_decompress::wasm_of(m),
    }
}

/// What a self-extracting program carries of `archive`'s modules.
pub struct Made {
    pub exe: Vec<u8>,
    /// The native blobs merged, the shared libraries they link against, and whether the
    /// interpreter blob is.
    pub natives: usize,
    pub libs: usize,
    pub interp: bool,
}

/// The self-extracting program of `archive` (an Aqueous archive made with [`wasm_of`]): the stub,
/// the native blobs of the modules it names by their hashes and, if it has any module in full,
/// the interpreter blob merged into it, and the archive after.
pub fn make(archive: &[u8]) -> Result<Made, String> {
    let pack = pack()?;
    let ins = aqr::inspect::inspect(archive, false, &|_| None, None).map_err(|e| format!("{e:?}"))?;
    let (mut hashes, mut full) = (Vec::new(), false);
    for b in ins.blocks.iter().filter(|b| b.segment.is_some()) {
        match b.ty {
            // a shorthand module block: its type, its size (1 byte, 32) and the hash
            0x60 => {
                let hash: [u8; 32] = archive[b.at + 2..b.at + 34].try_into().unwrap();
                if !hashes.contains(&hash) {
                    hashes.push(hash);
                }
            }
            0x61..=0x63 => full = true,
            _ => {}
        }
    }
    let mut blobs = Vec::new();
    // (the shared libraries they link against, and those that these do, each once)
    let mut needed: Vec<String> = Vec::new();
    for h in &hashes {
        let (blob, slots) = pack.natives.get(h).ok_or_else(|| format!("no native blob of module {}", hex(h)))?;
        needed.extend(aqr_ld::pemerge::blob_facts(blob)?.2);
        blobs.push((aqr_sfx::native_name(h, slots), blob.clone()));
    }
    let mut libs: Vec<String> = Vec::new();
    while let Some(l) = needed.pop() {
        if !libs.contains(&l) {
            let (_, below) = pack.libs.get(&l).ok_or_else(|| format!("no shared library {l}"))?;
            needed.extend(below.iter().cloned());
            libs.push(l);
        }
    }
    libs.sort();
    for l in &libs {
        blobs.push((format!("{}{l}", aqr_sfx::LIB), pack.libs[l].0.clone()));
    }
    if full {
        blobs.push((aqr_sfx::INTERP.to_owned(), pack.interp.clone()));
    }
    let mut exe = aqr_ld::pemerge::merge_bytes(pack.stub.clone(), &blobs, "aqr_dll_apply", "aqr_dll_info")?;
    exe.extend_from_slice(archive);
    Ok(Made { exe, natives: hashes.len(), libs: libs.len(), interp: full })
}
