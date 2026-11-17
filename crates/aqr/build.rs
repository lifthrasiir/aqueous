//! The version (aqr-version): `env!("AQR_VERSION")` for `--version`, and on Windows `aqr.rc`,
//! its `VERSIONINFO`, embedded in `aqr.exe`.
//!
//! (The SFX pack is not of the build: `aqr-sfx-pack --embed` writes it into the executable once
//! it is linked, src/sfx.rs.)

fn main() {
    println!("cargo:rerun-if-changed=aqr.rc");
    let version = aqr_version::emit();
    aqr_version::write_rc(&version, "Aqueous archiver", "aqr.exe");
    // (nothing done for a target other than Windows)
    embed_resource::compile_for("aqr.rc", ["aqr"], embed_resource::NONE).manifest_optional().unwrap();
}
