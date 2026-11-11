//! The version (aqr-version): `env!("AQR_VERSION")` for `--version`, and on Windows `aqr.rc`,
//! its `VERSIONINFO`, embedded in `aqr.exe`.
//!
//! And on Windows x86-64, what `--format sfx` merges into a self-extracting archive: the files of
//! `target/aqr-sfx/` (or of the directory `AQR_SFX_PACK` names), which `crates/aqr-sfx/pack.sh`
//! gathers, packed into `$OUT_DIR/sfx.pack` (src/sfx.rs reads it): the interpreter blob in a zstd
//! frame of its own (its length before it), unpacked alone where only it is needed, then the
//! other files in one frame, the modules' blobs sharing much (each its own copy of the standard
//! library). Where they are not there, the
//! pack is empty and `--format sfx` says so.

use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=aqr.rc");
    let version = aqr_version::emit();
    aqr_version::write_rc(&version, "Aqueous archiver", "aqr.exe");
    // (nothing done for a target other than Windows)
    embed_resource::compile_for("aqr.rc", ["aqr"], embed_resource::NONE).manifest_optional().unwrap();
    sfx_pack();
}

fn sfx_pack() {
    println!("cargo:rerun-if-env-changed=AQR_SFX_PACK");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let out = out_dir.join("sfx.pack");
    let target = |k: &str| std::env::var(k).unwrap_or_default();
    let from = match std::env::var_os("AQR_SFX_PACK") {
        Some(d) => PathBuf::from(d),
        None => {
            let t = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
            t.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")).join("aqr-sfx")
        }
    };
    println!("cargo:rerun-if-changed={}", from.join("pack.id").display());
    // (the pack's id, `AQR_SFX_ID`, which src/sfx.rs includes so that it is in the build's
    // dep-info: what tells `cargo aqr` (aqr-bincache) one pack from another)
    let none = out_dir.join("sfx.id");
    let list = std::fs::read_to_string(from.join("modules.txt"));
    let (Ok(list), "windows", "x86_64") = (list, &*target("CARGO_CFG_TARGET_OS"), &*target("CARGO_CFG_TARGET_ARCH"))
    else {
        std::fs::write(&none, b"").unwrap();
        println!("cargo:rustc-env=AQR_SFX_ID={}", none.display());
        std::fs::write(&out, b"").unwrap();
        return;
    };
    let id = from.join("pack.id");
    assert!(id.exists(), "{}: no pack.id (crates/aqr-sfx/pack.sh writes it)", from.display());
    println!("cargo:rustc-env=AQR_SFX_ID={}", std::path::absolute(&id).unwrap().display());
    let libs = std::fs::read_to_string(from.join("libs.txt")).unwrap_or_default();
    let mut names = ["aqr-sfx-shim.exe", "modules.txt", "libs.txt"].map(String::from).to_vec();
    names.extend(list.lines().filter_map(|l| Some(format!("modules/{}.blob", l.split_whitespace().next()?))));
    names.extend(libs.lines().filter(|l| !l.is_empty()).map(|l| format!("libs/{l}")));
    // the files, each its name and its bytes (u32 lengths before them)
    let mut raw = Vec::new();
    for name in &names {
        let path = from.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for part in [name.as_bytes(), &data] {
            raw.extend_from_slice(&(part.len() as u32).to_le_bytes());
            raw.extend_from_slice(part);
        }
    }
    // the interpreter blob in a frame of its own, first (its length before it): what runs the
    // modules as WebAssembly, unpacked alone (in a worker of the sandbox, say)
    let interp_path = from.join("aqr-interp.blob");
    println!("cargo:rerun-if-changed={}", interp_path.display());
    let interp = std::fs::read(&interp_path).unwrap_or_else(|e| panic!("{}: {e}", interp_path.display()));
    let start = std::time::Instant::now();
    let interp_frame = frame(&interp);
    let rest = frame(&raw);
    let mut packed = (interp_frame.len() as u64).to_le_bytes().to_vec();
    packed.extend_from_slice(&interp_frame);
    packed.extend_from_slice(&rest);
    let secs = start.elapsed().as_secs_f64();
    let n = names.len() + 1;
    println!("sfx.pack: {n} files, {} -> {} bytes in {secs:.1} s", raw.len() + interp.len(), packed.len());
    std::fs::write(&out, packed).unwrap();
}

/// `data` in a zstd frame (level 19, long matching: the blobs share much, each its own std).
fn frame(data: &[u8]) -> Vec<u8> {
    let mut z = zstd::stream::Encoder::new(Vec::new(), 19).unwrap();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    z.multithread(threads).unwrap();
    z.long_distance_matching(true).unwrap();
    z.window_log(27).unwrap();
    z.include_contentsize(true).unwrap();
    z.set_pledged_src_size(Some(data.len() as u64)).unwrap();
    z.write_all(data).unwrap();
    z.finish().unwrap()
}
