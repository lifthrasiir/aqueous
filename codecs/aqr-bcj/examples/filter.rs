//! A filter on a file, for comparing with other implementations:
//!
//! ```sh
//! cargo run --release -p aqr-bcj --example aqr-bcj-filter -- ARCH|delta ENC(0/1) PC|DIST IN OUT
//! ```

use aqr_bcj::bra::{self, Arch};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let enc = a[1] == "1";
    let n: u32 = a[2].parse().unwrap();
    let mut d = std::fs::read(&a[3]).unwrap();
    let arch = match &a[0][..] {
        "x86" => Arch::X86,
        "ppc" => Arch::PowerPc,
        "ia64" => Arch::Ia64,
        "arm" => Arch::Arm,
        "armt" => Arch::ArmThumb,
        "sparc" => Arch::Sparc,
        "arm64" => Arch::Arm64,
        "riscv" => Arch::RiscV,
        "delta" => {
            if enc {
                aqr_bcj::delta::encode(&mut d, n as usize)
            } else {
                aqr_bcj::delta::decode(&mut d, n as usize)
            }
            std::fs::write(&a[4], d).unwrap();
            return;
        }
        x => panic!("{x}"),
    };
    bra::convert(arch, &mut d, n, enc);
    std::fs::write(&a[4], d).unwrap();
}
