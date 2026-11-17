//! The version (aqr-version): `env!("AQR_VERSION")` for `--version`, and on Windows `aqr.rc`,
//! its `VERSIONINFO`, embedded in `aqr.exe`.
//!
//! And on Windows x86-64, the pack: the interpreter blob, and what `--format sfx` puts into a
//! self-extracting archive, as `crates/aqr-sfx/pack.py` gathered and packed them
//! (`target/aqr-sfx/sfx.pack`, or of the directory `AQR_SFX_PACK` names; aqr-sfx's
//! `aqr-sfx-pack`), copied to `$OUT_DIR/sfx.pack` (src/sfx.rs reads it). Where there is none, the
//! pack is empty and `--format sfx` says so.

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
    // (the target of Windows 7 and 8 too: the stub and the blobs are built for it)
    let os_arch = (&*target("CARGO_CFG_TARGET_OS"), &*target("CARGO_CFG_TARGET_ARCH"));
    let (Ok(_), ("windows", "x86_64")) = (list, os_arch) else {
        // (left as they are when they are already empty: build.rs runs on every build where pack.id
        // is missing, and the files would otherwise look changed during it to `cargo aqr`)
        write_new(&none, b"");
        println!("cargo:rustc-env=AQR_SFX_ID={}", none.display());
        write_new(&out, b"");
        return;
    };
    let id = from.join("pack.id");
    assert!(id.exists(), "{}: no pack.id (crates/aqr-sfx/pack.py writes it)", from.display());
    println!("cargo:rustc-env=AQR_SFX_ID={}", std::path::absolute(&id).unwrap().display());
    let path = from.join("sfx.pack");
    println!("cargo:rerun-if-changed={}", path.display());
    let packed =
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (crates/aqr-sfx/pack.py writes it)", path.display()));
    // (the magic of src/bin/aqr-sfx-pack.rs's pack, which src/sfx.rs reads; a pack of another
    // form is of an earlier pack.py)
    assert!(
        packed.starts_with(b"AQRSFXP2"),
        "{}: not a pack of this aqr (run crates/aqr-sfx/pack.py again)",
        path.display()
    );
    write_new(&out, &packed);
}

/// Writes `data` to `path` unless it already holds it, keeping its modification time.
fn write_new(path: &Path, data: &[u8]) {
    if std::fs::read(path).ok().as_deref() != Some(data) {
        std::fs::write(path, data).unwrap();
    }
}
