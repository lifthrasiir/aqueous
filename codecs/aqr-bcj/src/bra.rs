//! The branch converters of xz and 7z (BCJ): calls and jumps of machine code whose relative
//! targets are made absolute (encoding) or relative again (decoding), so that calls of a function
//! are alike. Each converts what it finds in place, from the start of the data, as one call of
//! the LZMA SDK's converter on all of it would (its `Bra.c`, `Bra86.c`, `BraIA64.c`; public
//! domain, which this code is ported from): `pc` is the address of the first byte.

/// The architectures, by the code they convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86,
    PowerPc,
    Ia64,
    Arm,
    ArmThumb,
    Sparc,
    Arm64,
    RiscV,
}

impl Arch {
    pub const ALL: [Arch; 8] =
        [Arch::X86, Arch::PowerPc, Arch::Ia64, Arch::Arm, Arch::ArmThumb, Arch::Sparc, Arch::Arm64, Arch::RiscV];

    /// The alignment a start address must have (a mask of the bits that must be 0).
    pub fn alignment(self) -> u32 {
        match self {
            Arch::X86 => 0,
            Arch::ArmThumb | Arch::RiscV => 1,
            Arch::Ia64 => 15,
            _ => 3,
        }
    }
}

fn le16(d: &[u8], i: usize) -> u32 {
    u16::from_le_bytes([d[i], d[i + 1]]) as u32
}

fn le32(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[i..i + 4].try_into().unwrap())
}

fn be32(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(d[i..i + 4].try_into().unwrap())
}

fn set_le16(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn set_le32(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

fn set_be32(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 4].copy_from_slice(&v.to_be_bytes());
}

/// `v + c` encoding, `v − c` decoding.
#[inline]
fn conv(v: u32, c: u32, enc: bool) -> u32 {
    if enc { v.wrapping_add(c) } else { v.wrapping_sub(c) }
}

/// Converts `d` of `arch` (encoding if `enc`), its first byte at `pc`.
pub fn convert(arch: Arch, d: &mut [u8], pc: u32, enc: bool) {
    match arch {
        Arch::X86 => x86(d, pc, enc),
        Arch::PowerPc => ppc(d, pc, enc),
        Arch::Ia64 => ia64(d, pc, enc),
        Arch::Arm => arm(d, pc, enc),
        Arch::ArmThumb => armt(d, pc, enc),
        Arch::Sparc => sparc(d, pc, enc),
        Arch::Arm64 => arm64(d, pc, enc),
        Arch::RiscV if enc => riscv_enc(d, pc),
        Arch::RiscV => riscv_dec(d, pc),
    }
}

/// Whether `b` is 0x00 or 0xFF, the high byte of a near target.
#[inline]
fn ms_byte(b: u8) -> bool {
    b.wrapping_add(1) & 0xfe == 0
}

/// x86: E8 (call) and E9 (jmp) of a 32-bit offset whose high byte is 0x00 or 0xFF. A mask of
/// the E8/E9 bytes among the last three before tells which candidates are skipped, so that an
/// instruction's bytes are not taken for another's.
fn x86(d: &mut [u8], pc: u32, enc: bool) {
    if d.len() < 5 {
        return;
    }
    let limit = d.len() - 4;
    let ip = pc.wrapping_add(5);
    let mut mask = 0u32;
    let mut pos = 0usize;
    loop {
        let mut p = pos;
        while p < limit && d[p] & 0xfe != 0xe8 {
            p += 1;
        }
        let dist = p - pos;
        pos = p;
        if p >= limit {
            return;
        }
        if dist > 2 {
            mask = 0;
        } else {
            mask >>= dist;
            if mask != 0 && (mask > 4 || mask == 3 || ms_byte(d[p + (mask as usize >> 1) + 1])) {
                mask = (mask >> 1) | 4;
                pos += 1;
                continue;
            }
        }
        if ms_byte(d[p + 4]) {
            let mut v = le32(d, p + 1);
            let cur = ip.wrapping_add(pos as u32);
            pos += 5;
            v = conv(v, cur, enc);
            if mask != 0 {
                let sh = (mask & 6) << 2;
                if ms_byte((v >> sh) as u8) {
                    v ^= (0x100u32 << sh).wrapping_sub(1);
                    v = conv(v, cur, enc);
                }
                mask = 0;
            }
            d[p + 1] = v as u8;
            d[p + 2] = (v >> 8) as u8;
            d[p + 3] = (v >> 16) as u8;
            d[p + 4] = 0u8.wrapping_sub(((v >> 24) & 1) as u8);
        } else {
            mask = (mask >> 1) | 4;
            pos += 1;
        }
    }
}

/// PowerPC: `bl` (big-endian, opcode 18 with AA = 0 and LK = 1).
fn ppc(d: &mut [u8], pc: u32, enc: bool) {
    for i in (0..d.len() & !3).step_by(4) {
        let v = be32(d, i);
        if v & 0xfc00_0003 == 0x4800_0001 {
            let v = conv(v, pc.wrapping_add(i as u32), enc);
            set_be32(d, i, (v & 0x03ff_ffff) | 0x4800_0000);
        }
    }
}

/// ARM: `bl` (little-endian, condition "always").
fn arm(d: &mut [u8], pc: u32, enc: bool) {
    for i in (0..d.len() & !3).step_by(4) {
        if d[i + 3] == 0xeb {
            let c = pc.wrapping_add(i as u32).wrapping_add(8) >> 2;
            let v = conv(le32(d, i), c, enc);
            set_le32(d, i, (v & 0x00ff_ffff) | 0xeb00_0000);
        }
    }
}

/// ARM Thumb: the pair of halfwords of `bl`.
fn armt(d: &mut [u8], pc: u32, enc: bool) {
    let n = d.len() & !1;
    let mut i = 0;
    while i + 4 <= n {
        if d[i + 1] & 0xf8 == 0xf0 && d[i + 3] & 0xf8 == 0xf8 {
            let v = (le16(d, i) << 11) | (le16(d, i + 2) & 0x7ff);
            let c = pc.wrapping_add(i as u32).wrapping_add(4) >> 1;
            let v = conv(v, c, enc);
            set_le16(d, i, ((v >> 11) & 0x7ff) | 0xf000);
            set_le16(d, i + 2, v | 0xf800);
            i += 4;
        } else {
            i += 2;
        }
    }
}

/// SPARC: `call` (big-endian) of a target within ±2^24 bytes.
fn sparc(d: &mut [u8], pc: u32, enc: bool) {
    const FLAG: u32 = 1 << 22;
    for i in (0..d.len() & !3).step_by(4) {
        let mut v = be32(d, i);
        v = v.wrapping_add(5 << 29);
        v ^= 7 << 29;
        v = v.wrapping_add(FLAG);
        if v & 0u32.wrapping_sub(FLAG << 1) != 0 {
            continue;
        }
        v <<= 2;
        v = conv(v, pc.wrapping_add(i as u32), enc);
        v &= (FLAG << 3) - 1;
        v = v.wrapping_sub(FLAG << 2);
        v >>= 2;
        v |= 1 << 30;
        set_be32(d, i, v);
    }
}

/// ARM64: `bl`, and `adrp` of a target within ±2^32 bytes.
fn arm64(d: &mut [u8], pc: u32, enc: bool) {
    const FLAG: u32 = 1 << 20;
    const MASK: u32 = (1 << 24) - (FLAG << 1);
    for i in (0..d.len() & !3).step_by(4) {
        let pc = pc.wrapping_add(i as u32);
        let v = le32(d, i);
        if v.wrapping_sub(0x9400_0000) & 0xfc00_0000 == 0 {
            let v = conv(v, pc >> 2, enc);
            set_le32(d, i, (v & 0x03ff_ffff) | 0x9400_0000);
            continue;
        }
        let mut v = v.wrapping_sub(0x9000_0000);
        if v & 0x9f00_0000 != 0 {
            continue;
        }
        v = v.wrapping_add(FLAG);
        if v & MASK != 0 {
            continue;
        }
        let mut z = (v & 0xffff_ffe0) | (v >> 26);
        z = conv(z, (pc >> (12 - 3)) & !7, enc);
        let mut w = v & 0x1f;
        w |= 0x9000_0000;
        w |= z << 26;
        w |= 0x00ff_ffe0 & (z & ((FLAG << 1) - 1)).wrapping_sub(FLAG);
        set_le32(d, i, w);
    }
}

/// IA-64: the `br.call` slots of bundles of the templates that have branch slots.
fn ia64(d: &mut [u8], pc: u32, enc: bool) {
    let n = d.len() & !15;
    let mut pc = pc.wrapping_sub(1 << 4) >> (4 - 1);
    let mut b = 0;
    while b < n {
        let mut m = (0x334b_0000u32 >> (d[b] & 0x1e)) & 3;
        pc = pc.wrapping_add(1 << 1);
        if m != 0 {
            // (the slots from the first that may hold a branch; slot m's 41 bits at 5 + 41 m)
            let mut p = b + 5 * m as usize - 4;
            loop {
                let t = le16(d, p);
                let mut z = le32(d, p + 1) >> m;
                if (t >> m) & (0x70 << 1) == 0 && z.wrapping_sub(0x500_0000 << 1) & (0xf00_0000 << 1) == 0 {
                    let mut v = ((0x8f_ffff << 1) | 1) & z;
                    z ^= v;
                    if enc {
                        pc &= (0x1f_ffff << 1) | 1;
                        v = v.wrapping_add(pc);
                    } else {
                        pc |= !((0x1f_ffffu32 << 1) | 1);
                        v = v.wrapping_sub(pc);
                    }
                    v &= !(0x60_0000u32 << 1);
                    v = v.wrapping_add(0x70_0000 << 1);
                    v &= (0x8f_ffff << 1) | 1;
                    z |= v;
                    z <<= m;
                    set_le32(d, p + 1, z);
                }
                p += 5;
                m += 1;
                if m & 3 == 0 {
                    break;
                }
            }
        }
        b += 16;
    }
}

/// RISC-V: where a scan of halfwords (from `p`, below `lim`) finds the next JAL or AUIPC, its
/// low halfword plus one, xor 0x10 (`a`), or None at the end.
fn riscv_scan(d: &[u8], p: &mut usize, lim: usize) -> Option<u32> {
    loop {
        if *p >= lim {
            return None;
        }
        let a = (le16(d, *p) ^ 0x10) + 1;
        if a & 0x77 == 0 {
            return Some(a);
        }
        let a = (le16(d, *p + 2) ^ 0x10) + 1;
        *p += 4;
        if a & 0x77 == 0 {
            *p -= 2;
            if *p >= lim {
                return None;
            }
            return Some(a);
        }
    }
}

/// `(((b − 3) ^ (v << 8)) & 0xf8003) == 0`: the instruction after an AUIPC uses its register.
#[inline]
fn riscv_check1(v: u32, b: u32) -> bool {
    (b.wrapping_sub(3) ^ (v << 8)) & (0xf8000 + 3) == 0
}

#[inline]
fn riscv_check2(v: u32, r: u32) -> bool {
    (v.wrapping_sub((3 << 12) | (2 << 7) | 8) << 18) < (r & 0x1d)
}

/// RISC-V encoding: JAL, and AUIPC with the instruction after it.
fn riscv_enc(d: &mut [u8], pc: u32) {
    let size = d.len() & !1;
    if size <= 6 {
        return;
    }
    let lim = size - 6;
    let mut p = 0;
    while let Some(v) = riscv_scan(d, &mut p, lim) {
        let mut a = le32(d, p);
        let get = |p: usize| pc.wrapping_add(p as u32);
        if v & 8 == 0 {
            // JAL
            if v.wrapping_sub(0x100) & 0xd80 != 0 {
                p += 2;
                continue;
            }
            let mut v =
                ((a & (1 << 31)) >> 11) | ((a & (0x3ff << 21)) >> 20) | ((a & (1 << 20)) >> 9) | (a & (0xff << 12));
            v = v.wrapping_add(get(p));
            d[p + 1] = (((v >> 13) & 0xf0) | ((a >> 8) & 0xf)) as u8;
            d[p + 2] = (v >> 9) as u8;
            d[p + 3] = (v >> 1) as u8;
            p += 4;
            continue;
        }
        // AUIPC
        if v & 0xe80 != 0 {
            let b = le32(d, p + 4);
            if riscv_check1(v, b) {
                set_le32(d, p, (b << 12) | (0x17 + (2 << 7)));
                a &= 0xffff_f000;
                a = a.wrapping_add(((b as i32) >> 20) as u32);
                a = a.wrapping_add(get(p));
                set_be32(d, p + 4, a);
                p += 8;
            } else {
                p += 6;
            }
        } else {
            let r = a >> 27;
            if riscv_check2(v, r) {
                let v = le32(d, p + 4);
                let r = (r << 7).wrapping_add(0x17).wrapping_add(v & 0xffff_f000);
                let a = (a >> 12) | (v << 20);
                set_le32(d, p, r);
                set_le32(d, p + 4, a);
                p += 8;
            } else {
                p += 4;
            }
        }
    }
}

/// RISC-V decoding, the inverse of [`riscv_enc`].
fn riscv_dec(d: &mut [u8], pc: u32) {
    let size = d.len() & !1;
    if size <= 6 {
        return;
    }
    let lim = size - 6;
    let mut p = 0;
    while let Some(mut a) = riscv_scan(d, &mut p, lim) {
        let get = |p: usize| pc.wrapping_add(p as u32);
        if a & 8 == 0 {
            // JAL
            a = a.wrapping_sub(0x100 - 0x7f);
            if a & 0xd80 != 0 {
                p += 2;
                continue;
            }
            let old = (a + (0xef - 0x7f)) & 0xfff;
            let mut v = (d[p + 3] as u32) << 1 | (d[p + 2] as u32) << 9 | ((a & 0xf000) << 5);
            v = v.wrapping_sub(get(p));
            let a = old | (v << 11 & (1 << 31)) | (v << 20 & (0x3ff << 21)) | (v << 9 & (1 << 20)) | (v & (0xff << 12));
            set_le32(d, p, a);
            p += 4;
            continue;
        }
        // AUIPC
        let v = a;
        let a = le32(d, p);
        if v & 0xe80 == 0 {
            let r = a >> 27;
            if riscv_check2(v, r) {
                let mut b = be32(d, p + 4);
                let v = a >> 12;
                b = b.wrapping_sub(get(p));
                let a = (r << 7).wrapping_add(0x17).wrapping_add(b.wrapping_add(0x800) & 0xffff_f000);
                set_le32(d, p, a);
                set_le32(d, p + 4, v | (b << 20));
                p += 8;
            } else {
                p += 4;
            }
        } else {
            let b = le32(d, p + 4);
            if !riscv_check1(v, b) {
                p += 6;
            } else {
                let v = (a & 0xffff_f000) | (b >> 20);
                set_le32(d, p, (b << 12) | (0x17 + (2 << 7)));
                set_le32(d, p + 4, v);
                p += 8;
            }
        }
    }
}
