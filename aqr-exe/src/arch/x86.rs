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

use bytemuck::must_cast_slice;

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

#[inline(always)]
fn lookup_opcode(op: u8, map: u8, is64: bool) -> u8 {
    const OPCODES: [u16; 256] = {
        let mut tab = [0u16; 256];
        let mut i = 0;
        while i < 256 {
            assert!(OPCODES0[i] | OPCODES1[i] | OPCODES4[i] | OPCODES7[i] < 16);
            tab[i] =
                OPCODES0[i] as u16 | (OPCODES1[i] as u16) << 4 | (OPCODES4[i] as u16) << 8 | (OPCODES7[i] as u16) << 12;
            i += 1;
        }

        // Override escape codes.
        assert!(OPCODES0[ESC as usize] == N_);
        assert!(OPCODES0[JUMPTAB as usize] == N_);
        tab[ESC as usize] = tab[ESC as usize] & !0xf | XX as u16;
        tab[JUMPTAB as usize] = tab[JUMPTAB as usize] & !0xf | XX as u16;

        tab
    };

    const MAP_FLAGS: [u8; 16] = [0x20, 0x24, R_, R1, 0x28, R_, R_, 0x2c, XX, XX, XX, XX, XX, XX, XX, XX];

    if map == 0 && is64 && OPCODES0_I64[op as usize >> 5] >> (op as u32 & 31) & 1 == 1 {
        XX
    } else {
        let map_flags = MAP_FLAGS[map as usize];
        if map_flags < 0x20 {
            map_flags
        } else {
            (OPCODES[op as usize] >> (map_flags & 0x1f)) as u8 & 15
        }
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

const _: () = {
    let prefixes = [0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, 0x66, 0x67, 0xf0, 0xf2, 0xf3];
    let mut bitset = 0;
    let mut i = 0;
    while i < prefixes.len() {
        let b = prefixes[i];
        bitset |= 1u32 << prefix_hash(b);
        assert!(OPCODES0[b as usize] == BP);
        i += 1;
    }
    assert!(bitset.count_ones() == 11);
};

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
}

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
}

#[cfg(feature = "encode")]
impl EncodedStreams {
    fn new(origin: u64) -> Self {
        Self { origin, sentinel_size: 0, streams: array::from_fn(|_| Vec::new()) }
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
        w.write_all(must_cast_slice(&header))?;

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
    // TODO
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

#[cfg(feature = "encode")]
#[allow(clippy::eq_op)] // `copy(8 / 8, ...)` spells out the bit width
pub fn encode(input: &[u8], mut origin: u64, is64: bool) -> Option<EncodedStreams> {
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

    // Bytes an instruction may read past its start: should be enough for this encoding scheme.
    const SENTINEL: usize = 15;

    let mut prefixes = 0;
    let mut pad = false;

    // Encodes `code` up to its last SENTINEL bytes, the last `slack` of which are zeros after
    // the code; returns the bytes left.
    let mut run = |mut code: &[u8], slack: usize| -> usize {
        while code.len() > SENTINEL {
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

                if false {
                    // Speculatively record the next instruction as a call target.
                    call_idx(origin);
                }

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
                        st.jumptab((range.len() - 1) as u8);
                        for j in range {
                            let addr = to_u32(&code[j * 4..j * 4 + 4]);
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

            let rel_to_abs =
                |addr: u32, delta: usize| (origin + (i.get() + delta) as u64).wrapping_add(addr as i32 as u64);

            let mut flags = lookup_opcode(op, map, is64);

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
                        let addr = fetch32();
                        if is64 {
                            // [eip+disp32] or [rip+disp32]
                            // Note that we haven't fully decoded operands yet, hence a delta.
                            let delta = modrm_imm_size(flags, prefixes);
                            st.put64(ST_ADDR64, rel_to_abs(addr, delta));
                        } else {
                            st.put32(ST_ADDR32, addr); // [disp32]
                        }
                    }
                    0 if sib & 7 == 5 => copy(32 / 8, &mut st, ST_DISP32), // [reg*scale+disp32]
                    // [reg+disp8] or [reg*scale+disp8]
                    1 => copy(8 / 8, &mut st, disp8_stream(base, sib, rex, addr16)),
                    2 if addr16 => copy(16 / 8, &mut st, ST_DISP16), // [regs+disp16]
                    2 => copy(32 / 8, &mut st, ST_DISP32),           // [reg+disp32]
                    _ => {}
                }
            }

            match flags {
                J4 if has_jump16(prefixes, is64) => copy(16 / 8, &mut st, ST_JUMP16),
                J4 => {
                    let target = rel_to_abs(fetch32(), 0);
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
                    copy(1 << lgn, &mut st, addr_stream(lgn));
                }

                JA if has_jump16(prefixes, is64) => copy(16 / 8, &mut st, ST_AJUMP16),
                JA => copy(32 / 8, &mut st, ST_AJUMP32),
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
                    copy(1 << lgn, &mut st, imm_stream(lgn));
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
#[allow(clippy::eq_op)] // `copy(8 / 8, ...)` spells out the bit width
pub fn decode(streams: &DecodedStreams<'_>, is64: bool) -> Option<Vec<u8>> {
    let origin = streams.origin;
    let sentinel_size = streams.sentinel_size;
    let streams = streams.streams.each_ref().map(|stream| Cell::new(&stream[..]));

    let pc = Cell::new(origin);
    let mut code = Vec::new();

    let read8 = |st: usize| {
        if_trace_decode! {
            print!("({}:", &STREAM_NAMES[st][3..]);
        }
        let (&[head], tail) = streams[st].get().split_first_chunk::<1>()?;
        if_trace_decode! {
            print!("{head:02X})");
        }
        streams[st].set(tail);
        Some(head)
    };

    let read32 = |st: usize| {
        if_trace_decode! {
            print!("({}:", &STREAM_NAMES[st][3..]);
        }
        let (&head, tail) = streams[st].get().split_first_chunk::<4>()?;
        let head = u32::from_le_bytes(head);
        if_trace_decode! {
            print!("{head:08X})");
        }
        streams[st].set(tail);
        Some(head)
    };

    let read64 = |st: usize| {
        if_trace_decode! {
            print!("({}:", &STREAM_NAMES[st][3..]);
        }
        let (&head, tail) = streams[st].get().split_first_chunk::<8>()?;
        let head = u64::from_le_bytes(head);
        if_trace_decode! {
            print!("{head:016X})");
        }
        streams[st].set(tail);
        Some(head)
    };

    let mut call_cache = CallCache::new();

    let read_call = |cache: &mut CallCache| {
        let idx = read8(ST_CALL_IDX)?;
        let target = if let Some(target) = cache.find_target(idx) {
            target
        } else if is64 {
            read64(ST_CALL64)?
        } else {
            read32(ST_CALL32)? as u64
        };
        cache.record(idx, target);
        Some(target)
    };

    let copy = |n: usize, code: &mut Vec<u8>, st: usize| {
        if_trace_decode! {
            print!("({}:", &STREAM_NAMES[st][3..]);
        }
        let (head, tail) = streams[st].get().split_at_checked(n)?;
        if_trace_decode! {
            for &b in head {
                print!("{:02X}", b);
            }
            print!(")");
        }
        code.extend_from_slice(head);
        streams[st].set(tail);
        Some(())
    };

    macro_rules! fatal {
        ($fmt:tt) => {
            panic!(concat!($fmt, " @ {:06X} {:02X?}"), pc.get(), &code[code.len().max(15) - 15..])
        };
    }

    let mut prefixes = 0;
    let mut pad = false;

    while pad || !streams[ST_OP].get().is_empty() {
        pc.set(origin + code.len() as u64);
        if_trace_decode! {
            println!();
            print!("{:06X}: ", pc.get());
        }

        if pad {
            match read8(ST_PAD)? {
                PAD_NONE => {}
                PAD_FILL => code.resize(code.len() + pad_fill_len(pc.get()), 0xcc),
                PAD_INT3 => code.push(0xcc),
                PAD_BYTES => loop {
                    let pad_size = read8(ST_PAD)?;
                    copy(pad_size as usize, &mut code, ST_PAD)?;
                    if pad_size < 0xff {
                        break;
                    }
                },
                _ => return None,
            }

            if false {
                // Speculatively record the next instruction as a call target.
                let target = origin + code.len() as u64;
                let idx = call_cache.find_index(target);
                call_cache.record(idx, target);
            }

            pad = false;
            continue;
        }

        let mut op = read8(ST_OP).unwrap();

        if op == ESC {
            // The encoder discards any pending prefixes when escaping.
            prefixes = 0;
            code.push(read8(ST_OP)?);
            continue;
        }

        if op == JUMPTAB {
            let count = read8(ST_JUMPTAB_COUNT)? as usize + 1;
            if is64 {
                for _ in 0..count {
                    code.extend_from_slice(&read64(ST_JUMPTAB64)?.to_le_bytes());
                }
            } else {
                for _ in 0..count {
                    code.extend_from_slice(&(read_call(&mut call_cache)? as u32).to_le_bytes());
                }
            }
            continue;
        }

        let (pre, evex, vex, rex) = match op {
            PRE_VEX3 => (op, 0, read8(ST_VEX)?, read8(ST_REX)?),
            PRE_VEX2 => (op, 0, read8(ST_VEX)?, read8(ST_REX)?),
            PRE_EVEX => (op, read8(ST_EVEX)?, read8(ST_VEX)?, read8(ST_REX)?),
            REX_MARKER | PRE_REX2 if !is64 => fatal!("unsupported REX prefixes in x86-32"),
            REX_MARKER => (op, 0, 0, read8(ST_REX)?),
            PRE_REX2 => (op, 0, 0, read8(ST_REX)?),
            _ => (0, 0, 0, 0),
        };
        if pre != 0 {
            op = read8(ST_OP)?;
        }

        let map;
        if op & 0xf0 == 0x40 {
            map = op & 0x0f;
            op = read8(ST_OP)?;
        } else {
            map = 0;
        }

        let mut flags = lookup_opcode(op, map, is64);
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
                    let Some([x, y]) = unshuffle_vex3([vex, rex], map) else {
                        fatal!("bad VEX3 prefix");
                    };
                    code.extend_from_slice(&[PRE_VEX3, x, y, op]);
                    break 'prefix;
                }
                PRE_VEX2 => {
                    let Some([x]) = unshuffle_vex2([vex, rex], map) else {
                        fatal!("bad VEX2 prefix");
                    };
                    code.extend_from_slice(&[PRE_VEX2, x, op]);
                    break 'prefix;
                }
                PRE_EVEX => {
                    let Some([x, y, z]) = unshuffle_evex([evex, vex, rex], map) else {
                        fatal!("bad EVEX prefix");
                    };
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
                copy(16 / 8, &mut code, ST_IMM16)?;
            }

            // F6/F7: TEST E,I (/0-1) vs. NOT/NEG/[I]MUL/[I]DIV E (/2-7)
            OP_GRP3_1 | OP_GRP3_2 | OP_MAP4_GRP3_1 | OP_MAP4_GRP3_2
                if modrm_reg(*streams[ST_MODRM].get().first()?) >= 2 =>
            {
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

        let abs_to_rel = |addr: u64, code: &[u8], delta: usize| addr.wrapping_sub(origin + (code.len() + delta) as u64);

        // ModR/M present
        if has_modrm(flags) {
            flags = modrm_to_imm(flags);

            let modrm = read8(ST_MODRM)?;
            code.push(modrm);

            let (mode, base) = parse_modrm(modrm);
            let addr16 = has_addr16(prefixes, is64);
            let sib;
            if !addr16 && modrm_has_sib((mode, base)) {
                sib = read8(ST_SIB)?;
                code.push(sib);
            } else {
                sib = 0;
            }

            match mode {
                0 if addr16 && base == 6 => copy(16 / 8, &mut code, ST_ADDR16)?, // [disp16]
                0 if addr16 => {}
                0 if base == 5 => {
                    let addr = if is64 {
                        // [eip+disp32] or [rip+disp32]
                        // Note that we haven't fully decoded operands yet, hence a delta.
                        let delta = modrm_imm_size(flags, prefixes);
                        abs_to_rel(read64(ST_ADDR64)?, &code, delta + 4) as u32
                    } else {
                        read32(ST_ADDR32)? // [disp32]
                    };
                    code.extend_from_slice(&addr.to_le_bytes());
                }
                // [reg*scale+disp32]
                0 if sib & 7 == 5 => copy(32 / 8, &mut code, ST_DISP32)?,
                // [reg+disp8] or [reg*scale+disp8]
                1 => copy(8 / 8, &mut code, disp8_stream(base, sib, rex, addr16))?,
                2 if addr16 => copy(16 / 8, &mut code, ST_DISP16)?, // [regs+disp16]
                2 => copy(32 / 8, &mut code, ST_DISP32)?,           // [reg+disp32]
                _ => {}
            }
        }

        match flags {
            J4 if has_jump16(prefixes, is64) => copy(16 / 8, &mut code, ST_JUMP16)?,
            J4 => {
                let target = if op == OP_CALLN {
                    read_call(&mut call_cache)?
                } else if is64 {
                    read64(ST_JUMP64)?
                } else {
                    read32(ST_JUMP32)? as u64
                };
                let target = abs_to_rel(target, &code, 4) as u32;
                code.extend_from_slice(&target.to_le_bytes());
            }

            A_ => {
                let lgn = addr_lgn(op, prefixes, is64);
                copy(1 << lgn, &mut code, addr_stream(lgn))?;
            }

            JA if has_jump16(prefixes, is64) => copy(16 / 8, &mut code, ST_AJUMP16)?,
            JA => copy(32 / 8, &mut code, ST_AJUMP32)?,
            J1 => copy(8 / 8, &mut code, ST_JUMP8)?,
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
                copy(1 << lgn, &mut code, imm_stream(lgn))?;
            }
        }
    }

    code.truncate(code.len().checked_sub(sentinel_size as usize)?);
    Some(code)
}

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
}
