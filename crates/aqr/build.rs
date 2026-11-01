//! The version (aqr-version): `env!("AQR_VERSION")` for `--version`, and on Windows `aqr.rc`,
//! its `VERSIONINFO`, embedded in `aqr.exe`.

fn main() {
    println!("cargo:rerun-if-changed=aqr.rc");
    let version = aqr_version::emit();
    aqr_version::write_rc(&version, "Aqueous archiver", "aqr.exe");
    // (nothing done for a target other than Windows)
    embed_resource::compile_for("aqr.rc", ["aqr"], embed_resource::NONE).manifest_optional().unwrap();
}
