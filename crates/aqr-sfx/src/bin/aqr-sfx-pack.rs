//! `aqr-sfx-pack DIR`: the pack that aqr carries (`DIR/sfx.pack`, which aqr's build.rs includes)
//! of the files that crates/aqr-sfx/pack.py gathered in DIR (`target/aqr-sfx`), which aqr alone
//! reads (`crates/aqr/src/sfx.rs`): the interpreter blob, then the modules' and the shared
//! libraries' blobs, each set a bundle of raw LZMA2 frames ([`aqr_sfx::bundle::pack_xz`]: their
//! code through the disfilter), then the stub (packed by UPX already) and the lists as they are.
//!
//! ```text
//! pack = "AQRSFXP1" le64:n bundle[n] le64:n bundle[n] file*
//! file = le32:n name[n] le32:n byte[n]
//! ```
//!
//! The first bundle has the blob `aqr-interp.blob` alone; the second each `modules/NAME.blob`
//! of modules.txt and `libs/LIB` of libs.txt, by those names. The files are aqr-sfx-shim.exe,
//! modules.txt and libs.txt.

use std::path::Path;
use std::process::ExitCode;

/// The magic of the pack (crates/aqr/build.rs and aqr-bincache check it).
const MAGIC: &[u8; 8] = b"AQRSFXP1";

fn main() -> ExitCode {
    let Some(dir) = std::env::args_os().nth(1) else {
        eprintln!("usage: aqr-sfx-pack DIR");
        return ExitCode::from(2);
    };
    match pack(Path::new(&dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr-sfx-pack: {e}");
            ExitCode::FAILURE
        }
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
    let interp = vec![("aqr-interp.blob".to_string(), read("aqr-interp.blob")?)];
    let raw: usize = blobs.iter().chain(&interp).map(|b| b.1.len()).sum();
    let start = std::time::Instant::now();
    // (the two at once: the interpreter's frames take the longest)
    let (i, b) = std::thread::scope(|s| {
        let i = s.spawn(|| aqr_sfx::bundle::pack_xz(&interp));
        let b = aqr_sfx::bundle::pack_xz(&blobs);
        (i.join().unwrap(), b)
    });
    let (i, b) = (i?, b?);
    let mut out = MAGIC.to_vec();
    for bundle in [&i, &b] {
        out.extend_from_slice(&(bundle.len() as u64).to_le_bytes());
        out.extend_from_slice(bundle);
    }
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
        "{}: {} blobs, {raw} bytes -> {} (the interpreter's {}), the stub and lists {files}; {:.1} s",
        to.display(),
        blobs.len() + 1,
        i.len() + b.len(),
        i.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
