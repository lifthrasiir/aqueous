//! `aqr-sfx-pack DIR`: the pack that aqr carries (`DIR/sfx.pack`) of the files that
//! crates/aqr-sfx/pack.py gathered in DIR (`target/aqr-sfx`), which aqr alone reads
//! (`crates/aqr/src/sfx.rs`): the interpreter blobs, then the modules' and the shared libraries'
//! blobs, each a bundle of raw LZMA2 frames ([`aqr_sfx::bundle::pack_xz`]: their code through the
//! disfilter), then the stub (packed by UPX already) and the lists as they are.
//!
//! ```text
//! pack = "AQRSFXP3" le32:k interp[k] le64:n bundle[n] file*
//! interp = le32:n engine[n] le64:n bundle[n]
//! file = le32:n name[n] le32:n byte[n]
//! ```
//!
//! Each interpreter blob is a bundle of its own, of its engine's name (`wah`: `aqr-interp.blob`,
//! always; `wasmtime`: `aqr-interp-wasmtime.blob`, where there is one), so that aqr unpacks only
//! the one it places. The last bundle has each `modules/NAME.blob` of modules.txt and `libs/LIB`
//! of libs.txt, by those names. The files are aqr-sfx-shim.exe, modules.txt and libs.txt.
//!
//! `aqr-sfx-pack --embed DIR EXE` writes DIR's `sfx.pack` and `pack.id` into the executable EXE
//! (aqr, linked: [`aqr_sfx::resource`]), checking that the pack is of this form first.

use std::path::Path;
use std::process::ExitCode;

/// The magic of the pack (`--embed` and aqr-bincache check it, crates/aqr/src/sfx.rs reads it).
const MAGIC: &[u8; 8] = b"AQRSFXP3";

/// The interpreter blobs: each engine and its file (the first always there).
const INTERPS: [(&str, &str); 2] = [("wah", "aqr-interp.blob"), ("wasmtime", "aqr-interp-wasmtime.blob")];

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match &args[..] {
        [dir] => pack(Path::new(dir)),
        [e, dir, exe] if e == "--embed" => embed(Path::new(dir), Path::new(exe)),
        _ => {
            eprintln!("usage: aqr-sfx-pack DIR | aqr-sfx-pack --embed DIR EXE");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-sfx-pack: {e}");
            ExitCode::FAILURE
        }
    }
}

/// DIR's pack and its id written into EXE.
fn embed(dir: &Path, exe: &Path) -> Result<(), String> {
    let read = |name: &str| std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()));
    let (pack, id) = (read("sfx.pack")?, read("pack.id")?);
    if !pack.starts_with(MAGIC) {
        return Err(format!("{}: not a pack of this form (run crates/aqr-sfx/pack.py again)", dir.display()));
    }
    #[cfg(windows)]
    {
        aqr_sfx::resource::embed(exe, &pack, String::from_utf8_lossy(&id).trim())
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, id);
        Err("--embed: Windows only (the resources of a PE executable)".into())
    }
}

fn pack(dir: &Path) -> Result<(), String> {
    let read = |name: &str| std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()));
    let names = |list: &str| -> Result<Vec<String>, String> {
        Ok(String::from_utf8_lossy(&read(list)?)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(String::from)
            .collect())
    };
    let mut blobs = Vec::new();
    for m in names("modules.txt")? {
        let name = format!("modules/{m}.blob");
        blobs.push((name.clone(), read(&name)?));
    }
    for l in names("libs.txt")? {
        let name = format!("libs/{l}");
        blobs.push((name.clone(), read(&name)?));
    }
    let mut interps = Vec::new();
    for (i, (engine, file)) in INTERPS.into_iter().enumerate() {
        if i == 0 || dir.join(file).is_file() {
            interps.push((engine, vec![(file.to_string(), read(file)?)]));
        }
    }
    let raw: usize = blobs.iter().chain(interps.iter().flat_map(|i| &i.1)).map(|b| b.1.len()).sum();
    let start = std::time::Instant::now();
    // (all at once: the interpreters' frames take the longest)
    let (is, b) = std::thread::scope(|s| {
        let is: Vec<_> = interps.iter().map(|(_, i)| s.spawn(|| aqr_sfx::bundle::pack_xz(i))).collect();
        let b = aqr_sfx::bundle::pack_xz(&blobs);
        (is.into_iter().map(|i| i.join().unwrap()).collect::<Vec<_>>(), b)
    });
    let (is, b) = (is.into_iter().collect::<Result<Vec<_>, _>>()?, b?);
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(is.len() as u32).to_le_bytes());
    for ((engine, _), bundle) in interps.iter().zip(&is) {
        out.extend_from_slice(&(engine.len() as u32).to_le_bytes());
        out.extend_from_slice(engine.as_bytes());
        out.extend_from_slice(&(bundle.len() as u64).to_le_bytes());
        out.extend_from_slice(bundle);
    }
    out.extend_from_slice(&(b.len() as u64).to_le_bytes());
    out.extend_from_slice(&b);
    let mut files = 0;
    for name in ["aqr-sfx-shim.exe", "modules.txt", "libs.txt"] {
        let data = read(name)?;
        files += data.len();
        for part in [name.as_bytes(), &data] {
            out.extend_from_slice(&(part.len() as u32).to_le_bytes());
            out.extend_from_slice(part);
        }
    }
    let to = dir.join("sfx.pack");
    std::fs::write(&to, &out).map_err(|e| format!("{}: {e}", to.display()))?;
    eprintln!(
        "{}: {} blobs, {raw} bytes -> {} (the interpreters' {}), the stub and lists {files}; {:.1} s",
        to.display(),
        blobs.len() + is.len(),
        is.iter().map(Vec::len).sum::<usize>() + b.len(),
        interps.iter().zip(&is).map(|((e, _), i)| format!("{e} {}", i.len())).collect::<Vec<_>>().join(", "),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
