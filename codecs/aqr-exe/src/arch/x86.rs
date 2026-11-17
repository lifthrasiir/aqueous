#![cfg_attr(not(all(feature = "encode", feature = "decode")), allow(dead_code, unused_imports))]

// TODO:
// - Better error checking
// - Better code structure
// - Detect function boundary (skip CC or multi-byte nops)

use alloc::{vec, vec::Vec};
use core::array;
use core::cell::Cell;
use core::mem;
use core::ops;

use crate::util::{to_u32, to_u64};

#[cfg(all(feature = "encode", trace_encode))]
macro_rules! if_trace_encode {
    ($($t:tt)*) => {{$($t)*}};
}

#[cfg(all(feature = "encode", not(trace_encode)))]
macro_rules! if_trace_encode {
    ($($t:tt)*) => {};
}

#[cfg(all(feature = "decode", trace_decode))]
macro_rules! if_trace_decode {
    ($($t:tt)*) => {{$($t)*}};
}

#[cfg(all(feature = "decode", not(trace_decode)))]
macro_rules! if_trace_decode {
    ($($t:tt)*) => {};
}

const N_: u8 = 0b0000; // no immediate
const N1: u8 = 0b0001; // 8-bit immediate
const N2: u8 = 0b0010; // 16-bit immediate
const N4: u8 = 0b0011; // 32/16-bit immediate, depending on operand size prefix
const NZ: u8 = 0b0100; // 32/64-bit immediate, depending on REX.W

const J1: u8 = 0b0101; // 8-bit relative jump target
const J4: u8 = 0b0110; // 32-bit relative jump target
const JA: u8 = 0b0111; // 32-bit absolute jump target
const A_: u8 = 0b1000; // 16/32/64-bit absolute address that is not a jump target,
                       // exact bit size depends on the instruction and context

const R1: u8 = 0b1001; // modR/M + 8-bit immediate
const XX: u8 = 0b1010; // invalid, has to be escaped
const R4: u8 = 0b1011; // modR/M + 32/16-bit immediate, depending on operand size prefix
const R_: u8 = 0b1100; // modR/M + no immediate
const BP: u8 = 0b1101; // 1-byte prefix that will be marked
const M2: u8 = 0b1110; // opcode byte + modR/M + no immediate (map 2)
const M3: u8 = 0b1111; // opcode byte + modR/M + 8-bit immediate (map 3)

#[inline(always)]
const fn op3_followed(flags: u8) -> bool {
    flags >= M2
}

#[inline(always)]
const fn has_modrm(flags: u8) -> bool {
    flags >= R1
}

#[inline(always)]
const fn modrm_to_imm(flags: u8) -> u8 {
    flags & 3
}

const _: () = {
    assert!(N_ + 1 == N1 && N1 + 1 == N2 && N2 + 1 == N4 && N4 + 1 == NZ);
    assert!(modrm_to_imm(R_) == N_ && modrm_to_imm(R1) == N1 && modrm_to_imm(R4) == N4);
    assert!(M2 + 1 == M3);
};

// https://sandpile.org/
//
// https://github.com/torvalds/linux/blob/master/arch/x86/lib/x86-opcode-map.txt
// - Placeholders are an argument made of one uppercase letter and one lowercase letter.
// - Immediate or address is present if some placeholder starts with AIJLO.
//   - For -b/-w/-d/-z placeholders, its size is fixed to 8/16/32/32 bits.
//   - For -v placeholders, its size is normally 32 bits but becomes 16 bits with 66 prefixed.
//   - -p placeholder introduces an additional 16-bit segment selector before address.
//   - L- placeholder introduces an additional 8-bit register selector.
// - ModR/M byte is present if some placeholder starts with CDEGMNPQRSTUVW.

// 1-byte opcodes (legacy map 0)
const OPCODES0: [u8; 256] = [
    R_, R_, R_, R_, N1, N4, N_, N_, R_, R_, R_, R_, N1, N4, N_, XX, // 0
    R_, R_, R_, R_, N1, N4, N_, N_, R_, R_, R_, R_, N1, N4, N_, N_, // 1
    R_, R_, R_, R_, N1, N4, BP, N_, R_, R_, R_, R_, N1, N4, BP, N_, // 2
    R_, R_, R_, R_, N1, N4, BP, N_, R_, R_, R_, R_, N1, N4, BP, N_, // 3
    N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, // 4
    N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, // 5
    N_, N_, R_, R_, BP, BP, BP, BP, N4, R4, N1, R1, N_, N_, N_, N_, // 6
    J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, J1, // 7
    R1, R4, R1, R1, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 8
    N_, N_, N_, N_, N_, N_, N_, N_, N_, N_, JA, N_, N_, N_, N_, N_, // 9
    A_, A_, A_, A_, N_, N_, N_, N_, N1, N4, N_, N_, N_, N_, N_, N_, // A
    N1, N1, N1, N1, N1, N1, N1, N1, NZ, NZ, NZ, NZ, NZ, NZ, NZ, NZ, // B
    R1, R1, N2, N_, R_, R_, R1, R4, N2, N_, N2, N_, N_, N1, N_, N_, // C
    R_, R_, R_, R_, N1, N1, N_, N_, R_, R_, R_, R_, R_, R_, R_, R_, // D
    J1, J1, J1, J1, N1, N1, N1, N1, J4, J4, A_, J1, N_, N_, N_, N_, // E
    BP, N_, BP, BP, N_, N_, R1, R4, N_, N_, N_, N_, N_, N_, R_, R_, // F
];

// Some 1-byte opcodes are outright invalid in x86-64.
const OPCODES0_I64: [u32; 8] = [
    //FEDCBA9876543210 FEDCBA9876543210
    0b0100000011000000_1100000011000000, // 1x, 0x
    0b1000000010000000_1000000010000000, // 3x, 2x
    0b0000000000000000_1111111111111111, // 5x, 4x
    0b0000000000000000_0000000000000111, // 7x, 6x
    0b0000010000000000_0000000000000100, // 9x, 8x
    0b0000000000000000_0000000000000000, // Bx, Ax
    0b0000000001110000_0100000000110000, // Dx, Cx
    0b0000000000000000_0000010000000000, // Fx, Ex
];

const PRE_VEX3: u8 = 0xc4;
const PRE_VEX2: u8 = 0xc5;
const PRE_EVEX: u8 = 0x62;
const PRE_REX2: u8 = 0xd5;
const PRE_2BYTE: u8 = 0x0f;
const PRE_OSIZE: u8 = 0x66;
const PRE_ASIZE: u8 = 0x67;
const PRE_REPNE: u8 = 0xf2;
const PRE_REP: u8 = 0xf3;

const OP_CALLN: u16 = 0x0_e8; // CALL Jz

// Opcodes in the map 0 that need an additional 16-bit immediate.
const OP_CALLF: u16 = 0x0_9a; // CALL Ap (32-bit only)
const OP_JMPF: u16 = 0x0_ea; // JMP Ap (32-bit only)
const OP_ENTER: u16 = 0x0_c8; // ENTER Iw,Ib

// Opcodes in the map 0 that have immediates only with /0 or /1.
const OP_GRP3_1: u16 = 0x0_f6; // TEST Eb,Ib; NOT/NEG/[I]MUL/[I]DIV Eb
const OP_GRP3_2: u16 = 0x0_f7; // TEST Ev,Iv; NOT/NEG/[I]MUL/[I]DIV Ev

// 2-byte opcodes, starting with 0F (legacy map 1)
const OPCODES1: [u8; 256] = [
    R_, R_, N_, N_, XX, N_, N_, N_, N_, N_, XX, XX, XX, R_, N_, R1, // 0F 0
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, XX, R_, R_, // 0F 1
    R_, R_, R_, R_, XX, XX, XX, XX, R_, R_, R_, R_, R_, R_, R_, R_, // 0F 2
    N_, N_, N_, N_, N_, N_, XX, N_, M2, XX, M3, XX, XX, XX, XX, XX, // 0F 3
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F 4
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F 5
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F 6
    R1, R1, R1, R1, R_, R_, R_, N_, N_, R_, R_, R_, R_, R_, R_, R_, // 0F 7
    J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, J4, // 0F 8
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F 9
    N_, N_, N_, R_, R1, R_, R_, R_, N_, N_, N_, R_, R1, R_, R_, R_, // 0F A
    R_, R_, R_, R_, R_, R_, R_, R_, N_, R_, R1, R_, R_, R_, R_, R_, // 0F B
    R_, R_, R1, R_, R1, R1, R1, R_, N_, N_, N_, N_, N_, N_, N_, N_, // 0F C
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F D
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 0F E
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, XX, // 0F F
];

// Opcode in the map 1 that has a ModR/M byte only with some prefixes.
const OP_JMPE_POPCNT: u16 = 0x1_b8; // JMPE Jz (IA-64 only); POPCNT Gv,Ev (with F3 prefix)

// EVEX opcode map 4 (largely a subset of the opcode map 0)
const OPCODES4: [u8; 256] = [
    R_, R_, R_, R_, XX, XX, XX, XX, R_, R_, R_, R_, XX, XX, XX, XX, // 0
    R_, R_, R_, R_, XX, XX, XX, XX, R_, R_, R_, R_, XX, XX, XX, XX, // 1
    R_, R_, R_, R_, R1, XX, XX, XX, R_, R_, R_, R_, R1, XX, XX, XX, // 2
    R_, R_, R_, R_, XX, XX, XX, XX, R_, R_, R_, R_, XX, XX, XX, XX, // 3
    R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, R_, // 4
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 5
    R_, R_, XX, XX, XX, R_, R_, XX, XX, R4, XX, R1, XX, XX, XX, XX, // 6
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 7
    R1, R4, XX, R1, N_, N_, XX, XX, R_, XX, XX, XX, XX, XX, XX, R_, // 8
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 9
    XX, XX, XX, XX, XX, R_, XX, XX, XX, XX, XX, XX, XX, R_, XX, R_, // A
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // B
    R1, R1, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // C
    R_, R_, R_, R_, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // D
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // E
    R_, R_, R_, XX, R_, R_, R1, R4, R_, R_, XX, XX, XX, XX, R_, R_, // F
];

const OP_MAP4_GRP3_1: u16 = 0x4_f6;
const OP_MAP4_GRP3_2: u16 = 0x4_f7;

// EVEX opcode map 7
const OPCODES7: [u8; 256] = [
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 0
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 1
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 2
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 3
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 4
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 5
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 6
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 7
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 8
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // 9
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // A
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // B
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // C
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // D
    XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, XX, // E
    XX, XX, XX, XX, XX, XX, XX, XX, R4, XX, XX, XX, XX, XX, XX, XX, // F
];

// Opcode in the map 7 which immediate isn't affected by the operand size prefix.
const OP_URDMSR_UWRMSR: u16 = 0x7_f8; // URDMSR Rq,Id; UWRMSR Id,Rq

// Special bytes in the ST_OP stream. They should be available in any operating modes
// and decode without any more operand, but yet have to be extremely unlikely to occur.
//
// - Since we regularize any *VEX & REX prefixes, a single byte REX prefix is
//   also rewritten as a uniform marker followed by a normalized REX data byte.
//   REX bytes themselves (40-4F) are used as an opcode map prefix instead,
//   so the original 2-byte prefix (0F) is repurposed for the REX marker instead.
//
// - Jump tables are indicated by INT1 (F1), which is meant to be used for hardware debugging
//   and therefore extremely unlikely to occur in the regular x86 opcode stream.
//
// - Verbatim bytes are indicated by HLT (F4), which is privileged and can only occur rarely
//   due to its semantics, making it a good opcode to steal.
//
// The original disfilter used to use INTO (CE) instead of HLT, but it is now invalid in
// the long mode and has a chance to be repurposed in the future.
const REX_MARKER: u8 = 0x0f;
const JUMPTAB: u8 = 0xf1;
const ESC: u8 = 0xf4;

/// The classes of the opcodes of maps 0, 1, 4 and 7, and of map 0 in x86-64: the resource of
/// the module (§4 of FORMAT.md), so that opcodes can be added without another version of it.
/// The encoder parses by [`Opcodes::BUILTIN`]; the decoder by those it is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opcodes([[u8; 256]; 5]);

impl Opcodes {
    /// The bytes of the resource: the five tables one after another.
    pub const LEN: usize = 5 * 256;

    /// This implementation's tables.
    pub const BUILTIN: Opcodes = {
        let mut tabs = [OPCODES0, OPCODES1, OPCODES4, OPCODES7, OPCODES0];
        let mut i = 0;
        while i < 256 {
            if OPCODES0_I64[i >> 5] >> (i & 31) & 1 == 1 {
                tabs[4][i] = XX;
            }
            i += 1;
        }
        assert!(OPCODES0[ESC as usize] == N_);
        assert!(OPCODES0[JUMPTAB as usize] == N_);
        Opcodes::markers(tabs)
    };

    /// The tables of a resource, unless it is not [`Opcodes::LEN`] bytes, a byte is no class,
    /// or the class `BP` is anywhere but at the 1-byte prefixes in map 0.
    pub fn from_resource(r: &[u8]) -> Option<Opcodes> {
        let r: &[u8; Self::LEN] = r.try_into().ok()?;
        let tabs: [[u8; 256]; 5] = array::from_fn(|t| *r[t * 256..].first_chunk().unwrap());
        for (t, tab) in tabs.iter().enumerate() {
            for (op, &k) in tab.iter().enumerate() {
                let map0 = t == 0 || t == 4;
                if k >= 16 || k == BP && !(map0 && PREFIXES.contains(&(op as u8))) {
                    return None;
                }
            }
        }
        Some(Opcodes::markers(tabs))
    }

    pub fn to_resource(&self) -> Vec<u8> {
        self.0.as_flattened().to_vec()
    }

    /// `tabs` with the markers of map 0 invalid (`XX`), whatever they say.
    const fn markers(mut tabs: [[u8; 256]; 5]) -> Opcodes {
        tabs[0][ESC as usize] = XX;
        tabs[0][JUMPTAB as usize] = XX;
        tabs[4][ESC as usize] = XX;
        tabs[4][JUMPTAB as usize] = XX;
        Opcodes(tabs)
    }

    #[inline(always)]
    fn class(&self, op: u8, map: u8, is64: bool) -> u8 {
        let t = match map {
            0 if is64 => 4,
            0 => 0,
            1 => 1,
            4 => 2,
            7 => 3,
            2 | 5 | 6 => return R_,
            3 => return R1,
            _ => return XX,
        };
        self.0[t][op as usize]
    }
}

#[inline(always)]
const fn prefix_hash(b: u8) -> u32 {
    let b = b as u32;
    ((b << 2) ^ (b >> 2)) & 31
}

#[inline(always)]
const fn has_osize_prefix(prefixes: u32) -> bool {
    (prefixes >> prefix_hash(PRE_OSIZE)) & 1 != 0
}

#[inline(always)]
const fn has_asize_prefix(prefixes: u32) -> bool {
    (prefixes >> prefix_hash(PRE_ASIZE)) & 1 != 0
}

#[inline(always)]
const fn has_rep_prefix(prefixes: u32) -> bool {
    prefixes & ((1 << prefix_hash(PRE_REPNE)) | (1 << prefix_hash(PRE_REP))) != 0
}

// ModR/M uses the 16-bit addressing form (no SIB, disp16) in x86-32 with 67.
#[inline(always)]
const fn has_addr16(prefixes: u32, is64: bool) -> bool {
    !is64 && has_asize_prefix(prefixes)
}

// Log2 of the byte size of an A_ operand.
#[inline(always)]
const fn addr_lgn(op: u16, prefixes: u32, is64: bool) -> usize {
    if op == OP_JMPF {
        // EA: 32-bit only, 16-bit if 66 ("Ap" = w:z)
        if has_osize_prefix(prefixes) {
            1
        } else {
            2
        }
    } else {
        // Ax: 32-bit or 64-bit per operating mode, halved with 67 ("Ov")
        (is64 as usize + 2) - has_asize_prefix(prefixes) as usize
    }
}

// J4 and JA operands are 16-bit in x86-32 with 66 (and J4 also truncates the target to 16 bits).
// x86-64 ignores 66 for them, at least for Intel processors.
#[inline(always)]
const fn has_jump16(prefixes: u32, is64: bool) -> bool {
    !is64 && has_osize_prefix(prefixes)
}

// Size of an immediate that follows ModR/M, as needed for [rip+disp32].
#[inline(always)]
const fn modrm_imm_size(flags: u8, prefixes: u32) -> usize {
    match flags {
        N1 => 1,
        N4 if has_osize_prefix(prefixes) => 2,
        N4 => 4,
        _ => 0,
    }
}

/// The 1-byte prefixes, the only opcodes of class `BP`.
const PREFIXES: [u8; 11] = [0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, 0x66, 0x67, 0xf0, 0xf2, 0xf3];

const _: () = {
    let mut bitset = 0;
    let mut i = 0;
    while i < PREFIXES.len() {
        let b = PREFIXES[i];
        bitset |= 1u32 << prefix_hash(b);
        assert!(OPCODES0[b as usize] == BP);
        i += 1;
    }
    assert!(bitset.count_ones() == 11);
};

/// Bytes an instruction may read past its start, and so the most that a section's end is
/// extended with and cut again: enough for this encoding scheme (FORMAT.md allows 0 to this).
pub(crate) const SENTINEL: usize = 15;

macro_rules! define_streams {
    ($($i:ident $s:literal),* $(,)?) => (
        define_streams! { @0 $($i)* }
        /// Names of the streams, in order.
        pub const STREAM_NAMES: [&str; NUM_STREAMS] = [$(stringify!($i)),*];
        const STREAM_SIZES: [usize; NUM_STREAMS] = [$($s / 8),*];
    );

    (@$c:tt $i:ident $($t:tt)*) => (
        #[allow(dead_code)] const $i: usize = $c;
        define_streams! { @(1 + $c) $($t)* }
    );

    (@$c:tt) => (
        /// Number of the streams; all of them are always present, possibly empty.
        pub const NUM_STREAMS: usize = $c;
    );
}

// Fields of several sizes share a stream (size 0, not checked).
define_streams! {
    ST_OP 0,
    ST_SIB 8,
    ST_CALL_IDX 8,
    ST_DISP8_SP 8, ST_DISP8_BP 8, ST_DISP8 8, ST_DISP8_IDX 8,
    ST_DISP32 32,
    ST_JUMP8 8, ST_JUMP 0,
    ST_IMM32 32,
    ST_ADDR 0,
    ST_CALL 0,
    ST_JUMPN 8,
}

/// A jump's count (§5.6 of FORMAT.md): the steps from the one after the jump to its target's,
/// from `-MAX_COUNT` to `MAX_COUNT`; [`COUNT_ESC`] where the jump is written otherwise.
const MAX_COUNT: i64 = 127;
const COUNT_ESC: u8 = 0x80;
/// Bytes after a jump's field that the target of a forward count may be at most.
pub(crate) const COUNT_REACH: usize = 4096;
/// Steps that a decoder remembers the addresses of, for backward counts; and the jumps it may
/// have waiting for their targets (fewer than this: one a step, each within `MAX_COUNT` steps).
const COUNT_STEPS: usize = 128;
const COUNT_WAITING: usize = 256;

// The fields kept with others: in `OP` those that the opcodes around them tell best (REX bits
// above all) or that are rare, and the 32- and 64-bit forms of a field together (a section
// has one mode).
const ST_MODRM: usize = ST_OP;
const ST_EVEX: usize = ST_OP;
const ST_VEX: usize = ST_OP;
const ST_REX: usize = ST_OP;
const ST_IMM8: usize = ST_OP;
const ST_IMM16: usize = ST_OP;
const ST_IMM64: usize = ST_OP;
const ST_DISP16: usize = ST_OP;
const ST_ADDR16: usize = ST_OP;
const ST_JUMP16: usize = ST_OP;
const ST_AJUMP16: usize = ST_OP;
const ST_JUMPTAB_COUNT: usize = ST_OP;
const ST_PAD: usize = ST_OP;
const ST_JUMP32: usize = ST_JUMP;
const ST_JUMP64: usize = ST_JUMP;
const ST_AJUMP32: usize = ST_JUMP;
const ST_JUMPTAB64: usize = ST_JUMP;
const ST_ADDR32: usize = ST_ADDR;
const ST_ADDR64: usize = ST_ADDR;
const ST_CALL32: usize = ST_CALL;
const ST_CALL64: usize = ST_CALL;

/// The stream of an absolute address of `1 << lgn` bytes (`lgn` from 1 to 3).
const fn addr_stream(lgn: usize) -> usize {
    if lgn == 1 {
        ST_ADDR16
    } else {
        ST_ADDR
    }
}

/// The stream of an immediate of `1 << lgn` bytes (`lgn` from 0 to 3).
const fn imm_stream(lgn: usize) -> usize {
    match lgn {
        0 => ST_IMM8,
        1 => ST_IMM16,
        2 => ST_IMM32,
        _ => ST_IMM64,
    }
}

/// The stream of an 8-bit displacement, by the ModR/M `rm` field, the SIB byte (0 if none) and
/// the instruction's REX byte (0 if none; bit 0 is REX.B and bit 1 REX.X in every prefix):
/// `[rsp+...]`, `[rbp+...]`, and of other bases those with an index apart.
const fn disp8_stream(rm: u8, sib: u8, rex: u8, addr16: bool) -> usize {
    let (b, x) = (rex & 1 != 0, rex & 2 != 0);
    match rm {
        _ if addr16 => ST_DISP8,
        4 if sib & 7 == 4 && !b => ST_DISP8_SP,
        4 if (sib >> 3) & 7 != 4 || x => ST_DISP8_IDX,
        5 if !b => ST_DISP8_BP,
        _ => ST_DISP8,
    }
}

/// The absolute fields that the decoder tells of (§5.7 of FORMAT.md, the base relocations'
/// predictions): of 4 bytes in x86-32 and of 8 in x86-64, those whose values `v` are such that
/// `v - base` (wrapping) is from `lo` to `hi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub base: u64,
    pub lo: u64,
    pub hi: u64,
}

impl Fields {
    /// Of a field at `at` of `size` bytes holding `value`, its address less `base` (of 32 bits),
    /// where it is one of them and ends by `end`.
    #[inline]
    pub fn take(&self, at: u64, value: u64, size: usize, is64: bool, end: u64) -> Option<u32> {
        let ok = size == if is64 { 8 } else { 4 }
            && at.checked_add(size as u64).is_some_and(|e| e <= end)
            && (self.lo..=self.hi).contains(&value.wrapping_sub(self.base));
        ok.then(|| at.wrapping_sub(self.base) as u32)
    }
}

/// Padding records (§5.5 of FORMAT.md): none, `CC` up to the next multiple of 16 of the
/// address (16 bytes from one), a single `CC`, or the bytes in chunks of their lengths.
const PAD_NONE: u8 = 0;
const PAD_FILL: u8 = 1;
const PAD_INT3: u8 = 2;
const PAD_BYTES: u8 = 3;

/// The bytes of [`PAD_FILL`] at `addr`.
const fn pad_fill_len(addr: u64) -> usize {
    16 - (addr & 0xf) as usize
}

#[cfg(feature = "encode")]
#[derive(Debug)]
pub struct EncodedStreams {
    origin: u64,
    sentinel_size: u8,
    streams: [Vec<u8>; NUM_STREAMS],
    side: Vec<u8>,
}

#[cfg(feature = "encode")]
impl EncodedStreams {
    fn new(origin: u64) -> Self {
        Self { origin, sentinel_size: 0, streams: array::from_fn(|_| Vec::new()), side: Vec::new() }
    }

    fn check(&self, st: usize, size: usize) {
        let expected = STREAM_SIZES[st];
        if expected > 0 {
            debug_assert_eq!(size, expected);
        }
    }

    fn put8(&mut self, st: usize, v: u8) {
        if_trace_encode! {
            print!("({}:{v:02X})", &STREAM_NAMES[st][3..]);
        }
        self.check(st, 1);
        self.streams[st].push(v);
    }

    fn put32(&mut self, st: usize, v: u32) {
        if_trace_encode! {
            print!("({}:{v:08X})", &STREAM_NAMES[st][3..]);
        }
        self.check(st, 4);
        self.streams[st].extend_from_slice(&v.to_le_bytes());
    }

    fn put64(&mut self, st: usize, v: u64) {
        if_trace_encode! {
            print!("({}:{v:016X})", &STREAM_NAMES[st][3..]);
        }
        self.check(st, 8);
        self.streams[st].extend_from_slice(&v.to_le_bytes());
    }

    fn copy(&mut self, st: usize, v: &[u8]) {
        if_trace_encode! {
            print!("({}:{})", &STREAM_NAMES[st][3..], {
                use core::fmt;
                struct Hex<'a>(&'a [u8]);
                impl fmt::Display for Hex<'_> {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        for b in self.0 {
                            write!(f, "{b:02X}")?;
                        }
                        Ok(())
                    }
                }
                Hex(v)
            });
        }
        self.check(st, v.len());
        self.streams[st].extend_from_slice(v);
    }

    fn op(&mut self, map: u8, op: u8) {
        if map > 0 || op & 0xf0 == 0x40 {
            self.op_map(map);
        }
        self.put8(ST_OP, op);
    }

    fn op_esc(&mut self, b: u8) {
        if_trace_encode! {
            //print!("(OP:{ESC:02X}=ESC)(OP:{b:02X})");
            print!("(OP:{ESC:02X})(OP:{b:02X})");
        }
        self.streams[ST_OP].extend_from_slice(&[ESC, b]);
    }

    fn op_map(&mut self, m: u8) {
        let b = 0x40 + m;
        if_trace_encode! {
            //print!("(OP:{b:02X}=MAP{m:X})");
            print!("(OP:{b:02X})");
        }
        assert!(m < 16);
        self.streams[ST_OP].push(b);
    }

    fn call32(&mut self, idx: u8, target: u32) {
        if idx == 0xff {
            if_trace_encode! {
                print!("(CALL_IDX:{idx:02X})(CALL32:{target:08X})");
            }
            self.streams[ST_CALL_IDX].push(idx);
            self.streams[ST_CALL32].extend_from_slice(&target.to_le_bytes());
        } else {
            if_trace_encode! {
                print!("(CALL_IDX:{idx:02X})");
            }
            self.streams[ST_CALL_IDX].push(idx);
        }
    }

    fn call64(&mut self, idx: u8, target: u64) {
        if idx == 0xff {
            if_trace_encode! {
                print!("(CALL_IDX:{idx:02X})(CALL64:{target:016X})");
            }
            self.streams[ST_CALL_IDX].push(idx);
            self.streams[ST_CALL64].extend_from_slice(&target.to_le_bytes());
        } else {
            if_trace_encode! {
                print!("(CALL_IDX:{idx:02X})");
            }
            self.streams[ST_CALL_IDX].push(idx);
        }
    }

    fn jumptab(&mut self, count: u8) {
        if_trace_encode! {
            //print!("(OP:{JUMPTAB:02X}=JUMPTAB)(JUMPTAB_COUNT:{count:02X})");
            print!("(OP:{JUMPTAB:02X})(OP:{count:02X})");
        }
        self.streams[ST_OP].push(JUMPTAB);
        self.streams[ST_JUMPTAB_COUNT].push(count);
    }

    /// Virtual address of the code's first byte.
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// Bytes of the sentinel the decoder drops from the end.
    pub fn sentinel_size(&self) -> u8 {
        self.sentinel_size
    }

    /// The streams, named by [`STREAM_NAMES`].
    pub fn streams(&self) -> &[Vec<u8>; NUM_STREAMS] {
        &self.streams
    }

    /// The side output the decoder makes of the [`Fields`] given ([`encode_fields`]): the
    /// fields' addresses less the base, as `le32`, in order; empty for none.
    pub fn side(&self) -> &[u8] {
        &self.side
    }

    pub fn write_to(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        let mut mask = 0u64;
        for (i, stream) in self.streams.iter().enumerate() {
            if !stream.is_empty() {
                mask |= 1 << i as u64;
            }
        }

        const { assert!(NUM_STREAMS <= 60) };

        let mut header = vec![];
        header.extend_from_slice(&self.origin.to_le_bytes());
        header.push(self.sentinel_size);
        header.extend_from_slice(&mask.to_le_bytes());
        for stream in &self.streams {
            if !stream.is_empty() {
                header.extend_from_slice(&(stream.len() as u32).to_le_bytes());
            }
        }
        w.write_all(&header)?;

        for stream in &self.streams {
            w.write_all(stream)?;
        }

        Ok(())
    }
}

#[inline(always)]
const fn rex_has_w(rex: u8) -> bool {
    rex & 0x08 != 0
}

//      ___       ____        ____
// VEX3 RXB0mmmm WvvvvLpp -> 1vvvvLpp 0000WRXB + map mmmm
//         ^
// The bit 4 of the second byte is technically the fifth map bit,
// which gets ignored by the current encoding scheme
// because it is currently completely unused.
#[inline(always)]
fn shuffle_vex3([x, y]: [u8; 2]) -> Option<([u8; 2], u8)> {
    if x & 0x10 != 0 {
        return None;
    }

    let map = x & 0x0f;
    let rex = (y >> 4) & 0x08 | (!x >> 5);
    let vex = 0x80 | (y & 0x7f);
    Some(([vex, rex], map))
}

#[inline(always)]
fn unshuffle_vex3([vex, rex]: [u8; 2], map: u8) -> Option<[u8; 2]> {
    if vex & 0x80 == 0 || rex & 0xf0 != 0 || map >= 16 {
        return None;
    }

    let x = (!rex & 0x07) << 5 | map;
    let y = (rex & 0x08) << 4 | (vex & 0x7f);
    Some([x, y])
}

//      _____        ____
// VEX2 RvvvvLpp -> 1vvvvLpp 00000R00 + map 1 (implied)
#[inline(always)]
fn shuffle_vex2([x]: [u8; 1]) -> ([u8; 2], u8) {
    let rex = (!x >> 5) & 0x04;
    let vex = 0x80 | (x & 0x7f);
    ([vex, rex], 1)
}

#[inline(always)]
fn unshuffle_vex2([vex, rex]: [u8; 2], map: u8) -> Option<[u8; 1]> {
    if vex & 0x80 == 0 || rex & 0xfb != 0 || map != 1 {
        return None;
    }

    let x = (!rex & 0x04) << 5 | (vex & 0x7f);
    Some([x])
}

//      _____     _____       _                _____
// EVEX RXBrbmmm Wvvvvxpp **L*V*** -> **0*0*** VvvvvLpp 0rxbWRXB + map mmm
#[inline(always)]
fn shuffle_evex([x, y, z]: [u8; 3]) -> ([u8; 3], u8) {
    let map = x & 0x07;
    let rex = (!x & 0x10) << 2 | (!x & 0x08) << 1 | (!x >> 5) | (!y & 0x04) << 3 | (y >> 4) & 0x08;
    let vex = (y & 0x7b) | (z >> 3) & 0x04 | (z & 0x08) << 4;
    let evex = z & 0xd7;
    ([evex, vex, rex], map)
}

#[inline(always)]
fn unshuffle_evex([evex, vex, rex]: [u8; 3], map: u8) -> Option<[u8; 3]> {
    if evex & 0x28 != 0 || rex & 0x80 != 0 || map >= 8 {
        return None;
    }

    let x = (!rex & 0x07) << 5 | (!rex >> 2) & 0x10 | (!rex >> 1) & 0x08 | map;
    let y = (rex & 0x08) << 4 | (vex & 0x7b) | (!rex >> 3) & 0x04;
    let z = evex | (vex & 0x04) << 3 | (vex >> 4) & 0x08;
    Some([x, y, z])
}

#[inline(always)]
const fn parse_modrm(modrm: u8) -> (u8 /*mode*/, u8 /*base*/) {
    (modrm >> 6, modrm & 0b111)
}

#[inline(always)]
const fn modrm_is_reg_only((mode, _base): (u8, u8)) -> bool {
    mode == 0b11
}

#[inline(always)]
const fn modrm_reg(modrm: u8) -> u8 {
    modrm >> 3 & 7
}

#[inline(always)]
const fn modrm_has_sib((mode, base): (u8, u8)) -> bool {
    mode < 0b11 && base == 0b100
}

fn range_chunks(count: usize, chunk_size: usize) -> impl Iterator<Item = ops::Range<usize>> {
    let remainder = count % chunk_size;
    (0..count - remainder).step_by(chunk_size).map(move |start| start..start + chunk_size).chain(if remainder > 0 {
        Some(count - remainder..count)
    } else {
        None
    })
}

// Try to recognize common function boundary padding starting from `code[0]`:
// - `00` (ADD Eb,Gb)
// - `90` (NOP)
// - `0F 1F /0` (NOP E[bv])
//   `0F 1F 00'000'reg`
//   `0F 1F 00'000'100 zz'zzz'zzz`
//   `0F 1F 00'000'100 zz'zzz'101 XX XX XX XX`
//   `0F 1F 00'000'101 XX XX XX XX`
//   `0F 1F 01'000'reg XX`
//   `0F 1F 01'000'100 ZZ XX`
//   `0F 1F 10'000'reg XX XX XX XX`
//   `0F 1F 10'000'100 ZZ XX XX XX XX`
// - `CC` (INT3)
// - `89 11'reg'reg` (MOV R#,R#)
// - `8D 00'reg'reg` (LEA R#,[R#]) where reg != 101
// - `8D 01'reg'reg 00` (LEA R#,[R#+00h])
// - `8D 10'reg'reg 00 00 00 00` (LEA R#,[R#+00000000h])
// - `8D 00'reg'100 zz'100'reg (LEA R#,[R#*1]) where reg != 101
// - `8D 01'reg'100 zz'100'reg 00` (LEA R#,[R#*1+00h]) where reg != 101
// - `8D 10'reg'100 zz'100'reg 00 00 00 00` (LEA R#,[R#*1+00000000h]) where reg != 101
// - Any 1-byte prefix besides from LOCK and REX: `26 2E 36 3E 64 65 66 67 F2 F3`
fn scan_pad(mut code: &[u8]) -> usize {
    let len = code.len();

    // Skip any 00 bytes only at the very beginning of possible padding.
    let n = code.iter().position(|&b| b != 0x00).unwrap_or(len);
    code = &code[n..];

    loop {
        code = match code {
            [0xcc, rest @ ..]
            | [0x90, rest @ ..]
            | [0x66, 0x90, rest @ ..]
            | [0x0f, 0x1f, 0x00, rest @ ..]
            | [0x0f, 0x1f, 0x40, _, rest @ ..]
            | [0x0f, 0x1f, 0x44, 0x00, _, rest @ ..]
            | [0x66, 0x0f, 0x1f, 0x44, 0x00, _, rest @ ..]
            | [0x0f, 0x1f, 0x80, _, _, _, _, rest @ ..]
            | [0x0f, 0x1f, 0x84, 0x00, _, _, _, _, rest @ ..]
            | [0x66, 0x0f, 0x1f, 0x84, 0x00, _, _, _, _, rest @ ..]
            | [0x66, 0x66, 0x0f, 0x1f, 0x84, 0x00, _, _, _, _, rest @ ..]
            | [0x66, 0x66, 0x66, 0x0f, 0x1f, 0x84, 0x00, _, _, _, _, rest @ ..] => rest,

            _ => break,
        };
    }
    len - code.len()
}

const fn pad_followed(op: u16) -> bool {
    matches!(op, 0x9a | 0xe8 | 0xe9 | 0xea | 0xeb | 0xc2 | 0xc3 | 0xca | 0xcb | 0xcc | 0xcf)
}

struct CallCache {
    cache: [u64; 0x100],
}

impl CallCache {
    fn new() -> Self {
        Self { cache: [0u64; 0x100] }
    }

    #[inline(always)]
    fn find_index(&self, target: u64) -> u8 {
        self.cache.iter().position(|&cached| cached == target).unwrap_or(0xff) as u8
    }

    #[inline(always)]
    fn find_target(&self, idx: u8) -> Option<u64> {
        if idx < 0xff {
            Some(self.cache[idx as usize])
        } else {
            None
        }
    }

    #[inline(always)]
    fn record(&mut self, idx: u8, target: u64) {
        self.cache.copy_within(0..(idx as usize), 1);
        self.cache[0] = target;
    }
}

/// What a reference that the filter makes absolute is ([`encode_with`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ref {
    /// The target of `CALL rel32`.
    Call,
    /// The target of another jump of a 32-bit offset (`JMP`, `Jcc`).
    Jump,
    /// `[rip+disp32]` in x86-64, `[disp32]` in x86-32.
    Address,
}

/// A jump whose count waits for its target to be reached ([`Counts`]).
#[cfg(feature = "encode")]
struct Waiting {
    /// `JUMP8` or `JUMPN`, and where its count is in it.
    st: usize,
    at: usize,
    /// The index of the step after the jump's, and the jump's among the jumps.
    next: usize,
    seq: usize,
    /// Of `JUMP8`, the offset as it is; of `JUMPN`, where its field is, its target, and where in
    /// `JUMP` it goes if escaped.
    rel8: u8,
    field: u64,
    target: u64,
    jump_at: usize,
}

/// The jumps written as counts (§5.6 of FORMAT.md) as the encoder goes: the steps' addresses,
/// the jumps waiting for their targets, and what the escaped ones put in the streams after.
#[cfg(feature = "encode")]
struct Counts {
    /// Steps begun, and the addresses of the last [`COUNT_STEPS`] (by index modulo).
    n: usize,
    at: [u64; COUNT_STEPS],
    /// By target, then in order (the index into `jumps`).
    waiting: alloc::collections::BinaryHeap<core::cmp::Reverse<(u64, usize)>>,
    jumps: Vec<Waiting>,
    /// Jumps so far.
    seq: usize,
    /// Bytes put into `JUMP8`, and the targets into `JUMP`, once the streams are done: where,
    /// the jump's order, and what (of a target, where its field is).
    into_jump8: Vec<(usize, usize, u8)>,
    into_jump: Vec<(usize, usize, u64, u64)>,
}

#[cfg(feature = "encode")]
impl Counts {
    fn new() -> Self {
        Counts {
            n: 0,
            at: [0; COUNT_STEPS],
            waiting: Default::default(),
            jumps: Vec::new(),
            seq: 0,
            into_jump8: Vec::new(),
            into_jump: Vec::new(),
        }
    }

    /// A step (an item, not a padding record) starts at `addr`: the jumps waiting for it, or
    /// past it, are written.
    #[inline]
    fn step(&mut self, st: &mut EncodedStreams, addr: u64) {
        let k = self.n;
        self.at[k % COUNT_STEPS] = addr;
        self.n += 1;
        while let Some(&core::cmp::Reverse((target, j))) = self.waiting.peek() {
            if target > addr {
                break;
            }
            self.waiting.pop();
            let w = &self.jumps[j];
            if target == addr && (k - w.next) as i64 <= MAX_COUNT {
                st.streams[w.st][w.at] = (k - w.next) as u8;
            } else {
                self.escape(j);
            }
        }
    }

    fn escape(&mut self, j: usize) {
        let w = &self.jumps[j];
        if w.st == ST_JUMP8 {
            self.into_jump8.push((w.at + 1, w.seq, w.rel8));
        } else {
            self.into_jump.push((w.jump_at, w.seq, w.field, w.target));
        }
    }

    /// The count of a jump to `target` of stream `stream` (its offset `rel8` in `JUMP8`, its
    /// field at `field`), the next step at `next`: of a step behind, now; [`COUNT_ESC`] while
    /// waiting for one ahead; `None` where it is not one (`JUMP8`'s byte then for the caller to
    /// write, `JUMP`'s target put in its place).
    fn jump(&mut self, st: &EncodedStreams, stream: usize, rel8: u8, field: u64, target: u64, next: u64) -> Option<u8> {
        let (k, seq) = (self.n, self.seq);
        self.seq += 1;
        let jump_at = st.streams[ST_JUMP].len();
        let c = if target < next {
            // the steps of the last `MAX_COUNT`, ascending
            let lo = k.saturating_sub(MAX_COUNT as usize);
            let (mut a, mut b) = (lo, k);
            while a < b {
                let m = (a + b) / 2;
                if self.at[m % COUNT_STEPS] < target {
                    a = m + 1;
                } else {
                    b = m;
                }
            }
            (a < k && self.at[a % COUNT_STEPS] == target).then(|| ((k - a) as u8).wrapping_neg())
        } else if target - next <= COUNT_REACH as u64 {
            let at = st.streams[stream].len();
            self.waiting.push(core::cmp::Reverse((target, self.jumps.len())));
            self.jumps.push(Waiting { st: stream, at, next: k, seq, rel8, field, target, jump_at });
            Some(COUNT_ESC)
        } else {
            None
        };
        if c.is_none() && stream == ST_JUMPN {
            self.into_jump.push((jump_at, seq, field, target));
        }
        c
    }

    /// Puts the escaped jumps' bytes into the streams, those still waiting escaped, the targets
    /// as `hook` gives them (told of them in order).
    fn finish(mut self, st: &mut EncodedStreams, is64: bool, hook: &mut dyn FnMut(Ref, u64, u64) -> u64) {
        while let Some(core::cmp::Reverse((_, j))) = self.waiting.pop() {
            self.escape(j);
        }
        fn splice<const N: usize>(s: &mut Vec<u8>, mut ins: Vec<(usize, usize, [u8; N])>, n: usize) {
            if ins.is_empty() {
                return;
            }
            let mut out = Vec::with_capacity(s.len() + ins.len() * n);
            let mut from = 0;
            for (at, _, b) in ins.drain(..) {
                out.extend_from_slice(&s[from..at]);
                out.extend_from_slice(&b[..n]);
                from = at;
            }
            out.extend_from_slice(&s[from..]);
            *s = out;
        }
        let mut j8: Vec<_> = self.into_jump8.into_iter().map(|(at, j, b)| (at, j, [b])).collect();
        j8.sort_unstable_by_key(|x| (x.0, x.1));
        splice(&mut st.streams[ST_JUMP8], j8, 1);
        self.into_jump.sort_unstable_by_key(|x| (x.0, x.1));
        let j = self.into_jump.into_iter().map(|(at, j, field, t)| (at, j, hook(Ref::Jump, field, t).to_le_bytes()));
        splice(&mut st.streams[ST_JUMP], j.collect(), if is64 { 8 } else { 4 });
    }
}

#[cfg(feature = "encode")]
pub fn encode(input: &[u8], origin: u64, is64: bool) -> Option<EncodedStreams> {
    encode_with(input, origin, is64, &mut |_, _, v| v)
}

/// [`encode`], the side output of `fields` made too ([`EncodedStreams::side`]).
#[cfg(feature = "encode")]
pub fn encode_fields(input: &[u8], origin: u64, is64: bool, fields: Fields) -> Option<EncodedStreams> {
    encode_core(input, origin, is64, Some(Counts::new()), Some(fields), &mut |_, _, v| v)
}

/// [`encode`], each reference of [`Ref`] written as `hook` gives it: of its kind, the address
/// of its field and its absolute value (of which an address of x86-32 is the low 32 bits). The
/// jumps written as counts are none (§5.6 of FORMAT.md), and `hook` is told of the others once
/// the code is done, in order (of the calls and the addresses as they come).
#[cfg(feature = "encode")]
pub fn encode_with(
    input: &[u8],
    origin: u64,
    is64: bool,
    hook: &mut dyn FnMut(Ref, u64, u64) -> u64,
) -> Option<EncodedStreams> {
    encode_core(input, origin, is64, Some(Counts::new()), None, hook)
}

/// [`encode_with`] with no jump as a count, `hook` told of every reference as it comes: the
/// references of the code (for `remap`'s matching), not streams any module takes.
#[cfg(feature = "encode")]
pub(crate) fn encode_refs(
    input: &[u8],
    origin: u64,
    is64: bool,
    hook: &mut dyn FnMut(Ref, u64, u64) -> u64,
) -> Option<EncodedStreams> {
    encode_core(input, origin, is64, None, None, hook)
}

/// [`encode_with`] with `counts`, else [`encode_refs`]; the side output of `fields`, if given.
#[cfg(feature = "encode")]
#[allow(clippy::eq_op)] // `copy(8 / 8, ...)` spells out the bit width
fn encode_core(
    input: &[u8],
    mut origin: u64,
    is64: bool,
    mut counts: Option<Counts>,
    fields: Option<Fields>,
    hook: &mut dyn FnMut(Ref, u64, u64) -> u64,
) -> Option<EncodedStreams> {
    let opcodes = &Opcodes::BUILTIN;
    let mut st = EncodedStreams::new(origin);

    let mut call_cache = CallCache::new();
    let mut call_idx = |target: u64| {
        let idx = call_cache.find_index(target);
        call_cache.record(idx, target);
        idx
    };

    let code_len = input.len();
    let code_start = origin;
    let code_end = origin + code_len as u64;
    // (a field told of, at `at` of `n` bytes)
    let field = |st: &mut EncodedStreams, at: u64, v: u64, n: usize| {
        if let Some(r) = fields.as_ref().and_then(|f| f.take(at, v, n, is64, code_end)) {
            st.side.extend_from_slice(&r.to_le_bytes());
        }
    };

    let mut prefixes = 0;
    let mut pad = false;

    // Encodes `code` up to its last SENTINEL bytes, the last `slack` of which are zeros after
    // the code; returns the bytes left.
    let mut run = |mut code: &[u8], slack: usize| -> usize {
        while code.len() > SENTINEL {
            if let Some(c) = counts.as_mut().filter(|_| !pad) {
                c.step(&mut st, origin);
            }
            if_trace_encode! {
                println!();
                print!("{origin:06X}: ");
                //print!("{:02X?} ", &code[..SENTINEL]);
            }

            // Try to skip any padding.
            if pad {
                let pad_size = scan_pad(&code[..code.len() - slack]);
                let mut padding;
                (padding, code) = code.split_at(pad_size);
                let int3 = padding.iter().all(|&b| b == 0xcc);
                if pad_size == 0 {
                    st.put8(ST_PAD, PAD_NONE);
                } else if int3 && pad_size == pad_fill_len(origin) {
                    st.put8(ST_PAD, PAD_FILL);
                } else if int3 && pad_size == 1 {
                    st.put8(ST_PAD, PAD_INT3);
                } else {
                    st.put8(ST_PAD, PAD_BYTES);
                    while padding.len() >= 0xff {
                        let chunk;
                        (chunk, padding) = padding.split_at(0xff);
                        st.put8(ST_PAD, 0xff);
                        st.copy(ST_PAD, chunk);
                    }
                    st.put8(ST_PAD, padding.len() as u8);
                    st.copy(ST_PAD, padding);
                }
                origin += pad_size as u64;

                pad = false;
                continue;
            }

            // Detect a possible jump table of at least 3 entries.
            const MIN_JUMPTAB: usize = 3;
            if !is64 {
                // TBW
                let mut i = 0;
                let min_addr = code_start as u32;
                let max_addr = (code_end - 1) as u32;
                while i + 4 <= code.len() - slack {
                    let addr = to_u32(&code[i..i + 4]);
                    if addr < min_addr || max_addr < addr {
                        break;
                    }
                    i += 4;
                }
                if i >= MIN_JUMPTAB * 4 {
                    for range in range_chunks(i / 4, 0x100) {
                        // (each table a step, as the decoder reads them)
                        if let Some(c) = counts.as_mut().filter(|_| range.start > 0) {
                            c.step(&mut st, origin + (range.start * 4) as u64);
                        }
                        st.jumptab((range.len() - 1) as u8);
                        for j in range {
                            let addr = to_u32(&code[j * 4..j * 4 + 4]);
                            field(&mut st, origin + (j * 4) as u64, addr as u64, 4);
                            st.call32(call_idx(addr as u64), addr);
                        }
                    }
                    code = &code[i..];
                    origin += i as u64;
                    continue;
                }
            } else if origin.is_multiple_of(8) {
                // In x86-64, jump tables are typically 64-bit aligned offsets.
                // Since we don't know where they will be used,
                // we assume that they may be used anywhere within this `code`.
                let min_offset = -(code_len as i64 - 1);
                let max_offset = code_len as i64 - 1;
                let mut i = 0;
                while i + 8 <= code.len() - slack {
                    let offset = to_u64(&code[i..i + 8]) as i64;
                    if offset < min_offset || max_offset < offset {
                        break;
                    }
                    i += 8;
                }
                if i >= MIN_JUMPTAB * 8 {
                    for range in range_chunks(i / 8, 0x100) {
                        // (each table a step, as the decoder reads them)
                        if let Some(c) = counts.as_mut().filter(|_| range.start > 0) {
                            c.step(&mut st, origin + (range.start * 8) as u64);
                        }
                        st.jumptab((range.len() - 1) as u8);
                        for j in range {
                            st.put64(ST_JUMPTAB64, to_u64(&code[j * 8..j * 8 + 8]));
                        }
                    }
                    code = &code[i..];
                    origin += i as u64;
                    continue;
                }
            }

            let mut i = 0;
            let mut pre = 0;
            let mut evex = 0;
            let mut vex = 0;
            let mut rex = 0;
            let mut map = 0;
            let mut op = 0;

            // Handle prefixes that cannot be combined first.
            // They are all followed by ModR/M, where mode = 0b11 is required in x86-32.
            if is64 || modrm_is_reg_only(parse_modrm(code[1])) {
                match code[0] {
                    PRE_VEX3 => {
                        if let Some(ret) = shuffle_vex3([code[1], code[2]]) {
                            pre = PRE_VEX3;
                            ([vex, rex], map) = ret;
                            op = code[3];
                            i = 4;
                        }
                    }

                    PRE_VEX2 => {
                        pre = PRE_VEX2;
                        ([vex, rex], map) = shuffle_vex2([code[1]]);
                        op = code[2];
                        i = 3;
                    }

                    PRE_EVEX => {
                        pre = PRE_EVEX;
                        ([evex, vex, rex], map) = shuffle_evex([code[1], code[2], code[3]]);
                        op = code[4];
                        i = 5;
                    }

                    _ => {}
                }
            }

            // *VEX cannot coexist with 0F or REX.
            let has_vex = i > 0;
            if !has_vex {
                if is64 {
                    let c = code[0];
                    if c & 0xf0 == 0x40 {
                        // REX (40..4F)
                        pre = REX_MARKER;
                        rex = c & 0x0f;
                        i = 1;
                    } else if c == PRE_REX2 {
                        pre = PRE_REX2;
                        rex = code[1];
                        i = 2;
                    }
                }

                if code[i] == PRE_2BYTE {
                    map = 1;
                    op = code[i + 1];
                    i += 2;
                } else {
                    op = code[i];
                    i += 1;
                }
            }

            let i = Cell::new(i);

            let fetch8 = || {
                let ret = code[i.get()];
                i.set(i.get() + 1);
                ret
            };

            let fetch32 = || {
                let ret = to_u32(&code[i.get()..i.get() + 4]);
                i.set(i.get() + 4);
                ret
            };

            let copy = |n: usize, streams: &mut EncodedStreams, st: usize| {
                debug_assert!(STREAM_SIZES[st] == 0 || STREAM_SIZES[st] == n);
                streams.copy(st, &code[i.get()..i.get() + n]);
                i.set(i.get() + n);
            };
            // (of an absolute field, told of)
            let copy_abs = |n: usize, streams: &mut EncodedStreams, st: usize| {
                let mut b = [0u8; 8];
                b[..n].copy_from_slice(&code[i.get()..i.get() + n]);
                field(streams, origin + i.get() as u64, u64::from_le_bytes(b), n);
                copy(n, streams, st);
            };

            let rel_to_abs =
                |addr: u32, delta: usize| (origin + (i.get() + delta) as u64).wrapping_add(addr as i32 as u64);

            let mut flags = opcodes.class(op, map, is64);

            // In x86-32 these can be regular opcodes (LES/LDS/BOUND/AAD), but the decoder
            // would see them as *VEX/REX2 prefixes in the ST_OP stream, so they have to be escaped.
            if pre == 0 && map == 0 && matches!(op, PRE_VEX3 | PRE_VEX2 | PRE_EVEX | PRE_REX2) {
                flags = XX;
            }

            if flags == BP {
                // 1-byte prefixes can't have any *VEX and REX prefix.
                if i.get() != 1 {
                    flags = XX;
                } else {
                    assert_eq!(map, 0);
                    prefixes |= 1 << prefix_hash(op);
                    st.op(map, op);
                    code = &code[1..];
                    origin += 1;
                    continue;
                }
            } else if op3_followed(flags) {
                // 3-byte opcode prefixes can't have any *VEX prefix which has its own map index.
                if has_vex {
                    flags = XX;
                } else {
                    assert_eq!(map, 1);
                    map = (flags - M2) + 2;
                    flags = (flags - M2) + R_;
                    op = fetch8();
                }
            }

            let mut prefixes = mem::replace(&mut prefixes, 0);

            if flags == XX {
                // Do NOT commit the current position if this instruction is invalid.
                st.op_esc(code[0]);
                code = &code[1..];
                origin += 1;
                continue;
            }

            // Now we can commit any prefixes and opcode.
            if pre != 0 {
                st.put8(ST_OP, pre);
                if pre == PRE_EVEX {
                    st.put8(ST_EVEX, evex);
                    st.put8(ST_VEX, vex);
                } else if pre == PRE_VEX2 || pre == PRE_VEX3 {
                    st.put8(ST_VEX, vex);
                }
                st.put8(ST_REX, rex);
            }
            st.op(map, op);

            let op = (map as u16) << 8 | op as u16;

            match op {
                // Parse an additional 16-bit immediate for these:
                //
                // 9A/EA: CALL/JMP Ap (16-bit segment + 32-bit address)
                // C8: ENTER Iw,Ib (16-bit immediate + 8-bit immediate)
                OP_CALLF | OP_JMPF | OP_ENTER => {
                    copy(16 / 8, &mut st, ST_IMM16);
                }

                // F6/F7: TEST E,I (/0-1) vs. NOT/NEG/[I]MUL/[I]DIV E (/2-7)
                OP_GRP3_1 | OP_GRP3_2 | OP_MAP4_GRP3_1 | OP_MAP4_GRP3_2 if modrm_reg(code[i.get()]) >= 2 => {
                    flags = R_;
                }

                // 0F B8: JMPE Jz (IA-64 only) vs. POPCNT Gv,Ev (F3)
                OP_JMPE_POPCNT if has_rep_prefix(prefixes) => {
                    flags = R_;
                }

                // MAP7 F8: URDMSR Rq,Id; UWRMSR Id,Rq (immediate size doesn't depend on 66)
                OP_URDMSR_UWRMSR => {
                    prefixes &= !(1 << prefix_hash(PRE_OSIZE));
                }

                _ => {}
            }

            pad = pad_followed(op);

            // ModR/M present
            if has_modrm(flags) {
                flags = modrm_to_imm(flags);

                let modrm = fetch8();
                st.put8(ST_MODRM, modrm);

                let (mode, base) = parse_modrm(modrm);
                let addr16 = has_addr16(prefixes, is64);
                let sib;
                if !addr16 && modrm_has_sib((mode, base)) {
                    sib = fetch8();
                    st.put8(ST_SIB, sib);
                } else {
                    sib = 0;
                }

                match mode {
                    0 if addr16 && base == 6 => copy(16 / 8, &mut st, ST_ADDR16), // [disp16]
                    0 if addr16 => {}
                    0 if base == 5 => {
                        let at = origin + i.get() as u64;
                        let addr = fetch32();
                        if is64 {
                            // [eip+disp32] or [rip+disp32]
                            // Note that we haven't fully decoded operands yet, hence a delta.
                            let delta = modrm_imm_size(flags, prefixes);
                            st.put64(ST_ADDR64, hook(Ref::Address, at, rel_to_abs(addr, delta)));
                        } else {
                            field(&mut st, at, addr as u64, 4);
                            st.put32(ST_ADDR32, hook(Ref::Address, at, addr as u64) as u32);
                            // [disp32]
                        }
                    }
                    0 if sib & 7 == 5 => copy_abs(32 / 8, &mut st, ST_DISP32), // [reg*scale+disp32]
                    // [reg+disp8] or [reg*scale+disp8]
                    1 => copy(8 / 8, &mut st, disp8_stream(base, sib, rex, addr16)),
                    2 if addr16 => copy(16 / 8, &mut st, ST_DISP16), // [regs+disp16]
                    2 => copy_abs(32 / 8, &mut st, ST_DISP32),       // [reg+disp32]
                    _ => {}
                }
            }

            match flags {
                J4 if has_jump16(prefixes, is64) => copy(16 / 8, &mut st, ST_JUMP16),
                J4 if op != OP_CALLN && counts.is_some() => {
                    let (field, next) = (origin + i.get() as u64, origin + (i.get() + 4) as u64);
                    let target = rel_to_abs(fetch32(), 0);
                    let c = counts.as_mut().unwrap().jump(&st, ST_JUMPN, 0, field, target, next);
                    st.put8(ST_JUMPN, c.unwrap_or(COUNT_ESC));
                }
                J4 => {
                    let at = origin + i.get() as u64;
                    let target = rel_to_abs(fetch32(), 0);
                    let kind = if op == OP_CALLN { Ref::Call } else { Ref::Jump };
                    let target = hook(kind, at, target);
                    if op == OP_CALLN {
                        let idx = call_idx(target);
                        if is64 {
                            st.call64(idx, target);
                        } else {
                            st.call32(idx, target as u32);
                        }
                    } else {
                        if is64 {
                            st.put64(ST_JUMP64, target);
                        } else {
                            st.put32(ST_JUMP32, target as u32);
                        }
                    }
                }

                A_ => {
                    let lgn = addr_lgn(op, prefixes, is64);
                    copy_abs(1 << lgn, &mut st, addr_stream(lgn));
                }

                JA if has_jump16(prefixes, is64) => copy(16 / 8, &mut st, ST_AJUMP16),
                JA => copy_abs(32 / 8, &mut st, ST_AJUMP32),
                J1 if counts.is_some() => {
                    let rel = fetch8();
                    let next = origin + i.get() as u64;
                    let target = next.wrapping_add(rel as i8 as u64);
                    match counts.as_mut().unwrap().jump(&st, ST_JUMP8, rel, next - 1, target, next) {
                        Some(c) => st.put8(ST_JUMP8, c),
                        None => {
                            st.put8(ST_JUMP8, COUNT_ESC);
                            st.put8(ST_JUMP8, rel);
                        }
                    }
                }
                J1 => copy(8 / 8, &mut st, ST_JUMP8),
                N_ => {}

                _ => {
                    assert!(matches!(flags, N1 | N2 | N4 | NZ));
                    if flags == NZ && !rex_has_w(rex) {
                        flags = N4;
                    }
                    if flags == N4 && has_osize_prefix(prefixes) {
                        flags = N2;
                    }
                    let lgn = (flags - N1) as usize;
                    copy_abs(1 << lgn, &mut st, imm_stream(lgn));
                }
            }

            let i = i.get();
            code = &code[i..];
            origin += i as u64;
        }
        code.len()
    };

    // The code is read where it is while an instruction can be read within it, and its last
    // bytes from a copy with SENTINEL zeros after them.
    let left = run(input, 0);
    let mut tail = [0u8; 2 * SENTINEL];
    tail[..left].copy_from_slice(&input[code_len - left..]);
    let left = run(&tail[..left + SENTINEL], SENTINEL);

    // The decoder expects a padding record after every padding-followed instruction,
    // even when the input ends right there.
    if pad {
        st.put8(ST_PAD, PAD_NONE);
    }
    if let Some(c) = counts {
        c.finish(&mut st, is64, hook);
    }

    st.sentinel_size = (SENTINEL - left) as u8;
    Some(st)
}

#[cfg(feature = "decode")]
#[derive(Debug)]
pub struct DecodedStreams<'a> {
    origin: u64,
    sentinel_size: u8,
    streams: [&'a [u8]; NUM_STREAMS],
}

#[cfg(feature = "decode")]
impl<'a> DecodedStreams<'a> {
    /// From the parts of an [`EncodedStreams`] (see its accessors).
    pub fn new(origin: u64, sentinel_size: u8, streams: [&'a [u8]; NUM_STREAMS]) -> Self {
        Self { origin, sentinel_size, streams }
    }
}

#[cfg(all(feature = "encode", feature = "decode"))]
impl<'a> From<&'a EncodedStreams> for DecodedStreams<'a> {
    fn from(streams: &'a EncodedStreams) -> Self {
        Self {
            origin: streams.origin,
            sentinel_size: streams.sentinel_size,
            streams: streams.streams.each_ref().map(|s| &s[..]),
        }
    }
}

#[cfg(feature = "decode")]
#[export_name = "disfilter_decode"]
pub fn decode(streams: &DecodedStreams<'_>, is64: bool) -> Option<Vec<u8>> {
    decode_with(streams, is64, &Opcodes::BUILTIN, 0)
}

/// [`decode`] by the opcode classes of `opcodes` (the module's resource).
#[cfg(feature = "decode")]
pub fn decode_with(streams: &DecodedStreams<'_>, is64: bool, opcodes: &Opcodes, capacity: usize) -> Option<Vec<u8>> {
    decode_hooked(streams, is64, opcodes, capacity, |_, _, _| Some(()))
}

/// The section of `streams` built onto `code`, which is told of the absolute fields
/// ([`Sink::field`]); `None` where the streams do not make one.
#[cfg(feature = "decode")]
pub fn decode_into<K: Sink>(streams: &DecodedStreams<'_>, is64: bool, opcodes: &Opcodes, code: &mut K) -> Option<()> {
    let mut src = Slices(streams.streams.each_ref().map(|s| &s[..]));
    decode_core(&mut src, code, streams.origin, streams.sentinel_size, is64, opcodes, |_, _, _| Some(()))
}

/// [`decode_with`], telling `hook` of each reference of [`Ref`] as it is written: its kind, its
/// value in the streams, and where its field is in the output (before the sentinel is cut off);
/// of the jumps, those not written as counts. Decoding fails where it fails.
#[cfg(feature = "decode")]
pub(crate) fn decode_hooked(
    streams: &DecodedStreams<'_>,
    is64: bool,
    opcodes: &Opcodes,
    capacity: usize,
    hook: impl FnMut(Ref, u64, usize) -> Option<()>,
) -> Option<Vec<u8>> {
    let mut code = crate::util::room(capacity)?;
    let mut src = Slices(streams.streams.each_ref().map(|s| &s[..]));
    decode_core(&mut src, &mut code, streams.origin, streams.sentinel_size, is64, opcodes, hook)?;
    Some(code)
}

/// The streams of the filter, each read in order ([`decode_core`]).
#[cfg(feature = "decode")]
pub trait Src {
    /// The next byte of stream `st`.
    fn read8(&mut self, st: usize) -> Option<u8>;
    /// The next `le32` of stream `st`.
    fn read32(&mut self, st: usize) -> Option<u32>;
    /// The next `le64` of stream `st`.
    fn read64(&mut self, st: usize) -> Option<u64>;
    /// The next byte of stream `st`, not taken.
    fn peek8(&mut self, st: usize) -> Option<u8>;
    /// Whether stream `st` has no bytes left.
    fn is_empty(&mut self, st: usize) -> bool;
    /// Appends the next `n` bytes of stream `st` to `out`.
    fn copy<K: Sink>(&mut self, n: usize, st: usize, out: &mut K) -> Option<()>;
}

/// The section being built ([`decode_core`]): bytes appended, none read back, some written over
/// (the fields of jumps as counts, §5.6 of FORMAT.md).
#[cfg(feature = "decode")]
pub trait Sink {
    fn push(&mut self, b: u8);
    fn extend_from_slice(&mut self, d: &[u8]);
    /// Writes `d` over the bytes from `at` on, kept by [`Sink::hold`].
    fn patch(&mut self, at: usize, d: &[u8]);
    /// Of an absolute field about to be appended at `at`, of `size` bytes holding `value`
    /// (§5.7 of FORMAT.md): nothing but where the side output is made.
    #[inline(always)]
    fn field(&mut self, _at: usize, _value: u64, _size: usize) {}
    /// Keeps the bytes from `at` on (none where `None`), to be written over: at most
    /// [`COUNT_REACH`] and a field of 4 bytes of them while the streams are valid (a sink may
    /// fail past that).
    fn hold(&mut self, at: Option<usize>);
    /// Appends `n` bytes `b`.
    fn fill(&mut self, n: usize, b: u8);
    /// Bytes appended so far.
    fn len(&self) -> usize;
    /// Whether none are.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Ends the section, `sentinel` bytes shorter; `None` where it is shorter than that.
    fn finish(&mut self, sentinel: usize) -> Option<()>;
}

#[cfg(feature = "decode")]
impl Sink for Vec<u8> {
    fn push(&mut self, b: u8) {
        Vec::push(self, b)
    }

    fn extend_from_slice(&mut self, d: &[u8]) {
        Vec::extend_from_slice(self, d)
    }

    fn patch(&mut self, at: usize, d: &[u8]) {
        self[at..at + d.len()].copy_from_slice(d)
    }

    fn hold(&mut self, _: Option<usize>) {}

    fn fill(&mut self, n: usize, b: u8) {
        self.resize(self.len() + n, b)
    }

    fn len(&self) -> usize {
        Vec::len(self)
    }

    fn finish(&mut self, sentinel: usize) -> Option<()> {
        self.truncate(self.len().checked_sub(sentinel).filter(|_| sentinel <= SENTINEL)?);
        Some(())
    }
}

/// The streams as slices, each taken from the front.
#[cfg(feature = "decode")]
struct Slices<'a>([&'a [u8]; NUM_STREAMS]);

#[cfg(feature = "decode")]
impl Src for Slices<'_> {
    #[inline(always)]
    fn read8(&mut self, st: usize) -> Option<u8> {
        let (&[head], tail) = self.0[st].split_first_chunk::<1>()?;
        if_trace_decode! {
            print!("({}:{head:02X})", &STREAM_NAMES[st][3..]);
        }
        self.0[st] = tail;
        Some(head)
    }

    #[inline(always)]
    fn read32(&mut self, st: usize) -> Option<u32> {
        let (&head, tail) = self.0[st].split_first_chunk::<4>()?;
        let head = u32::from_le_bytes(head);
        if_trace_decode! {
            print!("({}:{head:08X})", &STREAM_NAMES[st][3..]);
        }
        self.0[st] = tail;
        Some(head)
    }

    #[inline(always)]
    fn read64(&mut self, st: usize) -> Option<u64> {
        let (&head, tail) = self.0[st].split_first_chunk::<8>()?;
        let head = u64::from_le_bytes(head);
        if_trace_decode! {
            print!("({}:{head:016X})", &STREAM_NAMES[st][3..]);
        }
        self.0[st] = tail;
        Some(head)
    }

    #[inline(always)]
    fn peek8(&mut self, st: usize) -> Option<u8> {
        self.0[st].first().copied()
    }

    #[inline(always)]
    fn is_empty(&mut self, st: usize) -> bool {
        self.0[st].is_empty()
    }

    #[inline(always)]
    fn copy<K: Sink>(&mut self, n: usize, st: usize, out: &mut K) -> Option<()> {
        let (head, tail) = self.0[st].split_at_checked(n)?;
        if_trace_decode! {
            print!("({}:", &STREAM_NAMES[st][3..]);
            for &b in head {
                print!("{:02X}", b);
            }
            print!(")");
        }
        out.extend_from_slice(head);
        self.0[st] = tail;
        Some(())
    }
}

/// `n` bytes (2, 4 or 8) of stream `st` appended, an absolute field told of (§5.7).
#[cfg(feature = "decode")]
#[inline(always)]
fn copy_abs<S: Src, K: Sink>(src: &mut S, n: usize, st: usize, code: &mut K) -> Option<()> {
    let v = match n {
        4 => src.read32(st)? as u64,
        8 => src.read64(st)?,
        _ => return src.copy(n, st, code),
    };
    code.field(code.len(), v, n);
    code.extend_from_slice(&v.to_le_bytes()[..n]);
    Some(())
}

/// A call target read from the cache, or from `CALL` where it is not there (§5.4).
#[cfg(feature = "decode")]
#[inline(always)]
fn read_call<S: Src>(src: &mut S, cache: &mut CallCache, is64: bool) -> Option<u64> {
    let idx = src.read8(ST_CALL_IDX)?;
    let target = if let Some(target) = cache.find_target(idx) {
        target
    } else if is64 {
        src.read64(ST_CALL64)?
    } else {
        src.read32(ST_CALL32)? as u64
    };
    cache.record(idx, target);
    Some(target)
}

/// The decoder's steps, for the jumps as counts (§5.6 of FORMAT.md): where the last
/// [`COUNT_STEPS`] began, and the jumps waiting for steps ahead, in order in a ring of
/// [`COUNT_WAITING`] (a step makes one at most, and each waits for [`MAX_COUNT`] steps at most).
#[cfg(feature = "decode")]
struct Steps {
    /// Steps begun, and where the last [`COUNT_STEPS`] did (by index modulo).
    n: usize,
    at: [usize; COUNT_STEPS],
    /// Of each jump waiting: where its field is, its size (0 once written) and its target step.
    field: [usize; COUNT_WAITING],
    size: [u8; COUNT_WAITING],
    target: [usize; COUNT_WAITING],
    /// Jumps made to wait, the first of them that may still wait, and the nearest target of
    /// those waiting (`usize::MAX` where none is).
    made: usize,
    oldest: usize,
    soonest: usize,
}

#[cfg(feature = "decode")]
impl Steps {
    fn new() -> Self {
        Steps {
            n: 0,
            at: [0; COUNT_STEPS],
            field: [0; COUNT_WAITING],
            size: [0; COUNT_WAITING],
            target: [0; COUNT_WAITING],
            made: 0,
            oldest: 0,
            soonest: usize::MAX,
        }
    }

    /// Whether a jump still waits.
    fn waiting(&self) -> bool {
        self.soonest != usize::MAX
    }

    /// A step (an item, not a padding record) begins where the output is: the jumps waiting for
    /// it are written.
    #[inline(always)]
    fn begin<K: Sink>(&mut self, code: &mut K) -> Option<()> {
        let a = code.len();
        let k = self.n;
        self.at[k % COUNT_STEPS] = a;
        self.n += 1;
        if k == self.soonest {
            self.arrive(code, k, a)?;
        }
        Some(())
    }

    /// Step `k` begins at `a`, which jumps wait for.
    #[inline(always)]
    fn arrive<K: Sink>(&mut self, code: &mut K, k: usize, a: usize) -> Option<()> {
        let mut soonest = usize::MAX;
        for j in self.oldest..self.made {
            let e = j % COUNT_WAITING;
            let size = self.size[e] as usize;
            if size == 0 {
                continue;
            }
            if self.target[e] != k {
                soonest = soonest.min(self.target[e]);
                continue;
            }
            let rel = a - (self.field[e] + size);
            if rel > COUNT_REACH || (size == 1 && rel > 0x7f) {
                return None;
            }
            code.patch(self.field[e], &(rel as u32).to_le_bytes()[..size]);
            self.size[e] = 0;
        }
        self.soonest = soonest;
        let was = self.oldest;
        while self.oldest < self.made && self.size[self.oldest % COUNT_WAITING] == 0 {
            self.oldest += 1;
        }
        if self.oldest != was {
            code.hold((self.oldest < self.made).then(|| self.field[self.oldest % COUNT_WAITING]));
        }
        Some(())
    }

    /// A jump's count read from `st` (`JUMP8` for a field of 1 byte, `JUMPN` of 4), and its
    /// field appended (true), unless it is [`COUNT_ESC`] (false).
    #[inline(always)]
    fn read<S: Src, K: Sink>(&mut self, src: &mut S, code: &mut K, st: usize) -> Option<bool> {
        let c = src.read8(st)?;
        if c == COUNT_ESC {
            return Some(false);
        }
        self.jump(code, c, if st == ST_JUMP8 { 1 } else { 4 })?;
        Some(true)
    }

    /// A jump's field of `size` bytes (1 or 4) appended, of count `c` (not [`COUNT_ESC`]).
    #[inline(always)]
    fn jump<K: Sink>(&mut self, code: &mut K, c: u8, size: usize) -> Option<()> {
        let (field, c) = (code.len(), c as i8 as isize);
        let t = (self.n as isize).checked_add(c).filter(|&t| t >= 0)? as usize;
        if c < 0 {
            let rel = self.at[t % COUNT_STEPS].wrapping_sub(field + size) as isize;
            if size == 1 && rel < -0x80 {
                return None;
            }
            code.extend_from_slice(&(rel as u32).to_le_bytes()[..size]);
            return Some(());
        }
        let e = self.made % COUNT_WAITING;
        if self.made - self.oldest >= COUNT_WAITING {
            return None;
        }
        (self.field[e], self.size[e], self.target[e]) = (field, size as u8, t);
        if self.oldest == self.made {
            code.hold(Some(field));
        }
        self.made += 1;
        self.soonest = self.soonest.min(t);
        code.fill(size, 0);
        Some(())
    }
}

/// The filter's decoder: the section of `origin` on, built onto `code` from `src` (less
/// `sentinel_size` bytes at its end), telling `hook` of each reference as `decode_hooked` says.
/// `None` where the streams do not make one.
#[cfg(feature = "decode")]
#[allow(clippy::eq_op)] // `copy(8 / 8, ...)` spells out the bit width
pub fn decode_core<S: Src, K: Sink>(
    src: &mut S,
    code: &mut K,
    origin: u64,
    sentinel_size: u8,
    is64: bool,
    opcodes: &Opcodes,
    mut hook: impl FnMut(Ref, u64, usize) -> Option<()>,
) -> Option<()> {
    let mut call_cache = CallCache::new();
    let mut steps = Steps::new();

    macro_rules! fatal {
        ($fmt:tt) => {
            return None
        };
    }

    let mut prefixes = 0;
    let mut pad = false;

    while pad || !src.is_empty(ST_OP) {
        if !pad {
            steps.begin(code)?;
        }
        if_trace_decode! {
            println!();
            print!("{:06X}: ", origin + code.len() as u64);
        }

        if pad {
            match src.read8(ST_PAD)? {
                PAD_NONE => {}
                PAD_FILL => code.fill(pad_fill_len(origin + code.len() as u64), 0xcc),
                PAD_INT3 => code.push(0xcc),
                PAD_BYTES => loop {
                    let pad_size = src.read8(ST_PAD)?;
                    src.copy(pad_size as usize, ST_PAD, code)?;
                    if pad_size < 0xff {
                        break;
                    }
                },
                _ => return None,
            }

            pad = false;
            continue;
        }

        let mut op = src.read8(ST_OP).unwrap();

        if op == ESC {
            // The encoder discards any pending prefixes when escaping.
            prefixes = 0;
            code.push(src.read8(ST_OP)?);
            continue;
        }

        if op == JUMPTAB {
            let count = src.read8(ST_JUMPTAB_COUNT)? as usize + 1;
            if is64 {
                for _ in 0..count {
                    code.extend_from_slice(&src.read64(ST_JUMPTAB64)?.to_le_bytes());
                }
            } else {
                for _ in 0..count {
                    let t = read_call(src, &mut call_cache, is64)? as u32;
                    code.field(code.len(), t as u64, 4);
                    code.extend_from_slice(&t.to_le_bytes());
                }
            }
            continue;
        }

        let (pre, evex, vex, rex) = match op {
            PRE_VEX3 | PRE_VEX2 => (op, 0, src.read8(ST_VEX)?, src.read8(ST_REX)?),
            PRE_EVEX => (op, src.read8(ST_EVEX)?, src.read8(ST_VEX)?, src.read8(ST_REX)?),
            REX_MARKER | PRE_REX2 if !is64 => fatal!("unsupported REX prefixes in x86-32"),
            REX_MARKER => (op, 0, 0, src.read8(ST_REX)?),
            PRE_REX2 => (op, 0, 0, src.read8(ST_REX)?),
            _ => (0, 0, 0, 0),
        };
        if pre != 0 {
            op = src.read8(ST_OP)?;
        }

        let map;
        if op & 0xf0 == 0x40 {
            map = op & 0x0f;
            op = src.read8(ST_OP)?;
        } else {
            map = 0;
        }

        let mut flags = opcodes.class(op, map, is64);
        if flags == XX {
            fatal!("invalid opcode");
        } else if flags == BP {
            assert_eq!(map, 0);
            prefixes |= 1 << prefix_hash(op);
            code.push(op);
            continue;
        }

        'prefix: {
            match pre {
                PRE_VEX3 => {
                    let [x, y] = unshuffle_vex3([vex, rex], map)?; // (else a bad VEX3 prefix)
                    code.extend_from_slice(&[PRE_VEX3, x, y, op]);
                    break 'prefix;
                }
                PRE_VEX2 => {
                    let [x] = unshuffle_vex2([vex, rex], map)?; // (else a bad VEX2 prefix)
                    code.extend_from_slice(&[PRE_VEX2, x, op]);
                    break 'prefix;
                }
                PRE_EVEX => {
                    let [x, y, z] = unshuffle_evex([evex, vex, rex], map)?; // (else a bad EVEX prefix)
                    code.extend_from_slice(&[PRE_EVEX, x, y, z, op]);
                    break 'prefix;
                }

                REX_MARKER => {
                    if rex & 0xf0 != 0 {
                        fatal!("bad REX prefix");
                    }
                    code.push(0x40 | rex);
                }
                PRE_REX2 => code.extend_from_slice(&[PRE_REX2, rex]),
                0 => {}
                _ => unreachable!(),
            }

            // Only applicable with non-*VEX prefixes.
            match map {
                0 => code.push(op),
                1 => code.extend_from_slice(&[0x0f, op]),
                2 => code.extend_from_slice(&[0x0f, 0x38, op]),
                3 => code.extend_from_slice(&[0x0f, 0x3a, op]),
                _ => fatal!("bad opcode map"),
            }
        }

        let mut prefixes = mem::replace(&mut prefixes, 0);

        let op = (map as u16) << 8 | op as u16;

        match op {
            // Parse an additional 16-bit immediate for these:
            //
            // 9A/EA: CALL/JMP Ap (16-bit segment + 32-bit address)
            // C8: ENTER Iw,Ib (16-bit immediate + 8-bit immediate)
            OP_CALLF | OP_JMPF | OP_ENTER => {
                src.copy(16 / 8, ST_IMM16, code)?;
            }

            // F6/F7: TEST E,I (/0-1) vs. NOT/NEG/[I]MUL/[I]DIV E (/2-7)
            OP_GRP3_1 | OP_GRP3_2 | OP_MAP4_GRP3_1 | OP_MAP4_GRP3_2 if modrm_reg(src.peek8(ST_MODRM)?) >= 2 => {
                flags = R_;
            }

            // 0F B8: JMPE Jz (IA-64 only) vs. POPCNT Gv,Ev (F3)
            OP_JMPE_POPCNT if has_rep_prefix(prefixes) => {
                flags = R_;
            }

            // MAP7 F8: URDMSR Rq,Id; UWRMSR Id,Rq (immediate size doesn't depend on 66)
            OP_URDMSR_UWRMSR => {
                prefixes &= !(1 << prefix_hash(PRE_OSIZE));
            }

            _ => {}
        }

        pad = pad_followed(op);

        let abs_to_rel = |addr: u64, len: usize, delta: usize| addr.wrapping_sub(origin + (len + delta) as u64);

        // ModR/M present
        if has_modrm(flags) {
            flags = modrm_to_imm(flags);

            let modrm = src.read8(ST_MODRM)?;
            code.push(modrm);

            let (mode, base) = parse_modrm(modrm);
            let addr16 = has_addr16(prefixes, is64);
            let sib;
            if !addr16 && modrm_has_sib((mode, base)) {
                sib = src.read8(ST_SIB)?;
                code.push(sib);
            } else {
                sib = 0;
            }

            match mode {
                0 if addr16 && base == 6 => src.copy(16 / 8, ST_ADDR16, code)?, // [disp16]
                0 if addr16 => {}
                0 if base == 5 => {
                    let addr = if is64 {
                        // [eip+disp32] or [rip+disp32]
                        // Note that we haven't fully decoded operands yet, hence a delta.
                        let delta = modrm_imm_size(flags, prefixes);
                        let v = src.read64(ST_ADDR64)?;
                        hook(Ref::Address, v, code.len())?;
                        abs_to_rel(v, code.len(), delta + 4) as u32
                    } else {
                        let v = src.read32(ST_ADDR32)?; // [disp32]
                        hook(Ref::Address, v as u64, code.len())?;
                        code.field(code.len(), v as u64, 4);
                        v
                    };
                    code.extend_from_slice(&addr.to_le_bytes());
                }
                // [reg*scale+disp32]
                0 if sib & 7 == 5 => copy_abs(src, 32 / 8, ST_DISP32, code)?,
                // [reg+disp8] or [reg*scale+disp8]
                1 => src.copy(8 / 8, disp8_stream(base, sib, rex, addr16), code)?,
                2 if addr16 => src.copy(16 / 8, ST_DISP16, code)?, // [regs+disp16]
                2 => copy_abs(src, 32 / 8, ST_DISP32, code)?,      // [reg+disp32]
                _ => {}
            }
        }

        // a jump as a count, else (escaped) as below
        // (one place that the sink is handed to, inlined, so that it stays in locals)
        let counted = match flags {
            J1 => Some(ST_JUMP8),
            J4 if op != OP_CALLN && !has_jump16(prefixes, is64) => Some(ST_JUMPN),
            _ => None,
        };
        if let Some(st) = counted {
            if steps.read(src, code, st)? {
                continue;
            }
        }

        match flags {
            J4 if has_jump16(prefixes, is64) => src.copy(16 / 8, ST_JUMP16, code)?,
            J4 => {
                let target = if op == OP_CALLN {
                    read_call(src, &mut call_cache, is64)?
                } else if is64 {
                    src.read64(ST_JUMP64)?
                } else {
                    src.read32(ST_JUMP32)? as u64
                };
                hook(if op == OP_CALLN { Ref::Call } else { Ref::Jump }, target, code.len())?;
                let target = abs_to_rel(target, code.len(), 4) as u32;
                code.extend_from_slice(&target.to_le_bytes());
            }

            A_ => {
                let lgn = addr_lgn(op, prefixes, is64);
                copy_abs(src, 1 << lgn, addr_stream(lgn), code)?;
            }

            JA if has_jump16(prefixes, is64) => src.copy(16 / 8, ST_AJUMP16, code)?,
            JA => copy_abs(src, 32 / 8, ST_AJUMP32, code)?,
            J1 => src.copy(8 / 8, ST_JUMP8, code)?,
            N_ => {}

            _ => {
                assert!(matches!(flags, N1 | N2 | N4 | NZ));
                if flags == NZ && !rex_has_w(rex) {
                    flags = N4;
                }
                if flags == N4 && has_osize_prefix(prefixes) {
                    flags = N2;
                }
                let lgn = (flags - N1) as usize;
                copy_abs(src, 1 << lgn, imm_stream(lgn), code)?;
            }
        }
    }

    if steps.waiting() {
        return None;
    }
    code.finish(sentinel_size as usize)
}

pub mod remap;

#[cfg(all(test, feature = "encode", feature = "decode", not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn test_call_cache() {
        let mut expected = vec![(1234, 0xff), (1234, 0), (5678, 0xff), (1234, 1), (1234, 0), (5678, 1)];
        for i in 9000..9256 {
            expected.push((i, 0xff));
        }
        expected.push((1234, 0xff));
        expected.push((5678, 0xff));

        let mut cache = CallCache::new();
        for &(target, idx) in &expected {
            assert_eq!(cache.find_index(target), idx);
            cache.record(idx, target);
        }

        let mut cache = CallCache::new();
        for &(target, idx) in &expected {
            if idx == 0xff {
                assert_eq!(cache.find_target(idx), None);
            } else {
                assert_eq!(cache.find_target(idx), Some(target));
            }
            cache.record(idx, target);
        }
    }

    #[cfg(test)]
    fn test_shuffle<const IN: usize, const OUT: usize>(
        shuffle: impl Fn([u8; IN]) -> Option<([u8; OUT], u8)>,
        unshuffle: impl Fn([u8; OUT], u8) -> Option<[u8; IN]>,
        map_range: impl Iterator<Item = u8> + Clone,
    ) {
        #[inline(always)]
        fn generate<const N: usize>() -> impl Iterator<Item = [u8; N]> {
            assert!(N <= 3);
            (0..1u32 << (N * 8)).map(|n| array::from_fn(|i| (n >> (i * 8) & 0xff) as u8))
        }

        let mut seen = std::collections::HashSet::new();
        for i in generate::<IN>() {
            if let Some((o, map)) = shuffle(i) {
                assert_eq!(unshuffle(o, map), Some(i), "{i:02X?} -> {o:02X?} + {map} -> (roundtrip failed)");
                assert!(seen.insert((o, map)), "{i:02X?} -> {o:02X?} + {map} (dupe)");
            }
        }
        for o in generate::<OUT>() {
            for map in map_range.clone() {
                if !seen.contains(&(o, map)) {
                    assert_eq!(unshuffle(o, map), None, "? <- {o:02X?} + {map} (didn't fail)");
                }
            }
        }
    }

    #[test]
    fn test_shuffle_vex3() {
        test_shuffle(shuffle_vex3, unshuffle_vex3, 0..=16);
    }

    #[test]
    fn test_shuffle_vex2() {
        test_shuffle(|i| Some(shuffle_vex2(i)), unshuffle_vex2, 0..=16);
    }

    #[test]
    fn test_shuffle_evex() {
        test_shuffle(|i| Some(shuffle_evex(i)), unshuffle_evex, [5, 8].into_iter());
    }

    // Helper function: test encode/decode roundtrip for byte array
    fn roundtrip_test(code: &[u8], origin: u64, is64: bool) -> Result<(), String> {
        let input = code.to_vec();

        let encoded = encode(&input, origin, is64).ok_or("encoding failed")?;

        let decoded = decode(&(&encoded).into(), is64).ok_or("decoding failed")?;

        if input != decoded {
            let mismatch =
                input.iter().zip(&decoded).position(|(a, b)| a != b).unwrap_or(input.len().min(decoded.len()));

            let input_end = (mismatch + 8).min(input.len());
            let decoded_end = (mismatch + 8).min(decoded.len());

            return Err(format!(
                "Length mismatch or content mismatch at offset {}: input_len={}, decoded_len={}, input[{}..{}]={:02X?}, decoded[{}..{}]={:02X?}",
                mismatch,
                input.len(),
                decoded.len(),
                mismatch, input_end, &input[mismatch..input_end],
                mismatch, decoded_end, &decoded[mismatch..decoded_end]
            ));
        }

        Ok(())
    }

    #[test]
    fn test_rip_relative_addressing() {
        // Test cases for RIP-relative addressing
        let test_cases = vec![
            // MOV EAX, [RIP+disp32] - various displacements
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0x1000, "MOV EAX, [RIP+0]"),
            (vec![0x8b, 0x05, 0x10, 0x00, 0x00, 0x00], 0x1000, "MOV EAX, [RIP+16]"),
            (vec![0x8b, 0x05, 0xff, 0xff, 0xff, 0xff], 0x1000, "MOV EAX, [RIP-1]"),
            // LEA with RIP-relative
            (vec![0x48, 0x8d, 0x05, 0x00, 0x00, 0x00, 0x00], 0x2000, "LEA RAX, [RIP+0]"),
            (vec![0x48, 0x8d, 0x05, 0x20, 0x00, 0x00, 0x00], 0x2000, "LEA RAX, [RIP+32]"),
            // CMP with RIP-relative and immediate
            (vec![0x48, 0x83, 0x3d, 0x00, 0x00, 0x00, 0x00, 0x42], 0x3000, "CMP QWORD PTR [RIP+0], 0x42"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_modrm_edge_cases() {
        let test_cases = vec![
            // SIB with base=5 (EBP/RBP special case)
            (vec![0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00], 0x1000, "MOV EAX, [0]"),
            (vec![0x8b, 0x44, 0x25, 0x10], 0x1000, "MOV EAX, [EBP+10h]"),
            // ModR/M mode=0, base=5 (special case)
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0x1000, "MOV EAX, [RIP+0] (32-bit: [disp32])"),
            // Complex SIB combinations
            (vec![0x8b, 0x04, 0x85, 0x00, 0x00, 0x00, 0x00], 0x1000, "MOV EAX, [EAX*4+0]"),
            (vec![0x8b, 0x44, 0x85, 0x10], 0x1000, "MOV EAX, [EBP+EAX*4+10h]"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));

            // Test in both 64-bit and 32-bit modes (excluding RIP-relative cases)
            if !code.starts_with(&[0x8b, 0x05]) {
                roundtrip_test(&code, origin, false).unwrap_or_else(|e| panic!("Failed {} (32-bit): {}", desc, e));
            }
        }
    }

    #[test]
    fn test_rex_prefix_combinations() {
        let test_cases = vec![
            // REX.W
            (vec![0x48, 0x89, 0xc8], 0x1000, "REX.W MOV RAX, RCX"),
            // REX.R (extends ModR/M reg field)
            (vec![0x44, 0x89, 0xc0], 0x1000, "REX.R MOV EAX, R8D"),
            // REX.X (extends SIB index field)
            (vec![0x42, 0x8b, 0x04, 0x00], 0x1000, "REX.X MOV EAX, [RAX+R8*1]"),
            // REX.B (extends ModR/M r/m field)
            (vec![0x41, 0x89, 0xc0], 0x1000, "REX.B MOV R8D, EAX"),
            // All REX bit combinations
            (vec![0x4f, 0x89, 0x04, 0x08], 0x1000, "REX.WRXB MOV [R8+R9*1], RAX"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_immediate_size_variations() {
        let test_cases = vec![
            // 8-bit immediate
            (vec![0x83, 0xc0, 0x10], 0x1000, "ADD EAX, 10h"),
            // 16-bit immediate with 66 prefix
            (vec![0x66, 0x81, 0xc0, 0x00, 0x10], 0x1000, "ADD AX, 1000h"),
            // 32-bit immediate
            (vec![0x81, 0xc0, 0x00, 0x00, 0x00, 0x10], 0x1000, "ADD EAX, 10000000h"),
            // 64-bit immediate with REX.W
            (vec![0x48, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10], 0x1000, "MOV RAX, 1000000000000000h"),
            // Operand size prefix effect test
            (vec![0x66, 0xb8, 0x00, 0x10], 0x1000, "MOV AX, 1000h"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_call_instruction_variations() {
        let test_cases = vec![
            // Near call with 32-bit displacement
            (vec![0xe8, 0x00, 0x00, 0x00, 0x00], 0x1000, "CALL +0"),
            (vec![0xe8, 0x10, 0x00, 0x00, 0x00], 0x1000, "CALL +10h"),
            (vec![0xe8, 0xf0, 0xff, 0xff, 0xff], 0x1000, "CALL -10h"),
            // Indirect call
            (vec![0xff, 0xd0], 0x1000, "CALL EAX"),
            (vec![0xff, 0x15, 0x00, 0x00, 0x00, 0x00], 0x1000, "CALL [RIP+0]"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_jump_instruction_variations() {
        let test_cases = vec![
            // Short jump (8-bit)
            (vec![0xeb, 0x10], 0x1000, "JMP SHORT +10h"),
            (vec![0xeb, 0xf0], 0x1000, "JMP SHORT -10h"),
            // Near jump (32-bit)
            (vec![0xe9, 0x00, 0x00, 0x00, 0x00], 0x1000, "JMP +0"),
            (vec![0xe9, 0x00, 0x10, 0x00, 0x00], 0x1000, "JMP +1000h"),
            // Conditional jumps
            (vec![0x0f, 0x84, 0x00, 0x00, 0x00, 0x00], 0x1000, "JZ +0"),
            (vec![0x74, 0x10], 0x1000, "JZ SHORT +10h"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_vex_prefix_combinations() {
        let test_cases = vec![
            // VEX2 prefix
            (vec![0xc5, 0xf8, 0x58, 0xc1], 0x1000, "VEX2 VADDPS XMM0, XMM1"),
            // VEX3 prefix - map 1
            (vec![0xc4, 0xe1, 0xf8, 0x58, 0xc1], 0x1000, "VEX3 VADDPS XMM0, XMM1"),
            // VEX3 prefix - map 2
            (vec![0xc4, 0xe2, 0x79, 0x00, 0xc1], 0x1000, "VEX3 VPSHUFB XMM0, XMM1"),
        ];

        for (code, origin, desc) in test_cases {
            // VEX is only valid in 64-bit mode
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_edge_case_addresses() {
        // Test RIP-relative calculation at boundary addresses
        let test_cases = vec![
            // Near zero address
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0x10, "Low address"),
            // High address
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0xfffff000, "High address"),
            // Page boundary
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0x1000, "Page boundary"),
            (vec![0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], 0x0ffe, "Near page boundary"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_instruction_boundary_detection() {
        // Test correct boundary detection in complex instruction sequences
        let complex_sequence = vec![
            // CALL + padding pattern
            0xe8, 0x10, 0x00, 0x00, 0x00, // CALL +10h
            0x90, 0x90, 0x90, // NOP padding
            // MOV with immediate
            0x48, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, // MOV RAX, imm64
            // RIP-relative LEA
            0x48, 0x8d, 0x05, 0x00, 0x00, 0x00, 0x00, // LEA RAX, [RIP+0]
        ];

        roundtrip_test(&complex_sequence, 0x1000, true).unwrap_or_else(|e| panic!("Failed complex sequence: {}", e));
    }

    #[test]
    fn test_32bit_vs_64bit_differences() {
        // Instructions that are interpreted differently in 32-bit vs 64-bit modes
        let test_cases = vec![
            // PUSH/POP immediate - some only valid in 32-bit
            (vec![0x68, 0x00, 0x00, 0x00, 0x00], "PUSH imm32"),
            // General MOV
            (vec![0x89, 0xc0], "MOV EAX, EAX"),
            // Address size prefix effect
            (vec![0x67, 0x8b, 0x00], "MOV EAX, [EAX] with addr32 prefix"),
        ];

        for (code, desc) in test_cases {
            // Test in 64-bit mode
            roundtrip_test(&code, 0x1000, true).unwrap_or_else(|e| panic!("Failed {} (64-bit): {}", desc, e));

            // Test in 32-bit mode
            roundtrip_test(&code, 0x1000, false).unwrap_or_else(|e| panic!("Failed {} (32-bit): {}", desc, e));
        }
    }

    #[test]
    fn test_call_cache_behavior() {
        // Pattern with repeated calls to the same target
        let repeated_calls = vec![
            0xe8, 0x10, 0x00, 0x00, 0x00, // CALL +10h
            0x90, 0x90, // padding
            0xe8, 0x10, 0x00, 0x00, 0x00, // CALL +10h (same target)
            0x90, 0x90, // padding
            0xe8, 0x20, 0x00, 0x00, 0x00, // CALL +20h (different target)
            0xe8, 0x10, 0x00, 0x00, 0x00, // CALL +10h (first target again)
        ];

        roundtrip_test(&repeated_calls, 0x1000, true).unwrap_or_else(|e| panic!("Failed call cache test: {}", e));
    }

    // Test accuracy of individual functions
    #[test]
    fn test_shuffle_functions_edge_cases() {
        // VEX3 shuffle with boundary values
        assert_eq!(shuffle_vex3([0x00, 0x00]), Some(([0x80, 0x07], 0)));
        assert_eq!(shuffle_vex3([0x10, 0x00]), None); // bit 4 set should fail

        // VEX2 boundary test
        let (vex, map) = shuffle_vex2([0xff]);
        assert_eq!(unshuffle_vex2(vex, map), Some([0xff]));

        // EVEX with various combinations
        let test_evex = [0x07, 0xfb, 0xd7]; // max valid values
        let (evex_out, map) = shuffle_evex(test_evex);
        assert_eq!(unshuffle_evex(evex_out, map), Some(test_evex));
    }

    #[test]
    fn test_prefix_combinations() {
        // Multiple prefixes in different orders
        let test_cases = vec![
            // REP + OSIZE
            (vec![0xf3, 0x66, 0xa5], 0x1000, "REP MOVSW"),
            (vec![0x66, 0xf3, 0xa5], 0x1000, "REP MOVSW (reversed order)"),
            // Segment override + address size
            (vec![0x2e, 0x67, 0x8b, 0x00], 0x1000, "MOV EAX, CS:[EAX] (32-bit addr)"),
            (vec![0x67, 0x2e, 0x8b, 0x00], 0x1000, "MOV EAX, CS:[EAX] (reversed order)"),
            // Multiple segment overrides (last one wins)
            (vec![0x26, 0x2e, 0x8b, 0x00], 0x1000, "MOV EAX, CS:[EAX] (ES then CS)"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_grp3_immediate_variants() {
        // Test F6/F7 group 3 instructions with and without immediates
        let test_cases = vec![
            // TEST with immediate (reg field 0-1)
            (vec![0xf6, 0xc0, 0x55], 0x1000, "TEST AL, 55h"),
            (vec![0xf7, 0xc0, 0x55, 0x55, 0x55, 0x55], 0x1000, "TEST EAX, 55555555h"),
            // NOT without immediate (reg field 2)
            (vec![0xf6, 0xd0], 0x1000, "NOT AL"),
            (vec![0xf7, 0xd0], 0x1000, "NOT EAX"),
            // NEG without immediate (reg field 3)
            (vec![0xf6, 0xd8], 0x1000, "NEG AL"),
            (vec![0xf7, 0xd8], 0x1000, "NEG EAX"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_special_addressing_modes() {
        // Test various special addressing modes
        let test_cases = vec![
            // Direct addressing (32-bit only)
            (vec![0xa1, 0x00, 0x00, 0x40, 0x00], 0x1000, "MOV EAX, [400000h]"),
            // RIP-relative with large positive displacement
            (vec![0x8b, 0x05, 0x00, 0x00, 0x10, 0x00], 0x1000, "MOV EAX, [RIP+100000h]"),
            // RIP-relative with large negative displacement
            (vec![0x8b, 0x05, 0x00, 0x00, 0xf0, 0xff], 0x1000, "MOV EAX, [RIP-100000h]"),
            // Complex SIB: [base + index*scale + disp32]
            (vec![0x8b, 0x84, 0x88, 0x00, 0x10, 0x00, 0x00], 0x1000, "MOV EAX, [EAX+ECX*4+1000h]"),
        ];

        for (code, origin, desc) in test_cases {
            let is_64bit = !desc.contains("[400000h]"); // Direct addressing is 32-bit only
            roundtrip_test(&code, origin, is_64bit).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));

            // Also test in 32-bit mode where applicable
            if !desc.contains("RIP") {
                roundtrip_test(&code, origin, false).unwrap_or_else(|e| panic!("Failed {} (32-bit): {}", desc, e));
            }
        }
    }

    #[test]
    fn test_escape_sequences() {
        // Test instructions that get escaped (invalid opcodes)
        let test_cases = vec![
            // Omit VEX/EVEX tests as they have complex validation rules

            // Raw bytes that should be escaped
            (vec![0xf4], 0x1000, "HLT (should be escaped)"),
            // Note: F1 (INT1) might be used as JUMPTAB marker, so skip it
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_boundary_instructions() {
        // Instructions at the boundary of code sections
        let test_cases = vec![
            // CALL near end of section
            (vec![0xe8, 0xfb, 0xff, 0xff, 0xff], 0x1000, "CALL -5 (near boundary)"),
            // Jump table detection edge case
            (vec![0x00, 0x10, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00], 0x1000, "Potential jump table data"),
            // Mixed code and data
            (vec![0x90, 0x00, 0x00, 0x00, 0x00, 0x90], 0x1000, "NOP, data, NOP"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_rex2_prefix() {
        // Test REX2 prefix handling (if supported)
        let test_cases = vec![
            // Basic REX2
            (vec![0xd5, 0x00, 0x90], 0x1000, "REX2 NOP"),
            // REX2 with various payloads
            (vec![0xd5, 0x48, 0x89, 0xc0], 0x1000, "REX2.W MOV RAX, RAX"),
        ];

        for (code, origin, desc) in test_cases {
            roundtrip_test(&code, origin, true).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_stress_patterns() {
        // Stress test with complex real-world patterns
        let prologue_pattern = vec![
            0x55, // PUSH RBP
            0x48, 0x89, 0xe5, // MOV RBP, RSP
            0x48, 0x83, 0xec, 0x20, // SUB RSP, 20h
            0x48, 0x89, 0x4d, 0x10, // MOV [RBP+10h], RCX
            0x48, 0x8b, 0x45, 0x10, // MOV RAX, [RBP+10h]
            0x48, 0x83, 0xc4, 0x20, // ADD RSP, 20h
            0x5d, // POP RBP
            0xc3, // RET
        ];

        // Simple indirect jump instead of jump table
        let indirect_jump_pattern = vec![
            0xff, 0x24, 0x85, 0x00, 0x20, 0x40, 0x00, // JMP [RAX*4+402000h]
            0x90, 0x90, 0x90, // Some padding
        ];

        roundtrip_test(&prologue_pattern, 0x401000, true).unwrap_or_else(|e| panic!("Failed function prologue: {}", e));

        roundtrip_test(&indirect_jump_pattern, 0x401000, true)
            .unwrap_or_else(|e| panic!("Failed indirect jump pattern: {}", e));
    }

    #[test]
    fn test_random_roundtrip() {
        // xorshift64*, deterministic
        let mut state = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545f4914f6cdd1d)
        };

        // Bias towards bytes with special meanings to the encoder or decoder.
        const SPECIAL: &[u8] = &[
            0x00, 0x0f, 0x38, 0x3a, 0x40, 0x48, 0x4f, 0x62, 0x66, 0x67, 0xc4, 0xc5, 0xd5, 0xf1, 0xf4, 0xf2, 0xf3, 0xe8,
            0xe9, 0xc3, 0xcc, 0x90, 0xf6, 0xf7, 0x05, 0x04,
        ];

        for iter in 0..200_000 {
            let len = (next() % 48) as usize;
            let code: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    if r & 1 == 0 {
                        SPECIAL[(r >> 8) as usize % SPECIAL.len()]
                    } else {
                        (r >> 8) as u8
                    }
                })
                .collect();
            let origin = next() & 0xffff_ffff & ![0, 7][(iter & 1) as usize];
            for is64 in [false, true] {
                roundtrip_test(&code, origin, is64)
                    .unwrap_or_else(|e| panic!("Failed {code:02X?} @ {origin:#x} (is64={is64}): {e}"));
            }
        }
    }

    #[test]
    fn test_short_tail_jumptab_scan() {
        // Fewer bytes than a jump table entry left; zeros are valid 64-bit offsets,
        // so the scan must not run into the sentinel.
        roundtrip_test(&[0x00, 0x00, 0x00, 0x00], 0x1000, true).unwrap();
        roundtrip_test(&[0x00, 0x10], 0x1000, false).unwrap();
    }

    #[test]
    fn test_trailing_padding() {
        // RET followed by padding at the very end of the input.
        roundtrip_test(&[0xc3, 0x90, 0x90], 0x1000, true).unwrap();
        roundtrip_test(&[0xc3, 0xcc], 0x1000, false).unwrap();
    }

    #[test]
    fn test_escape_resets_prefixes() {
        // 66 + escaped byte; the following MOV r32,imm32 must not see the 66 prefix.
        let code = [0x66, 0xf4, 0xb8, 0x01, 0x02, 0x03, 0x04, 0x90];
        roundtrip_test(&code, 0x1000, true).unwrap();
        roundtrip_test(&code, 0x1000, false).unwrap();
        // REX followed by a 1-byte prefix is escaped as well.
        let code = [0x66, 0x48, 0x66, 0xb8, 0x01, 0x02, 0x03, 0x04, 0x90];
        roundtrip_test(&code, 0x1000, true).unwrap();
    }

    #[test]
    fn test_32bit_vex_lookalikes() {
        let test_cases = [
            (&[0xc4, 0x00][..], "LES EAX, [EAX]"),
            (&[0xc5, 0x00][..], "LDS EAX, [EAX]"),
            (&[0x62, 0x00][..], "BOUND EAX, [EAX]"),
            (&[0xc4, 0xf0, 0x00, 0x00][..], "C4 with mod=11 but VEX3 map bit 4 set"),
            (&[0xd5, 0x0a][..], "AAD 0Ah"),
        ];
        for (code, desc) in test_cases {
            roundtrip_test(code, 0x1000, false).unwrap_or_else(|e| panic!("Failed {}: {}", desc, e));
        }
    }

    #[test]
    fn test_jump_table_detection() {
        // Helper function for jump table tests
        let roundtrip_test = |code: &[u8], origin: u64, is64: bool| -> Result<(), String> {
            let input = code.to_vec();

            let encoded = encode(&input, origin, is64).ok_or("encoding failed")?;

            let decoded = decode(&(&encoded).into(), is64).ok_or("decoding failed")?;

            if input != decoded {
                let mismatch =
                    input.iter().zip(&decoded).position(|(a, b)| a != b).unwrap_or(input.len().min(decoded.len()));

                let input_end = (mismatch + 8).min(input.len());
                let decoded_end = (mismatch + 8).min(decoded.len());

                return Err(format!(
                    "Length mismatch or content mismatch at offset {}: \
                     input_len={}, decoded_len={}, input[{}..{}]={:02X?}, decoded[{}..{}]={:02X?}",
                    mismatch,
                    input.len(),
                    decoded.len(),
                    mismatch,
                    input_end,
                    &input[mismatch..input_end],
                    mismatch,
                    decoded_end,
                    &decoded[mismatch..decoded_end]
                ));
            }

            Ok(())
        };

        // Test case 1: Obvious jump table (should be detected)
        let obvious_jump_table_32 = vec![
            // 5 entries, all within code range
            0x00, 0x10, 0x00, 0x00, // 0x1000
            0x20, 0x10, 0x00, 0x00, // 0x1020
            0x40, 0x10, 0x00, 0x00, // 0x1040
            0x60, 0x10, 0x00, 0x00, // 0x1060
            0x80, 0x10, 0x00, 0x00, // 0x1080
        ];

        // Test case 2: Not a jump table (mixed valid/invalid addresses)
        let not_jump_table = vec![
            0x00, 0x10, 0x00, 0x00, // 0x1000 (valid)
            0x00, 0x00, 0x80, 0x00, // 0x800000 (out of range)
            0x40, 0x10, 0x00, 0x00, // 0x1040 (valid)
            0xFF, 0xFF, 0xFF, 0xFF, // -1 (invalid)
        ];

        // Test case 3: Barely enough entries (exactly MIN_JUMPTAB)
        let minimal_jump_table_32 = vec![
            0x00, 0x10, 0x00, 0x00, // 0x1000
            0x20, 0x10, 0x00, 0x00, // 0x1020
            0x40, 0x10, 0x00, 0x00, // 0x1040
        ];

        // Test case 4: 64-bit jump table (RIP-relative offsets)
        let jump_table_64 = vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 0 offset
            0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // +32 offset
            0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // +64 offset
            0xE0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // -32 offset
        ];

        // Test case 5: Data that looks like jump table but shouldn't be (wrong alignment)
        let misaligned_data = vec![
            0x90, // NOP to break alignment
            0x00, 0x10, 0x00, 0x00, // looks like address but misaligned
            0x20, 0x10, 0x00, 0x00, 0x40, 0x10, 0x00, 0x00,
        ];

        // Test case 6: Instruction followed by jump table data
        let instruction_then_data = vec![
            0xff, 0x24, 0x85, 0x00, 0x20, 0x40, 0x00, // JMP [RAX*4+402000h]
            // This should NOT be detected as jump table (too few entries, wrong context)
            0x00, 0x10, 0x40, 0x00, // 0x401000
            0x10, 0x10, 0x40, 0x00, // 0x401010
            0x20, 0x10, 0x40, 0x00, // 0x401020
        ];

        println!("Testing 32-bit obvious jump table...");
        roundtrip_test(&obvious_jump_table_32, 0x1000, false)
            .unwrap_or_else(|e| panic!("Failed 32-bit obvious jump table: {}", e));

        println!("Testing mixed data (should not be jump table)...");
        roundtrip_test(&not_jump_table, 0x1000, false).unwrap_or_else(|e| panic!("Failed mixed data test: {}", e));

        println!("Testing minimal jump table (exactly MIN_JUMPTAB entries)...");
        roundtrip_test(&minimal_jump_table_32, 0x1000, false)
            .unwrap_or_else(|e| panic!("Failed minimal jump table: {}", e));

        println!("Testing 64-bit jump table...");
        // Only test 64-bit jump table at 8-byte aligned address
        roundtrip_test(&jump_table_64, 0x1000, true).unwrap_or_else(|e| panic!("Failed 64-bit jump table: {}", e));

        println!("Testing misaligned data...");
        roundtrip_test(&misaligned_data, 0x1000, false).unwrap_or_else(|e| panic!("Failed misaligned data: {}", e));

        println!("Testing instruction followed by data...");
        roundtrip_test(&instruction_then_data, 0x401000, true)
            .unwrap_or_else(|e| panic!("Failed instruction+data pattern: {}", e));

        // Additional edge case: exactly at code boundaries
        let boundary_addresses = vec![
            0x00, 0x10, 0x00, 0x00, // start of range
            0xFF, 0x1F, 0x00, 0x00, // near end of range
            0x00, 0x10, 0x00, 0x00, // back to start
        ];

        println!("Testing boundary addresses...");
        roundtrip_test(&boundary_addresses, 0x1000, false)
            .unwrap_or_else(|e| panic!("Failed boundary addresses: {}", e));

        println!("All jump table tests passed!");
    }

    #[test]
    fn test_opcodes_resource() {
        let r = Opcodes::BUILTIN.to_resource();
        assert_eq!(r.len(), Opcodes::LEN);
        assert_eq!(Opcodes::from_resource(&r), Some(Opcodes::BUILTIN));
        // map 0 of x86-64 apart: PUSH ES is invalid there
        assert_eq!((r[0x06], r[4 * 256 + 0x06]), (N_, XX));
        // the markers are XX whatever the resource says
        let mut other = r.clone();
        other[0xf1] = N_;
        other[4 * 256 + 0xf4] = N_;
        assert_eq!(Opcodes::from_resource(&other), Some(Opcodes::BUILTIN));

        assert_eq!(Opcodes::from_resource(&r[1..]), None, "too short");
        let bad = |i: usize, v: u8| {
            let mut r = r.clone();
            r[i] = v;
            Opcodes::from_resource(&r)
        };
        assert_eq!(bad(0x90, 16), None, "not a class");
        assert_eq!(bad(0x90, BP), None, "a prefix class at no prefix");
        assert_eq!(bad(256 + 0x66, BP), None, "a prefix class out of map 0");
        assert!(bad(0x66, N_).is_some(), "a prefix taken as an opcode");

        // the decoder reads by the tables it is given: URDMSR (map 7 F8, ModR/M then imm32)
        // as an opcode of no operand leaves its ModR/M and immediate to be read as opcodes
        let code = [0x62, 0xf7, 0x7f, 0x08, 0xf8, 0xc0, 0x01, 0x02, 0x03, 0x04, 0x90];
        let st = encode(&code, 0x1000, true).unwrap();
        assert_eq!(decode(&(&st).into(), true).as_deref(), Some(&code[..]));
        let other = bad(3 * 256 + 0xf8, N_).unwrap();
        assert_ne!(decode_with(&(&st).into(), true, &other, 0).as_deref(), Some(&code[..]));
    }

    /// The index of the stream of this name (without `ST_`).
    fn stream_index(name: &str) -> usize {
        STREAM_NAMES.iter().position(|n| n.strip_prefix("ST_") == Some(name)).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn test_stream_layout() {
        let names: Vec<_> = STREAM_NAMES.iter().map(|n| n.strip_prefix("ST_").unwrap()).collect();
        assert_eq!(
            names,
            [
                "OP",
                "SIB",
                "CALL_IDX",
                "DISP8_SP",
                "DISP8_BP",
                "DISP8",
                "DISP8_IDX",
                "DISP32",
                "JUMP8",
                "JUMP",
                "IMM32",
                "ADDR",
                "CALL",
                "JUMPN",
            ]
        );
    }

    #[test]
    fn test_disp8_streams() {
        let code = [
            0x8b, 0x44, 0x24, 0x08, // MOV EAX, [RSP+8]
            0x8b, 0x44, 0x4c, 0x04, // MOV EAX, [RSP+RCX*2+4]
            0x8b, 0x45, 0xf8, // MOV EAX, [RBP-8]
            0x8b, 0x41, 0x10, // MOV EAX, [RCX+10h]
            0x41, 0x8b, 0x44, 0x24, 0x18, // MOV EAX, [R12+18h]
            0x41, 0x8b, 0x45, 0x28, // MOV EAX, [R13+28h]
            0x8b, 0x44, 0x91, 0x20, // MOV EAX, [RCX+RDX*4+20h]
            0x42, 0x8b, 0x44, 0x21, 0x30, // MOV EAX, [RCX+R12+30h]
        ];
        let st = encode(&code, 0x1000, true).unwrap();
        let of = |name| &st.streams()[stream_index(name)][..];
        assert_eq!(of("DISP8_SP"), [0x08, 0x04]);
        assert_eq!(of("DISP8_BP"), [0xf8]);
        assert_eq!(of("DISP8"), [0x10, 0x18, 0x28]);
        assert_eq!(of("DISP8_IDX"), [0x20, 0x30]);
        roundtrip_test(&code, 0x1000, true).unwrap();
        // x86-32, and 16-bit addressing (all in DISP8)
        let code = [0x8b, 0x44, 0x24, 0x08, 0x8b, 0x45, 0xf8, 0x67, 0x8b, 0x44, 0x04, 0x67, 0x8b, 0x46, 0x06];
        let st = encode(&code, 0x1000, false).unwrap();
        let of = |name| &st.streams()[stream_index(name)][..];
        assert_eq!((of("DISP8_SP"), of("DISP8_BP"), of("DISP8")), (&[0x08][..], &[0xf8][..], &[0x04, 0x06][..]));
        roundtrip_test(&code, 0x1000, false).unwrap();
    }

    #[test]
    fn test_padding_records() {
        // RET at 0x1000, filled with CC to 0x1010; RET, one CC; RET, two NOPs; RET, nothing
        let mut code = vec![0xc3];
        code.extend([0xcc; 15]);
        code.extend([0xc3, 0xcc, 0xc3, 0x90, 0x90, 0xc3, 0x31, 0xc0]);
        code.extend([0x90; 16]);
        let st = encode(&code, 0x1000, true).unwrap();
        let op = &st.streams()[stream_index("OP")];
        assert_eq!(op[..12], [0xc3, 1, 0xc3, 2, 0xc3, 3, 2, 0x90, 0x90, 0xc3, 0, 0x31]);
        roundtrip_test(&code, 0x1000, true).unwrap();

        // the fill of an aligned address is 16 bytes; a record of FF bytes in chunks
        let mut code = vec![0xc3];
        code.extend([0xcc; 16]);
        code.push(0xc3);
        code.extend([0x90; 300]);
        code.push(0x31);
        code.extend([0xc0; 16]);
        let st = encode(&code, 0x0fff, true).unwrap();
        assert_eq!(st.streams()[stream_index("OP")][..5], [0xc3, 1, 0xc3, 3, 0xff]);
        roundtrip_test(&code, 0x0fff, true).unwrap();
        roundtrip_test(&code, 0x1000, false).unwrap();

        let streams = |op: &'static [u8]| {
            DecodedStreams::new(0x1000, 0, array::from_fn(|i| if i == stream_index("OP") { op } else { &[][..] }))
        };
        let out = decode(&streams(&[0xc3, 1, 0xc3, 2, 0xc3, 3, 0xff]), true);
        assert_eq!(out, None, "a record running out");
        assert_eq!(decode(&streams(&[0xc3, 4]), true), None, "no such record");
        let out = decode(&streams(&[0xc3, 1, 0xc3, 2, 0xc3, 3, 2, 0x66, 0x90, 0xc3, 0]), true).unwrap();
        let mut expected = vec![0xc3];
        expected.extend([0xcc; 15]);
        expected.extend([0xc3, 0xcc, 0xc3, 0x66, 0x90, 0xc3]);
        assert_eq!(out, expected);
    }
    /// x86-32 code of jumps of 8 and 32 bits, forward and backward, to steps and not, and the
    /// counts the encoder makes of them (§5.6 of FORMAT.md).
    fn jumps_code() -> Vec<u8> {
        let mut c = vec![0x90]; // 0 nop (step 0)
        c.extend([0x74, 0x03]); // 1 jz 6 (step 1): count 3
        c.push(0x90); // 3 nop (2)
        c.extend([0x66, 0x90]); // 4 the prefix (3), 5 nop (4)
        c.extend([0x31, 0xc0]); // 6 xor eax, eax (5)
        c.extend([0x75, 0xf6]); // 8 jnz 0 (6): count -7
        c.extend([0x0f, 0x84, 0x07, 0, 0, 0]); // 10 jz 23 (7): count 2
        c.extend([0xeb, 0x01]); // 16 jmp 19 (8), into the next instruction: escaped
        c.extend([0xb8, 0xc3, 0xc3, 0xc3, 0xc3]); // 18 mov eax, imm32 (9), after no padding
        c.push(0xe9); // 23 jmp 1 (10): count -10
        c.extend((1i32 - 28).to_le_bytes());
        c.push(0xe9); // 28 jmp 33 + 0x2000 (11), out of reach: escaped
        c.extend(0x2000i32.to_le_bytes());
        c.extend([0x31, 0xc0, 0x90]); // 33 (12), 35 (13)
        c
    }

    #[test]
    fn test_jump_counts() {
        let code = jumps_code();
        let st = encode(&code, 0x1000, false).unwrap();
        assert_eq!(st.streams()[ST_JUMP8], [3, 0xf9, 0x80, 0x01]);
        assert_eq!(st.streams()[ST_JUMPN], [2, 0xf6, 0x80]);
        assert_eq!(st.streams()[ST_JUMP], (0x1000u32 + 33 + 0x2000).to_le_bytes());
        roundtrip_test(&code, 0x1000, false).unwrap();
        roundtrip_test(&code, 0x1000, true).unwrap();

        // the hook told of the jumps escaped alone, where their fields are; the decoder alike
        let mut told = Vec::new();
        let again = encode_with(&code, 0x1000, false, &mut |r, at, v| {
            told.push((r, at, v));
            v
        });
        assert_eq!(again.unwrap().streams(), st.streams());
        assert_eq!(told, [(Ref::Jump, 0x1000 + 29, 0x1000 + 33 + 0x2000)]);
        let mut told = Vec::new();
        let back = decode_hooked(&(&st).into(), false, &Opcodes::BUILTIN, 0, |r, v, at| {
            told.push((r, at, v));
            Some(())
        });
        assert_eq!(back.as_deref(), Some(&code[..]));
        assert_eq!(told, [(Ref::Jump, 29, 0x1000 + 33 + 0x2000)]);

        // escaped where waiting at the end: a jump past it, and one to its end
        for tail in [&[0xeb, 0x05][..], &[0xeb, 0x00], &[0x0f, 0x85, 0, 0, 0, 0]] {
            let mut c = code.clone();
            c.extend(tail);
            let st = encode(&c, 0x1000, false).unwrap();
            let s = if tail[0] == 0xeb { ST_JUMP8 } else { ST_JUMPN };
            assert_eq!(st.streams()[s][st.streams()[s].len() - 1 - (s == ST_JUMP8) as usize], 0x80, "{tail:02x?}");
            roundtrip_test(&c, 0x1000, false).unwrap();
        }

        // jumps past 127 steps either way, and of 8 bits over more than 127 bytes of fewer steps
        let mut c = vec![0x0f, 0x84];
        c.extend((2 + 130 * 5i32).to_le_bytes());
        c.extend([0x74, 0x7f]);
        for _ in 0..130 {
            c.extend([0xb8, 1, 2, 3, 4]);
        }
        c.push(0xe9);
        c.extend((-(c.len() as i32 + 4)).to_le_bytes());
        let st = encode(&c, 0x1000, false).unwrap();
        assert_eq!(st.streams()[ST_JUMP8], [0x80, 0x7f]);
        assert_eq!(st.streams()[ST_JUMPN], [0x80, 0x80]);
        roundtrip_test(&c, 0x1000, false).unwrap();
    }

    /// A jump over a run of jump table entries longer than a table (256): each table is a step
    /// of its own, as the decoder reads them.
    #[test]
    fn test_jump_counts_over_tables() {
        for n in [255usize, 256, 257, 600] {
            let origin = 0x40_1000u64;
            let mut c = vec![0xe9];
            c.extend(((4 * n) as i32).to_le_bytes());
            for k in 0..n {
                c.extend((origin as u32 + 0x111 + k as u32).to_le_bytes());
            }
            c.extend([0x31, 0xc0, 0xc3]);
            let st = encode(&c, origin, false).unwrap();
            assert_eq!(st.streams()[ST_JUMPN], [n.div_ceil(256) as u8], "{n} entries");
            roundtrip_test(&c, origin, false).unwrap_or_else(|e| panic!("{n} entries: {e}"));
        }
    }

    #[test]
    fn test_jump_counts_corrupt() {
        let code = jumps_code();
        let st = encode(&code, 0x1000, false).unwrap();
        let with = |s: usize, f: &dyn Fn(&mut Vec<u8>)| {
            let mut v: [Vec<u8>; NUM_STREAMS] = st.streams().clone();
            f(&mut v[s]);
            let d = DecodedStreams::new(0x1000, st.sentinel_size(), v.each_ref().map(|s| &s[..]));
            decode(&d, false)
        };
        assert_eq!(with(ST_JUMP8, &|_| ()).as_deref(), Some(&code[..]));
        // a target past the end, or before the start
        assert_eq!(with(ST_JUMP8, &|s| s[0] = 0x7f), None);
        assert_eq!(with(ST_JUMPN, &|s| s[0] = 0x7f), None);
        assert_eq!(with(ST_JUMP8, &|s| s[1] = 0x81), None);
        assert_eq!(with(ST_JUMPN, &|s| s[1] = 0x81), None);
        // other steps decode, to other code
        let other = with(ST_JUMP8, &|s| s[0] = 4).unwrap();
        assert_eq!(other.len(), code.len());
        assert_eq!(other[2], 0x05);
        // a count where the stream has none left
        assert_eq!(with(ST_JUMPN, &|s| s.truncate(2)), None);

        // of 8 bits, more than 127 bytes ahead or 128 behind
        let mut c = vec![0x74, 0x00];
        for _ in 0..30 {
            c.extend([0xb8, 1, 2, 3, 4]);
        }
        c.extend([0xeb, 0xfe]);
        let st = encode(&c, 0x1000, false).unwrap();
        assert_eq!(st.streams()[ST_JUMP8], [0, 0xff]);
        let run = |j8: &[u8]| {
            let mut v: [&[u8]; NUM_STREAMS] = st.streams().each_ref().map(|s| &s[..]);
            v[ST_JUMP8] = j8;
            decode(&DecodedStreams::new(0x1000, st.sentinel_size(), v), false)
        };
        assert_eq!(run(&[0, 0xff]).as_deref(), Some(&c[..]));
        assert_eq!(run(&[25, 0xff]).map(|d| d[1]), Some(125));
        assert_eq!(run(&[26, 0xff]), None);
        assert_eq!(run(&[0, 0xe6]).map(|d| d[d.len() - 1]), Some(0x81));
        assert_eq!(run(&[0, 0xe5]), None);
    }
}
