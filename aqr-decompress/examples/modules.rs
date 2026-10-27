//! Prints every module of aqr-decompress, a line each: its name, its version and the name of
//! its binary in aqr-modules (`/` as `-`), for aqr-modules/build.sh.

fn main() {
    for m in aqr_decompress::MODULES {
        println!("{} {} {}", m.name, m.version, m.name.replace('/', "-"));
    }
}
