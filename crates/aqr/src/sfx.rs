//! Self-extracting archives (`--format sfx`, Windows x86-64): the stub (aqr-sfx-shim, packed by
//! UPX), the bundle of the blobs that the archive's modules need ([`aqr_sfx::bundle`]: their code
//! through aqr-exe's x86 disfilter, then zstd), which the stub places in memory as it runs, and
//! the archive.
//!
//! The stub and blobs are those `crates/aqr-sfx/pack.py` gathered (`sfx.pack`, aqr-sfx's
//! `aqr-sfx-pack`: the blobs in bundles of LZMA2, which this program alone reads; the bundles it
//! writes into self-extracting programs are of zstd), written into this program's executable as
//! resources once it was linked (`aqr-sfx-pack --embed`, [`aqr_sfx::resource`]: the release
//! workflow and `cargo aqr` do), so that the program is built without waiting for the pack. The
//! archive names a module with a native blob there by a shorthand module block, its hash alone
//! ([`wasm_of`]), and the bundle has the blob (named by the hash and
//! the module's task slots, [`aqr_sfx::native_name`]) and the shared libraries that blob links
//! against (crates/aqr-dll/layout.py: the runtime among them; the imports between them bound as
//! the stub places them); any other module is written in full, and the interpreter blob is in the
//! bundle to run it.
//!
//! An archive whose root has `.aqr/sfx/` makes a program of its settings ([`settings`],
//! docs/sfx-config-design.md): `sfx.toml` and the files it names there, read back of the
//! archive made (which keeps them as files: what makes the same program again), compiled into
//! the settings the stub reads ([`aqr_sfx::config`], after the bundle) and what they make of
//! the stub's image (its icon, `VERSIONINFO`, manifest and subsystem: [`aqr_sfx::pe`]).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::hex;
use aqr_archive::{EntryNode, Tree, Wasm};
use aqr_formula::Module;
use aqr_sfx::config::{Compiled, Kind};

/// The pack this program's executable carries, in place, and the digest of its files (`pack.id`);
/// both empty where it carries none.
fn embedded() -> (&'static [u8], &'static str) {
    #[cfg(windows)]
    {
        aqr_sfx::resource::embedded()
    }
    #[cfg(not(windows))]
    {
        (&[], "")
    }
}

/// The digest of the pack's files (`pack.id`; empty where there is none).
pub fn pack_id() -> &'static str {
    embedded().1
}

/// The stub and the blobs of the pack but the interpreter blob ([`interpreter`]).
pub struct Pack {
    stub: Vec<u8>,
    /// The native blob of each module by its hash, with the bytes of its task slots (the custom
    /// section `aqr_task_slots` of its Wasm module).
    natives: HashMap<[u8; 32], (Vec<u8>, Vec<u8>)>,
    /// The shared libraries the native blobs link against, by their names as libraries (what
    /// the blobs' imports name), each with the libraries it links against itself.
    libs: HashMap<String, (Vec<u8>, Vec<String>)>,
}

/// The interpreter blob of the pack: what runs the modules as WebAssembly where this program
/// links no Wasmtime (`aqr_decompress::set_interpreter`), unpacked alone (its bundle of its own,
/// the first).
pub fn interpreter() -> Result<Vec<u8>, String> {
    let [interp, _, _] = parts()?;
    let mut blobs = aqr_sfx::bundle::unpack(interp).map_err(|e| format!("the interpreter blob: {e}"))?;
    match (blobs.pop(), blobs.is_empty()) {
        (Some((name, blob)), true) if name == "aqr-interp.blob" => Ok(blob),
        _ => Err("the self-extracting pack has no interpreter blob".into()),
    }
}

/// The parts of the pack (aqr-sfx's `aqr-sfx-pack`): the bundle of the interpreter blob, the
/// bundle of the other blobs, and the other files.
fn parts() -> Result<[&'static [u8]; 3], String> {
    let packed = embedded().0;
    if packed.is_empty() {
        return Err("this aqr carries no blobs (the interpreter's and the self-extracting stub's: \
                    their pack, which crates/aqr-sfx/pack.py makes on Windows x86-64, not written \
                    into it by `aqr-sfx-pack --embed`)"
            .into());
    }
    let cut = || "the self-extracting pack is cut short".to_string();
    let mut d = packed.strip_prefix(b"AQRSFXP2").ok_or("the self-extracting pack is not valid")?;
    let mut bundle = || -> Result<&'static [u8], String> {
        let (n, r) = d.split_first_chunk::<8>().ok_or_else(cut)?;
        let (b, r) = r.split_at_checked(u64::from_le_bytes(*n) as usize).ok_or_else(cut)?;
        d = r;
        Ok(b)
    };
    let (interp, blobs) = (bundle()?, bundle()?);
    Ok([interp, blobs, d])
}

/// The pack, unpacked once; an error where this program carries none.
pub fn pack() -> Result<&'static Pack, String> {
    static PACK: OnceLock<Result<Pack, String>> = OnceLock::new();
    PACK.get_or_init(unpack).as_ref().map_err(String::clone)
}

fn unpack() -> Result<Pack, String> {
    let [_, blobs, raw] = parts()?;
    let mut files: HashMap<String, Vec<u8>> = aqr_sfx::bundle::unpack(blobs)?.into_iter().collect();
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
    let (stub, list) = (file("aqr-sfx-shim.exe")?, file("modules.txt")?);
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
        let wasm =
            aqr_archive::module_of_block_data(k.block).ok_or_else(|| format!("{name}: a module block not valid"))?;
        let slots = aqr_archive::custom_section(&wasm, "aqr_task_slots").unwrap_or_default().to_vec();
        natives.insert(k.sha512_256, (blob, slots));
    }
    Ok(Pack { stub, natives, libs })
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
    /// The native blobs in the bundle, the shared libraries they link against, whether the
    /// interpreter blob is, and the bundle's bytes.
    pub natives: usize,
    pub libs: usize,
    pub interp: bool,
    pub bundle: usize,
    /// Whether it has settings (of `.aqr/sfx/`).
    pub settings: bool,
}

/// The entry of the directory `.aqr/sfx/` of the root of `t`, if there is one.
pub fn settings_dir(t: &Tree) -> Option<usize> {
    aqr_sfx::config::dir_of(t)
}

/// The settings of `archive` (made by [`wasm_of`]) where its root has `.aqr/sfx/`: its files
/// there, of its first segment (where the compressor put them: [`aqr_compress::Options::lead`]),
/// decoded, compiled and checked against the archive's paths ([`aqr_sfx::config::compile`]).
pub fn settings(archive: &[u8], threads: usize) -> Result<Option<Compiled>, String> {
    let native = aqr_decompress::Native { threads };
    let a = aqr_archive::open(archive, &native).map_err(|e| format!("{e:?}"))?;
    let Some(t) = a.tree() else { return Ok(None) };
    if settings_dir(t).is_none() {
        return Ok(None);
    }
    let mut kinds: HashMap<String, Kind> = HashMap::new();
    for (i, e) in t.entries.iter().enumerate() {
        let path = aqr_sfx::config::path_of(t, i, None);
        match e.node {
            EntryNode::File(_) => drop(kinds.insert(path, Kind::File)),
            EntryNode::Dir => drop(kinds.insert(path, Kind::Dir)),
            _ => {}
        }
    }
    let files = aqr_sfx::config::read_files(archive, &native, threads, &|_| true)?;
    aqr_sfx::config::compile(&files, &|p| kinds.get(p).copied()).map(Some)
}

/// The self-extracting program of `archive` (an Aqueous archive made with [`wasm_of`]): the stub
/// (its image as `settings` tells), the bundle of the native blobs of the modules it names by
/// their hashes and, if it has any module in full, of the interpreter blob (none where it has no
/// module), the settings, and the archive.
pub fn make(archive: &[u8], settings: Option<&Compiled>) -> Result<Made, String> {
    let pack = pack()?;
    let ins = aqr_archive::inspect::inspect(archive, false, &|_| None, None).map_err(|e| format!("{e:?}"))?;
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
        blobs.push((aqr_sfx::INTERP.to_owned(), interpreter()?));
    }
    let bundle = if blobs.is_empty() { Vec::new() } else { aqr_sfx::bundle::pack(&blobs)? };
    let stub = match settings {
        Some(s) => aqr_sfx::pe::apply(&pack.stub, &s.pe)?,
        None => pack.stub.clone(),
    };
    let record = settings.map(|s| s.config.record()).unwrap_or_default();
    let mut exe = Vec::with_capacity(stub.len() + bundle.len() + record.len() + archive.len());
    exe.extend_from_slice(&stub);
    exe.extend_from_slice(&bundle);
    exe.extend_from_slice(&record);
    exe.extend_from_slice(archive);
    Ok(Made {
        exe,
        natives: hashes.len(),
        libs: libs.len(),
        interp: full,
        bundle: bundle.len(),
        settings: settings.is_some(),
    })
}
