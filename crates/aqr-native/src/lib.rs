//! How each module of aqr-modules runs natively, from the decoding code of the codec crates alone
//! (none of their `encode`): its native decoding of whole inputs (on threads, where it has
//! them), or its code as a part module (applied streamed, or whole, by `aqr_api`'s emulation).
//! aqr-decompress runs the modules it knows by it (pairing each of its modules with the entry of
//! its name); aqr-dll builds each module's native library with its entry ([`find`]).
//!
//! The entries are by the modules' names: a module's native code is that of the version its
//! codec crate makes (aqr-decompress's modules, and aqr-modules' binaries).

/// A module's native decoding of whole inputs: the parameters, the resources, the inputs, the
/// output's length and the threads it may use; the output.
pub type Decode = fn(&[u8], &[&[u8]], &[&[u8]], u64, usize) -> Result<Vec<u8>, String>;

/// Whether a module applied as a part module is to be streamed rather than applied whole, by the
/// lengths of its inputs and its output ([`Run::Streamed`]).
pub type Streamed = fn(&[u64], u64) -> bool;

/// How a module runs natively.
#[derive(Clone, Copy)]
pub enum Run {
    /// Its `decode`, on whole inputs.
    Whole(Decode),
    /// A part module: the entry point of its code (`aqr_api::entry!`'s), applied streamed, and
    /// its `decode` applied whole, where it has one (else the emulation's).
    Part(aqr_api::emu::Entry, Option<Decode>),
    /// A part module in Wasm that natively is applied whole, by its threaded `decode` (which
    /// spreads streams and parts over threads; the output is the same as the streamed one's).
    Threaded(Decode),
    /// [`Run::Threaded`], and where `streamed` says of the lengths of an application's inputs and
    /// output that holding them whole is not worth the threads (a stream too long to gather), the
    /// emulation of `entry`, its inputs pulled and its output pushed, none held.
    Streamed(aqr_api::emu::Entry, Decode, Streamed),
}

/// A module as it runs natively: its name, whether it takes resources (those that do not are
/// given none), and how it runs.
#[derive(Clone, Copy)]
pub struct Native {
    pub name: &'static str,
    pub resources: bool,
    pub run: Run,
}

/// The entry of the module named `name`, if there is one (at compile time too).
pub const fn find(name: &str) -> Option<Native> {
    let mut i = 0;
    while i < NATIVE.len() {
        if eq(NATIVE[i].name.as_bytes(), name.as_bytes()) {
            return Some(NATIVE[i]);
        }
        i += 1;
    }
    None
}

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// [`Native`] of a module taking resources.
const fn takes(name: &'static str, decode: Decode) -> Native {
    Native { name, resources: true, run: Run::Whole(decode) }
}

/// [`Native`] of a module taking none.
const fn bare(name: &'static str, decode: Decode) -> Native {
    Native { name, resources: false, run: Run::Whole(decode) }
}

/// [`Native`] of a part module, of the entry point `entry` (`aqr_api::entry!`'s) and `decode`.
const fn part(name: &'static str, resources: bool, entry: aqr_api::emu::Entry, decode: Decode) -> Native {
    Native { name, resources, run: Run::Part(entry, Some(decode)) }
}

/// [`Native`] of a part module run natively as it is in Wasm, through the emulation of `aqr_api`
/// (whole or streamed).
const fn by_emulation(name: &'static str, entry: aqr_api::emu::Entry) -> Native {
    Native { name, resources: false, run: Run::Part(entry, None) }
}

/// [`Native`] of a module that is a part module in Wasm but natively is applied whole, on threads.
const fn threaded(name: &'static str, decode: Decode) -> Native {
    Native { name, resources: false, run: Run::Threaded(decode) }
}

/// [`threaded`] of a module taking resources.
const fn threaded_takes(name: &'static str, decode: Decode) -> Native {
    Native { name, resources: true, run: Run::Threaded(decode) }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// [`Native`] of a branch converter of aqr-bcj, a part module of windows of a page.
macro_rules! bra {
    ($name:literal, $arch:ident) => {
        part(
            $name,
            false,
            aqr_api::emu::Entry {
                kind: aqr_api::emu::Kind::Part,
                windows: &[
                    aqr_api::Window::Input(0, aqr_bcj::module::BRA_WINDOW),
                    aqr_api::Window::Output(0, aqr_bcj::module::BRA_WINDOW),
                ],
                arity: 3,
                call: |a| aqr_bcj::module::bra_part(aqr_bcj::bra::Arch::$arch, a[0], a[1], a[2]),
            },
            |p, _, i, n, _| aqr_bcj::module::bra(aqr_bcj::bra::Arch::$arch, p, i, n).map_err(err),
        )
    };
}

/// [`Native`] of 7z's Swap2 or Swap4 (aqr-bcj), a part module of windows of a page.
macro_rules! swap {
    ($name:literal, $width:literal) => {
        part(
            $name,
            false,
            aqr_api::emu::Entry {
                kind: aqr_api::emu::Kind::Part,
                windows: &[
                    aqr_api::Window::Input(0, aqr_bcj::module::SWAP_WINDOW),
                    aqr_api::Window::Output(0, aqr_bcj::module::SWAP_WINDOW),
                ],
                arity: 3,
                call: |a| aqr_bcj::module::swap_part($width, a[0], a[1], a[2]),
            },
            |p, _, i, n, _| aqr_bcj::module::swap($width, p, i, n).map_err(err),
        )
    };
}

/// Every module of aqr-modules, as it runs natively.
pub const NATIVE: &[Native] = &[
    part(
        "exe-recons/x86",
        true,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(2, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(3, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(4, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(5, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(6, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(7, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(8, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(9, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(10, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(11, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(12, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::X86_WINDOW),
            ],
            arity: 16,
            call: |a| {
                aqr_exe::module::x86_part(
                    a[0],
                    a[1],
                    [a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14]],
                    a[15],
                )
            },
        },
        |p, r, i, n, _| aqr_exe::module::x86(p, r, i, n).map_err(err),
    ),
    takes("exe-recons/x86-remap", |p, r, i, n, _| aqr_exe::module::x86_remap(p, r, i, n).map_err(err)),
    part(
        "exe-recons/arm64",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(2, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(3, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(4, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(5, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(6, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(7, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(8, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(9, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(10, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::ARM64_WINDOW),
            ],
            arity: 13,
            call: |a| {
                aqr_exe::module::arm64_part(
                    a[0],
                    [a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]],
                    a[12],
                )
            },
        },
        |p, _, i, n, _| aqr_exe::module::arm64(p, i, n).map_err(err),
    ),
    bare("exe-recons/arm64-remap", |p, _, i, n, _| aqr_exe::module::arm64_remap(p, i, n).map_err(err)),
    bare("exe-recons/arm64-delta", |p, _, i, n, _| aqr_exe::module::arm64_delta(p, i, n).map_err(err)),
    part(
        "exe-recons/macho-codesign",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::MACHO_CODESIGN_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::MACHO_CODESIGN_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::MACHO_CODESIGN_WINDOW),
            ],
            arity: 4,
            call: |a| aqr_exe::module::macho_codesign_part(a[0], [a[1], a[2]], a[3]),
        },
        |p, _, i, n, _| aqr_exe::module::macho_codesign(p, i, n).map_err(err),
    ),
    bare("exe-recons/cil", |p, _, i, n, _| aqr_exe::module::cil(p, i, n).map_err(err)),
    bare("exe-recons/pe-reloc", |p, _, i, n, _| aqr_exe::module::reloc(p, i, n).map_err(err)),
    bare("exe-recons/macho-symtab", |p, _, i, n, _| aqr_exe::module::macho_symtab(p, i, n).map_err(err)),
    part(
        "flate-set",
        true,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_flate::module::SET_WINDOW),
                aqr_api::Window::Input(1, aqr_flate::module::SET_RECORDS_WINDOW),
                aqr_api::Window::Input(2, aqr_flate::module::SET_RECORDS_WINDOW),
                aqr_api::Window::Output(0, aqr_flate::module::SET_WINDOW),
            ],
            arity: 7,
            call: |a| aqr_flate::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5], a[6]),
        },
        |p, r, i, n, t| aqr_flate::module::decode(p, r, i, n, t).map_err(err),
    ),
    part(
        "flate-inflate",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_flate::module::INFLATE_WINDOW),
                aqr_api::Window::Output(0, aqr_flate::module::INFLATE_WINDOW),
            ],
            arity: 3,
            call: |a| aqr_flate::module::inflate_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, _| aqr_flate::module::inflate(p, i, n).map_err(err),
    ),
    part(
        "lz4-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_lz4::part::WINDOW),
                aqr_api::Window::Output(0, aqr_lz4::part::WINDOW),
            ],
            arity: 3,
            call: |a| aqr_lz4::part::decode_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, t| aqr_lz4::module::decode(p, i, n, t).map_err(err),
    ),
    threaded("lzma-recons", |p, _, i, n, t| aqr_lzma::module::decode(p, i, n, t).map_err(err)),
    threaded("lzma2-recons", |p, _, i, n, t| aqr_lzma::module::decode2(p, i, n, t).map_err(err)),
    bra!("bcj-x86", X86),
    bra!("bcj-powerpc", PowerPc),
    bra!("bcj-ia64", Ia64),
    bra!("bcj-arm", Arm),
    bra!("bcj-armthumb", ArmThumb),
    bra!("bcj-sparc", Sparc),
    bra!("bcj-arm64", Arm64),
    bra!("bcj-riscv", RiscV),
    part(
        "bcj-delta",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bcj::module::DELTA_WINDOW),
                aqr_api::Window::Output(0, aqr_bcj::module::DELTA_WINDOW),
            ],
            arity: 3,
            call: |a| aqr_bcj::module::delta_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, _| aqr_bcj::module::delta(p, i, n).map_err(err),
    ),
    part(
        "bcj2",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Input(1, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(0, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(1, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(2, aqr_bcj::module::BCJ2_WINDOW),
            ],
            arity: 6,
            call: |a| aqr_bcj::module::bcj2_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, _| aqr_bcj::module::bcj2(p, i, n).map_err(err),
    ),
    swap!("bcj-swap2", 2),
    swap!("bcj-swap4", 4),
    bare("lzo-recons", |p, _, i, n, t| aqr_lzo::module::decode(p, i, n, t).map_err(err)),
    bare("azo-recons", |p, _, i, n, t| aqr_azo::module::decode(p, i, n, t).map_err(err)),
    bare("lha-recons", |p, _, i, n, t| aqr_lha::module::decode(p, i, n, t).map_err(err)),
    bare("rar5-recons", |p, _, i, n, t| aqr_rar::module::decode5(p, i, n, t).map_err(err)),
    bare("rar3-recons", |p, _, i, n, t| aqr_rar::module::decode3(p, i, n, t).map_err(err)),
    bare("bvx-recons", |p, _, i, n, t| aqr_bvx::module::decode(p, i, n, t).map_err(err)),
    threaded("bz2-recons", |p, _, i, n, t| aqr_bz2::module::decode(p, i, n, t).map_err(err)),
    threaded("bz2-alz-recons", |p, _, i, n, t| aqr_bz2::module::decode_alz(p, i, n, t).map_err(err)),
    Native {
        name: "lzx-recons",
        resources: false,
        run: Run::Streamed(
            aqr_api::emu::Entry {
                kind: aqr_api::emu::Kind::Part,
                windows: &[
                    aqr_api::Window::Input(0, aqr_lzx::module::WINDOW),
                    aqr_api::Window::Input(1, aqr_lzx::module::WINDOW),
                    aqr_api::Window::Input(2, aqr_lzx::module::WINDOW),
                    aqr_api::Window::Input(3, 1),
                    aqr_api::Window::Output(0, aqr_lzx::module::WINDOW),
                ],
                arity: 6,
                call: |a| aqr_lzx::module::decode_part(a[0], [a[1], a[2], a[3], a[4]], a[5]),
            },
            |p, _, i, n, t| aqr_lzx::module::decode(p, i, n, t).map_err(err),
            |lens, _| aqr_lzx::module::streamed(lens),
        ),
    },
    bare("xpress-recons", |p, _, i, n, t| aqr_xpress::module::decode(p, i, n, t).map_err(err)),
    bare("lznt1-recons", |p, _, i, n, t| aqr_lznt1::module::decode(p, i, n, t).map_err(err)),
    part(
        "mszip-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(1, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(2, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(3, aqr_mszip::module::WINDOW),
                aqr_api::Window::Output(0, aqr_mszip::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_mszip::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, _| aqr_mszip::module::decode(p, i, n).map_err(err),
    ),
    part(
        "div3-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_div3::module::WINDOW),
                aqr_api::Window::Input(1, aqr_div3::module::WINDOW),
                aqr_api::Window::Input(2, aqr_div3::module::WINDOW),
                aqr_api::Window::Input(3, aqr_div3::module::WINDOW),
                aqr_api::Window::Output(0, aqr_div3::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_div3::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, t| aqr_div3::native::decode(p, i, n, t).map_err(err),
    ),
    part(
        "ac3-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_ac3::module::WINDOW),
                aqr_api::Window::Output(0, aqr_ac3::module::WINDOW),
            ],
            arity: 3,
            call: |a| aqr_ac3::module::decode_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, t| aqr_ac3::native::decode(p, i, n, t).map_err(err),
    ),
    part(
        "snappy-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_snappy::module::WINDOW),
                aqr_api::Window::Output(0, aqr_snappy::module::WINDOW),
            ],
            arity: 3,
            call: |a| aqr_snappy::module::decode_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, t| aqr_snappy::module::decode(p, i, n, t).map_err(err),
    ),
    bare("leveldb-recons", |p, _, i, n, t| aqr_snappy::module::decode_table(p, i, n, t).map_err(err)),
    part(
        "zip-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(1, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(2, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(3, aqr_zip::module::WINDOW),
                aqr_api::Window::Output(0, aqr_zip::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_zip::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, t| aqr_zip::module::decode(p, i, n, t).map_err(err),
    ),
    part(
        "bundle-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bundle::module::WINDOW),
                aqr_api::Window::Input(1, aqr_bundle::module::WINDOW),
                aqr_api::Window::Output(0, aqr_bundle::module::WINDOW),
            ],
            arity: 4,
            call: |a| aqr_bundle::module::decode_part(a[0], a[1], a[2], a[3]),
        },
        |p, _, i, n, t| aqr_bundle::module::decode(p, i, n, t).map_err(err),
    ),
    bare("asar-recons", |p, _, i, n, t| aqr_asar::module::decode(p, i, n, t).map_err(err)),
    part(
        "git-pack-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_git::module::WINDOW),
                aqr_api::Window::Input(1, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(2, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(3, aqr_git::module::WINDOW),
                aqr_api::Window::Output(0, aqr_git::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_git::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, t| aqr_git::module::decode(p, i, n, t).map_err(err),
    ),
    part(
        "git-pack-index-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_git::module::WINDOW),
                aqr_api::Window::Input(1, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(2, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(3, aqr_git::module::WINDOW),
                aqr_api::Window::Input(4, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Output(0, aqr_git::module::WINDOW),
            ],
            arity: 7,
            call: |a| aqr_git::module::decode_index_part(a[0], a[1], a[2], a[3], a[4], a[5], a[6]),
        },
        |p, _, i, n, t| aqr_git::module::decode_index(p, i, n, t).map_err(err),
    ),
    threaded("png-recons", |p, _, i, n, t| aqr_png::module::decode(p, i, n, t).map_err(err)),
    bare("png-against", |p, _, i, n, t| aqr_png::module::decode_against(p, i, n, t).map_err(err)),
    bare("png-set", |p, _, i, n, t| aqr_png::module::decode_set(p, i, n, t).map_err(err)),
    bare("gif-recons", |p, _, i, n, t| aqr_gif::module::decode(p, i, n, t).map_err(err)),
    by_emulation("prores-recons", aqr_prores::module::ENTRY),
    bare("jpeg-recons", |p, _, i, n, t| aqr_jpeg::module::decode(p, i, n, t).map_err(err)),
    threaded("jpeg-lossless-recons", |p, _, i, n, t| aqr_jpeg::lmodule::decode(p, i, n, t).map_err(err)),
    threaded_takes("mp3-recons", |p, r, i, n, t| aqr_mp3::module::decode(p, r, i, n, t).map_err(err)),
    threaded_takes("flac-recons", |p, r, i, n, t| aqr_flac::module::decode(p, r, i, n, t).map_err(err)),
    bare("psd-recons", |p, _, i, n, t| aqr_psd::module::decode(p, i, n, t).map_err(err)),
    bare("clip-recons", |p, _, i, n, t| aqr_clip::module::decode(p, i, n, t).map_err(err)),
    bare("bcn-recons", |p, _, i, n, t| aqr_tex::module::decode(p, i, n, t).map_err(err)),
    bare("pdf-recons", |p, _, i, n, t| aqr_pdf::module::decode(p, i, n, t).map_err(err)),
    bare("sqlite-recons", |p, _, i, n, t| aqr_sqlite::module::decode(p, i, n, t).map_err(err)),
    part(
        "fasta-recons",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(1, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(2, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(3, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(4, aqr_fasta::module::WINDOW),
                aqr_api::Window::Output(0, aqr_fasta::module::WINDOW),
            ],
            arity: 7,
            call: |a| aqr_fasta::module::decode_part(a[0], [a[1], a[2], a[3], a[4], a[5]], a[6]),
        },
        |p, _, i, n, t| aqr_fasta::module::decode(p, i, n, t).map_err(err),
    ),
    bare("dex-recons", |p, _, i, n, _| aqr_dex::module::decode(p, i, n).map_err(err)),
    bare("dwarf-apple", |p, _, i, n, _| aqr_dwarf::module::apple(p, i, n).map_err(err)),
    bare("dwarf-info", |p, _, i, n, _| aqr_dwarf::module::info(p, i, n).map_err(err)),
    bare("pdb-msf", |p, _, i, n, _| aqr_pdb::module::msf(p, i, n).map_err(err)),
    bare("pdb-symbols", |p, _, i, n, _| aqr_pdb::module::symbols(p, i, n).map_err(err)),
    part(
        "pdb-types",
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(1, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(2, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(3, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(4, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(5, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(6, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(7, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(8, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(9, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(10, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(11, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Input(12, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(0, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(1, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(2, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(3, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(4, aqr_pdb::module::TYPES_WINDOW),
                aqr_api::Window::Output(5, aqr_pdb::module::TYPES_WINDOW),
            ],
            arity: 20,
            call: |a| {
                aqr_pdb::module::types_part(
                    a[0],
                    [a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13]],
                    [a[14], a[15], a[16], a[17], a[18], a[19]],
                )
            },
        },
        |p, _, i, n, _| aqr_pdb::module::types(p, i, n).map_err(err),
    ),
    bare("arsc-recons", |p, _, i, n, _| aqr_arsc::module::decode(p, i, n).map_err(err)),
    bare("wasm-recons", |p, _, i, n, _| aqr_wasm::module::decode(p, i, n).map_err(err)),
    bare("gnu-gettext-recons", |p, _, i, n, _| aqr_gnu_gettext::module::decode(p, i, n).map_err(err)),
    bare("fits-recons-image", |p, _, i, n, _| aqr_fits::module::image(p, i, n).map_err(err)),
    bare("fits-recons-rice", |p, _, i, n, _| aqr_fits::module::rice(p, i, n).map_err(err)),
    bare("fits-recons-table", |p, _, i, n, _| aqr_fits::module::table(p, i, n).map_err(err)),
    bare("obj-recons", |p, _, i, n, t| aqr_obj::module::decode(p, i, n, t).map_err(err)),
    bare("vorbis-recons", |p, _, i, n, _| aqr_vorbis::module::decode(p, i, n).map_err(err)),
    bare("ogg-recons", |p, _, i, n, _| aqr_ogg::module::decode(p, i, n).map_err(err)),
    bare("ima-recons", |p, _, i, n, _| aqr_ima::module::decode(p, i, n).map_err(err)),
    bare("hdf5-recons-float", |p, _, i, n, _| aqr_hdf5::module::float(p, i, n).map_err(err)),
    bare("xsv-recons", |p, _, i, n, t| aqr_xsv::module::decode(p, i, n, t).map_err(err)),
    bare("jsonl-recons", |p, _, i, n, t| aqr_jsonl::module::decode(p, i, n, t).map_err(err)),
    takes("wrt-recons", |p, r, i, n, t| aqr_wrt::module::decode(p, r, i, n, t).map_err(err)),
    takes("wrt-recons-hangul", |p, r, i, n, t| aqr_wrt::module::decode_hangul(p, r, i, n, t).map_err(err)),
    threaded_takes("pcm-recons", |p, r, i, n, t| aqr_raw::module::pcm::decode(p, r, i, n, t).map_err(err)),
    bare("raster-recons", |p, _, i, n, t| aqr_raw::module::raster::decode(p, i, n, t).map_err(err)),
    bare("raster-recons-16", |p, _, i, n, _| aqr_raw::module::raster16::decode(p, i, n).map_err(err)),
    bare("recs-fields", |p, _, i, n, _| aqr_infer_recs::module::decode_fields(p, i, n).map_err(err)),
    bare("recs-entries", |p, _, i, n, _| aqr_infer_recs::module::decode_entries(p, i, n).map_err(err)),
    bare("recs-floats", |p, _, i, n, _| aqr_infer_recs::module::decode_floats(p, i, n).map_err(err)),
];
