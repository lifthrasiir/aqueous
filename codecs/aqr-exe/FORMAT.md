# The `exe-recons/x86` module

This document specifies the module `exe-recons/x86` of aqr-formula entry formulas, and
[`exe-recons/arm64`](#the-exe-reconsarm64-module) and
[`exe-recons/cil`](#the-exe-reconscil-module) after it. The x86 module rebuilds a section
of x86 or x86-64 machine code from streams that a disfilter split it into:
instructions are parsed, and their fields (opcodes, ModR/M and SIB bytes, prefixes,
displacements, immediates, jump and call targets, addresses) go to streams by what they are,
which compress better apart than interleaved; fields that the opcodes around them tell best
(REX bits, 8-bit immediates) or that are rare stay with the opcodes. Relative targets become absolute, so that repeated
calls and references to one place look alike; calls to recent targets become indices into a
move-to-front cache; padding between functions and jump tables are recognised. The document is
meant to be complete: an implementation of the module can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas; the
module does not depend on them.

The filter descends from Fabian Giesen's disfilter (kkrunchy). Any byte sequence can be
encoded: what is not parsed as an instruction is escaped byte by byte, so the module only
fixes how the streams are read, not which code is valid.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Streams](#3-streams)
4. [Opcode classes](#4-opcode-classes)
5. [Decoding](#5-decoding)
6. [Encoding](#6-encoding-encoder) *(encoder)*
7. [Validation](#7-validation)

## 1. Conventions

- Integers are unsigned. `le16`, `le32` and `le64` are little-endian integers. Arithmetic on
  addresses is in `u64`, wrapping; a value is truncated to `u32` by keeping its low 32 bits.
- `out` is the output so far, and `|out|` its length. The **address** of the next output byte
  is `origin + |out|` (wrapping).
- "Read `n` bytes from S" takes the next `n` bytes of stream S in order and fails if fewer
  are left. "Copy `n` bytes from S" reads them and appends them to `out`.
- Byte values are hexadecimal. An opcode is named `m:op`, its opcode map `m` and its byte
  `op` (`0:E8` is `CALL rel32`, `1:B8` is `0F B8`).
- The x86 encoding is that of the Intel SDM (with APX's REX2 and EVEX map 4) and of
  sandpile.org. This document only says how its bytes are split and joined.

## 2. The module

- **Module**: name `exe-recons/x86`, version 3.
- **Params**: exactly 10 bytes.

  ```text
  le64    origin        the virtual address of the section's first byte
  u8      sentinel      bytes dropped from the end of the output, 0..15 (§5.1)
  u8      mode          0: x86-32, otherwise x86-64
  ```

- **Resources**: exactly one, the opcode classes of §4: 1280 bytes, five tables of 256 class
  codes (§4), one byte for each opcode `00`–`FF` in order: map 0 in x86-32, maps 1, 4 and 7,
  and map 0 in x86-64.
- **Inputs**: exactly 13, the streams of §3 in order. Each is of class `x86/NAME` (`x86/OP`,
  `x86/SIB`, ...).
- **Output**: the code of §5. Its length must be the `len` the application declares.

## 3. Streams

| # | name | contents |
|---|---|---|
| 0 | `OP` | the instruction stream: prefixes, map bytes, opcodes, ModR/M bytes, markers (§5.2), escaped bytes and jump table counts; the bytes of VEX and EVEX prefixes and REX bits (§5.2); 8-, 16- and 64-bit immediates; 16-bit displacements, addresses and jumps; padding records (§5.5) |
| 1 | `SIB` | SIB bytes |
| 2 | `CALL_IDX` | call cache indices (§5.4) |
| 3 | `DISP8_SP` | 8-bit displacements from `rsp` (`esp`), with an index or not |
| 4 | `DISP8_BP` | 8-bit displacements from `rbp` (`ebp`) |
| 5 | `DISP8` | other 8-bit displacements without an index |
| 6 | `DISP8_IDX` | other 8-bit displacements with an index |
| 7 | `DISP32` | 32-bit displacements |
| 8 | `JUMP8` | 8-bit relative jumps, as they are |
| 9 | `JUMP` | 32-bit jump targets, absolute: `le32` in x86-32, `le64` in x86-64; absolute far jump offsets (`le32`); entries of x86-64 jump tables, as they are (`le64`) |
| 10 | `IMM32` | 32-bit immediates |
| 11 | `ADDR` | 32- and 64-bit absolute addresses; in x86-64, the targets of RIP-relative operands (`le64`) |
| 12 | `CALL` | call targets missing from the cache: `le32` in x86-32, `le64` in x86-64 |

All 13 are always present, possibly empty. Bytes left in any stream when decoding ends are
ignored. A field is read from the next bytes of its stream, whatever fields came before them:
`OP` holds its fields in the order §5 reads them.

## 4. Opcode classes

Each opcode has a class, which says what follows it:

| class | code | what follows |
|---|---|---|
| `N_` | 0 | nothing |
| `N1` | 1 | an 8-bit immediate |
| `N2` | 2 | a 16-bit immediate |
| `N4` | 3 | a 32-bit immediate, 16-bit with `66` |
| `NZ` | 4 | a 32-bit immediate, 64-bit with REX.W |
| `J1` | 5 | an 8-bit relative jump |
| `J4` | 6 | a 32-bit relative jump, 16-bit with `66` in x86-32 |
| `JA` | 7 | a 32-bit absolute jump offset, 16-bit with `66` in x86-32 |
| `A_` | 8 | an absolute address (§5.6) |
| `R1` | 9 | ModR/M, then an 8-bit immediate |
| `XX` | 10 | invalid: never decoded |
| `R4` | 11 | ModR/M, then as `N4` |
| `R_` | 12 | ModR/M, then nothing |
| `BP` | 13 | none: a 1-byte prefix |
| `M2` | 14 | ModR/M, then as `N2` (`0F 38`, which an encoder writes as map 2) |
| `M3` | 15 | ModR/M, then as `N4` (`0F 3A`, which an encoder writes as map 3) |

A class has ModR/M when its code is 9 or more (`XX` and `BP` are dealt with before); the
operand after ModR/M is then of the class whose code is the low two bits of the code (`R1` →
`N1`, `R4` and `M3` → `N4`, `R_` → `N_`, `M2` → `N2`).

`class(m, op)`, in mode x86-32 or x86-64, with `R` the resource:

- map 0: `R[op]` in x86-32, `R[1024 + op]` in x86-64, but `XX` for `F1` and `F4` (the markers
  of §5.2) whatever the resource holds;
- map 1: `R[256 + op]`;
- maps 2, 5 and 6: `R_`;
- map 3: `R1`;
- map 4: `R[512 + op]`;
- map 7: `R[768 + op]`;
- maps 8 to 15: `XX`.

The resource is what an encoder parsed by, so opcodes are added to it without a new version of
the module: a decoder takes whatever valid resource it is given (§7), never tables of its own.

*Informative, up to the prefixes below:* the tables that follow are this
implementation's resource (`aqr_exe::arch::x86::Opcodes::BUILTIN`), to show what one looks
like. They may change, as opcodes are added, and an archive holds the ones it was made with:
table 0 for map 0 in x86-32; the same in x86-64 but `XX` for
`06 07 0E 0F 16 17 1E 27 2F 37 3F 40`–`4F` `60 61 62 82 9A C4 C5 CE D4 D5 D6 EA`; tables 1 and
4 for maps 1 and 4; and for map 7, `R4` for `F8` and `XX` otherwise. Its `F1` and `F4` of map 0
are `XX`.

Table 0 (legacy map 0, one-byte opcodes):

| | x0 | x1 | x2 | x3 | x4 | x5 | x6 | x7 | x8 | x9 | xA | xB | xC | xD | xE | xF |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0x | R_ | R_ | R_ | R_ | N1 | N4 | N_ | N_ | R_ | R_ | R_ | R_ | N1 | N4 | N_ | XX |
| 1x | R_ | R_ | R_ | R_ | N1 | N4 | N_ | N_ | R_ | R_ | R_ | R_ | N1 | N4 | N_ | N_ |
| 2x | R_ | R_ | R_ | R_ | N1 | N4 | BP | N_ | R_ | R_ | R_ | R_ | N1 | N4 | BP | N_ |
| 3x | R_ | R_ | R_ | R_ | N1 | N4 | BP | N_ | R_ | R_ | R_ | R_ | N1 | N4 | BP | N_ |
| 4x | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ |
| 5x | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ |
| 6x | N_ | N_ | R_ | R_ | BP | BP | BP | BP | N4 | R4 | N1 | R1 | N_ | N_ | N_ | N_ |
| 7x | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 | J1 |
| 8x | R1 | R4 | R1 | R1 | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 9x | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | JA | N_ | N_ | N_ | N_ | N_ |
| Ax | A_ | A_ | A_ | A_ | N_ | N_ | N_ | N_ | N1 | N4 | N_ | N_ | N_ | N_ | N_ | N_ |
| Bx | N1 | N1 | N1 | N1 | N1 | N1 | N1 | N1 | NZ | NZ | NZ | NZ | NZ | NZ | NZ | NZ |
| Cx | R1 | R1 | N2 | N_ | R_ | R_ | R1 | R4 | N2 | N_ | N2 | N_ | N_ | N1 | N_ | N_ |
| Dx | R_ | R_ | R_ | R_ | N1 | N1 | N_ | N_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| Ex | J1 | J1 | J1 | J1 | N1 | N1 | N1 | N1 | J4 | J4 | A_ | J1 | N_ | N_ | N_ | N_ |
| Fx | BP | N_ | BP | BP | N_ | N_ | R1 | R4 | N_ | N_ | N_ | N_ | N_ | N_ | R_ | R_ |

Table 1 (legacy map 1, `0F xx`):

| | x0 | x1 | x2 | x3 | x4 | x5 | x6 | x7 | x8 | x9 | xA | xB | xC | xD | xE | xF |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0x | R_ | R_ | N_ | N_ | XX | N_ | N_ | N_ | N_ | N_ | XX | XX | XX | R_ | N_ | R1 |
| 1x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | XX | R_ | R_ |
| 2x | R_ | R_ | R_ | R_ | XX | XX | XX | XX | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 3x | N_ | N_ | N_ | N_ | N_ | N_ | XX | N_ | M2 | XX | M3 | XX | XX | XX | XX | XX |
| 4x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 5x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 6x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 7x | R1 | R1 | R1 | R1 | R_ | R_ | R_ | N_ | N_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 8x | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 | J4 |
| 9x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| Ax | N_ | N_ | N_ | R_ | R1 | R_ | R_ | R_ | N_ | N_ | N_ | R_ | R1 | R_ | R_ | R_ |
| Bx | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | N_ | R_ | R1 | R_ | R_ | R_ | R_ | R_ |
| Cx | R_ | R_ | R1 | R_ | R1 | R1 | R1 | R_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ | N_ |
| Dx | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| Ex | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| Fx | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | XX |

Table 4 (EVEX map 4, APX):

| | x0 | x1 | x2 | x3 | x4 | x5 | x6 | x7 | x8 | x9 | xA | xB | xC | xD | xE | xF |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0x | R_ | R_ | R_ | R_ | XX | XX | XX | XX | R_ | R_ | R_ | R_ | XX | XX | XX | XX |
| 1x | R_ | R_ | R_ | R_ | XX | XX | XX | XX | R_ | R_ | R_ | R_ | XX | XX | XX | XX |
| 2x | R_ | R_ | R_ | R_ | R1 | XX | XX | XX | R_ | R_ | R_ | R_ | R1 | XX | XX | XX |
| 3x | R_ | R_ | R_ | R_ | XX | XX | XX | XX | R_ | R_ | R_ | R_ | XX | XX | XX | XX |
| 4x | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ | R_ |
| 5x | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| 6x | R_ | R_ | XX | XX | XX | R_ | R_ | XX | XX | R4 | XX | R1 | XX | XX | XX | XX |
| 7x | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| 8x | R1 | R4 | XX | R1 | N_ | N_ | XX | XX | R_ | XX | XX | XX | XX | XX | XX | R_ |
| 9x | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| Ax | XX | XX | XX | XX | XX | R_ | XX | XX | XX | XX | XX | XX | XX | R_ | XX | R_ |
| Bx | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| Cx | R1 | R1 | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| Dx | R_ | R_ | R_ | R_ | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| Ex | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX | XX |
| Fx | R_ | R_ | R_ | XX | R_ | R_ | R1 | R4 | R_ | R_ | XX | XX | XX | XX | R_ | R_ |

*(End of the informative tables.)*

The **prefixes** are `26 2E 36 3E 64 65 66 67 F0 F2 F3`, the only opcodes that the resource
may give the class `BP`, in map 0 only. The set `P` of prefixes seen since the last instruction
is what "with `66`" and the like refer to.

## 5. Decoding

### 5.1 The loop

State: `out` empty, the set `P` empty, the flag `pad` clear and the call cache of §5.4. Then,
as long as `pad` is set or `OP` has bytes left:

1. If `pad` is set: read a padding record (§5.5), clear `pad`, and repeat.
2. Otherwise read a byte `b` from `OP` and decode an item (§5.2).

At the end, drop the last `sentinel` bytes of `out` (an error if it is shorter). `out` is the
output.

### 5.2 Items

By `b`:

- `F4` (escape): read a byte from `OP` and append it. Empty `P`.
- `F1` (jump table): read `c` from `OP`; `c + 1` entries follow. In x86-64, copy 8 bytes from
  `JUMP` per entry. In x86-32, read a call target per entry (§5.4) and append it as
  `le32`. `P` is kept.
- `C4`, `C5`, `62` (VEX3, VEX2, EVEX): read from `OP` the EVEX byte (EVEX only), then the
  VEX byte, then the REX byte, then the next byte `b`; continue below with this prefix.
- `0F` (REX) and `D5` (REX2), in x86-64 only (an error in x86-32): read the REX byte from
  `OP`, then the next byte `b`; continue below with this prefix.
- Anything else: an instruction without such a prefix, continued below.

Then the opcode: if `b` is `40`–`4F`, the map is `m = b − 40` and the opcode the next byte
`op` from `OP`; otherwise `m = 0` and `op = b`. With its class `k = class(m, op)`:

- `XX`: an error.
- `BP` (only in map 0): add `op` to `P`, append `op`, and go on to the next item. (Encoders
  never put a prefix marker before it; this implementation then appends `op` alone.)
- Anything else: the instruction of §5.3.

### 5.3 Instructions

1. **Prefix and opcode bytes.** By the prefix:
   - VEX3: with `v` the VEX byte and `r` the REX byte, an error unless `v ≥ 80`,
     `r < 10` and `m < 16`. Append `C4`, `((~r & 7) << 5) | m`, `((r & 8) << 4) | (v & 7F)`,
     `op`.
   - VEX2: an error unless `v ≥ 80`, `r & FB = 0` and `m = 1`. Append `C5`,
     `((~r & 4) << 5) | (v & 7F)`, `op`.
   - EVEX: with `e` the EVEX byte, an error unless `e & 28 = 0`, `r < 80` and `m < 8`.
     Append `62`, then
     `x = ((~r & 7) << 5) | ((~r >> 2) & 10) | ((~r >> 1) & 08) | m`,
     `y = ((r & 8) << 4) | (v & 7B) | ((~r >> 3) & 04)`,
     `z = e | ((v & 04) << 3) | ((v >> 4) & 08)`, then `op` (all in `u8`).
   - REX: an error unless `r < 10`; append `40 | r`, then the opcode bytes below.
   - REX2: append `D5`, `r`, then the opcode bytes below.
   - none: the opcode bytes below.

   The opcode bytes, by `m`: `op` for 0, `0F op` for 1, `0F 38 op` for 2, `0F 3A op` for 3;
   any other map is an error here (they come with VEX or EVEX only).
2. **State.** Take `P` as the instruction's prefixes and empty it. With `r` the REX byte of
   the instruction (0 if none), **REX.W** is `r & 8 ≠ 0`, **REX.X** `r & 2 ≠ 0` and **REX.B**
   `r & 1 ≠ 0`. The opcode `m:op` adjusts:
   - `0:9A`, `0:EA`, `0:C8`: copy 2 bytes from `OP` first (the segment of a far pointer,
     or `ENTER`'s 16-bit operand).
   - `0:F6`, `0:F7`, `4:F6`, `4:F7`: if the `reg` field (bits 5–3) of the next byte of `OP`,
     its ModR/M, is 2 or more, the class is `R_` (an error if `OP` is empty).
   - `1:B8` with `F2` or `F3` in `P` (`POPCNT`): the class is `R_`.
   - `7:F8`: `66` is taken out of `P`.
3. **Padding.** `pad` is set if `m:op` is one of `0:9A 0:C2 0:C3 0:CA 0:CB 0:CC 0:CF 0:E8
   0:E9 0:EA 0:EB` (calls, jumps, returns and `INT3`), and cleared otherwise.
4. **ModR/M**, if the class has it (§4): read the ModR/M byte `M` from `OP` and append it;
   the class becomes that of its low two bits. With `mod = M >> 6`, `rm = M & 7`, and
   **addr16** meaning x86-32 with `67` in `P`:
   - if not addr16, `mod < 3` and `rm = 4`: read the SIB byte `S` from `SIB` and append it;
     otherwise `S = 0`.
   - The displacement:

     | condition | what is appended |
     |---|---|
     | `mod = 0`, addr16, `rm = 6` | 2 bytes copied from `OP` |
     | `mod = 0`, addr16, other `rm` | nothing |
     | `mod = 0`, `rm = 5`, x86-64 | `le32` of `A − (address + 4 + d)`, truncated, for `A` read as `le64` from `ADDR` |
     | `mod = 0`, `rm = 5`, x86-32 | 4 bytes copied from `ADDR` |
     | `mod = 0`, `S & 7 = 5` | 4 bytes copied from `DISP32` |
     | `mod = 1` | 1 byte copied from the stream of the table below |
     | `mod = 2`, addr16 | 2 bytes copied from `OP` |
     | `mod = 2` | 4 bytes copied from `DISP32` |
     | otherwise | nothing |

     The rows are tried in order. `address` is that of the displacement's first byte, and `d`
     the size of the immediate still to come: 1 for `N1`, 2 for `N4` with `66` in `P`, 4 for
     `N4` without, and 0 for any other class (`N2` too). The result is the RIP-relative
     displacement whose target is `A`.

     The stream of an 8-bit displacement, by the first row that holds:

     | condition | stream | (as in) |
     |---|---|---|
     | addr16 | `DISP8` | `[bp+si+d8]` |
     | `rm = 4`, `S & 7 = 4`, not REX.B | `DISP8_SP` | `[rsp+d8]`, `[rsp+rcx*2+d8]` |
     | `rm = 4`, `(S >> 3) & 7 ≠ 4` or REX.X | `DISP8_IDX` | `[rcx+rdx*4+d8]`, `[rcx+r12+d8]` |
     | `rm = 5`, not REX.B | `DISP8_BP` | `[rbp+d8]` |
     | otherwise | `DISP8` | `[rcx+d8]`, `[r12+d8]`, `[r13+d8]` |

     (The bits that REX2 and EVEX add for registers 16 to 31 are not looked at.)
5. **The operand**, by the class:
   - `J4`, in x86-32 with `66` in `P`: copy 2 bytes from `OP`.
   - `J4` otherwise: the target `T` is, for `0:E8` (`CALL`), a call target (§5.4); for others
     a `le64` from `JUMP` in x86-64 or a `le32` from `JUMP` in x86-32. Append the `le32`
     of `T − (address + 4)`, truncated, `address` being that of its first byte.
   - `A_`: `n` bytes copied from `OP` for `n` = 2, or from `ADDR` for `n` = 4 or 8. For
     `0:EA`, `n` is 2 with `66` in `P` and 4 without; for the others, `n` is 4 in x86-32 and
     8 in x86-64, halved with `67` in `P`.
   - `JA`: in x86-32 with `66` in `P`, 2 bytes copied from `OP`; otherwise 4 bytes copied
     from `JUMP`.
   - `J1`: 1 byte copied from `JUMP8`.
   - `N_`: nothing.
   - `N1`, `N2`, `N4`, `NZ`: `NZ` without REX.W is `N4`; then `N4` with `66` in `P` is `N2`.
     Copy 1, 2, 4 or 8 bytes (`N1`, `N2`, `N4`, `NZ`), from `IMM32` for 4 and from `OP`
     otherwise.

The REX byte of a VEX or EVEX prefix carries its W, X and B bits as bits 3, 1 and 0, as that
of a REX or REX2 prefix does, so REX.W, REX.X and REX.B read them.

### 5.4 The call cache

The cache holds 256 `u64` entries, all 0 at the start. A **call target** is read thus: read
an index `i` from `CALL_IDX`. If `i < FF`, the target is entry `i`. If `i = FF`, it is a
`le64` from `CALL` in x86-64, or a `le32` from `CALL` in x86-32 (zero-extended). Then the
target moves to the front: entries `0 … i−1` move to `1 … i` (for `FF`, entries `0 … FE` move
to `1 … FF` and the last one is dropped), and entry 0 becomes the target.

Calls and the entries of x86-32 jump tables share the cache.

### 5.5 Padding records

A record is read from `OP`, `a` being the address of the next output byte. By its first byte:

- `00`: nothing.
- `01`: `16 − (a & F)` bytes `CC` are appended (16 where `a` is a multiple of 16).
- `02`: one byte `CC` is appended.
- `03`: read a length `n` and copy `n` bytes, from `OP`; repeat while `n = FF`.
- anything else: an error.

A record follows every instruction that sets `pad`, even the last one of the section.

## 6. Encoding *(encoder)*

`aqr_exe::arch::x86::encode` takes the code, its origin and the mode, and never fails. The code
is extended with 15 zero bytes, the **sentinel**, so that an instruction can be parsed whole
near the end; while some of the original bytes are left, an item is encoded at the current
position:

1. **Padding**, after an instruction that set `pad`: the bytes up to the sentinel are scanned
   for zero bytes at the start, then for any number of `CC`, `90`, `66 90`, `0F 1F 00`,
   `0F 1F 40 xx`, `0F 1F 44 00 xx`, `66 0F 1F 44 00 xx`, `0F 1F 80 xx xx xx xx`,
   `0F 1F 84 00 xx xx xx xx`, and the latter with one to three `66` before it. What matched
   (possibly nothing) is written as `00` if nothing, `01` if all `CC` up to the next
   multiple of 16, `02` if a single `CC`, and otherwise as `03`, then the bytes in chunks of
   255, each after `FF`, then the rest after its length.
2. **Jump tables**, of at least 3 entries, up to the sentinel. In x86-32, at any position: a
   run of `le32` values within the section's addresses, `[origin, origin + length − 1]`. In
   x86-64, at addresses that are multiples of 8: a run of `le64` values within
   `±(length − 1)` as signed integers. The run is cut into tables of at most 256 entries,
   each written as `F1`, its count less one in `OP`, and the entries.
3. **An instruction**, parsed as §5.3 reads it:
   - VEX3 (unless bit 4 of its first payload byte is set), VEX2 and EVEX prefixes are taken
     in x86-64, and in x86-32 when the byte after `C4`, `C5` or `62` has `mod = 3` (otherwise
     they are `LES`, `LDS`, `BOUND`). Their bits are rearranged as the inverses of §5.3.
   - Otherwise, in x86-64, a REX byte (`40`–`4F`) is taken as `0F` in `OP` and its low
     nibble in `REX`, and `D5` with the next byte as REX2. Then `0F` starts map 1.
   - `0F 38` and `0F 3A` become maps 2 and 3 (with a VEX or EVEX prefix, they are invalid).
   - The map byte `40 + m` is written before the opcode when `m > 0`, and also when `op` is
     itself `40`–`4F`, so that it cannot be read as one.
   - Escaped instead (`F4` and the instruction's first byte, after which parsing resumes at
     the next byte, with the prefixes dropped): an opcode of class `XX`, a 1-byte prefix
     after a REX, REX2 or VEX prefix, and, without such prefix, `C4`, `C5`, `62` and `D5` in
     map 0 (which would read as markers).
   - A 1-byte prefix is written alone and added to `P`.
   - The remaining fields go to the streams of §5.3. Relative targets are made absolute:
     `JUMP` and `ADDR` hold the 64-bit address in x86-64, `JUMP` the 32-bit one in x86-32. A call target
     already in the cache is written as its index; otherwise `FF` and the target, and the
     cache is updated as in §5.4 either way.
4. When the original bytes are used up, an instruction may have run into the sentinel: the
   number of sentinel bytes left unread is subtracted from 15 to give `sentinel`. A record of
   padding is written if `pad` is still set.

`aqr_exe::format::pe::locate_code` lists the sections of a PE32 or PE32+ image with
`IMAGE_SCN_CNT_CODE` (`0x20`), at `ImageBase + VirtualAddress`, stored at `PointerToRawData`
for `SizeOfRawData` bytes. The machine type is not checked. aqr-compress (`src/exe.rs`) applies
the module to each such section that lies within the file and after the previous one, cut to
the file, with `mode` from the image's magic and `Opcodes::BUILTIN` as the resource. The rest
of the image is one stream of class `pe`, sliced around the sections. aqr-compress keeps the
image raw if the formula would not archive smaller.

## 7. Validation

The module fails if:

- the params are not 10 bytes long (this implementation ignores trailing bytes), there is not
  exactly one resource, or not exactly 13 inputs;
- the resource is not 1280 bytes long, holds a byte above `0F` (no class), or gives the class
  `BP` to an opcode other than a prefix of map 0 (§4);
- a stream runs out where a byte is to be read or copied, or a padding record starts with a
  byte above `03`;
- an opcode is of class `XX`, a map is above 3 without a VEX or EVEX prefix, a REX or REX2
  marker appears in x86-32, or the bits of a prefix fail the checks of §5.3;
- `out` is shorter than `sentinel` at the end, or the output is not `len` bytes long.

# The `exe-recons/arm64` module

This part specifies the module `exe-recons/arm64`, which rebuilds a section of ARM64 (A64)
machine code. The instructions stay in one stream, as big-endian words, so that their opcode
bits come first and repeated instructions match as a whole; the targets of `BL` and `ADRP`
are made absolute, so that calls to one function and references to one page look alike; and
the offsets of local branches are taken out of their words, which then repeat, to streams of
their own, zigzag coded, a byte of each in each. Any byte sequence can be encoded. The
conventions of §1 hold.

## A1. The module

- **Module**: name `exe-recons/arm64`, version 1.
- **Params**: exactly 8 bytes, `le64 origin`, the virtual address of the section's first byte.
- **Resources**: none.
- **Inputs**: exactly 11, the streams of §A2 in order, each of class `arm64/NAME`.
- **Output**: the section of §A3, `len` bytes long (the `len` the application declares).

## A2. Streams

| # | name | contents |
|---|---|---|
| 0 | `CODE` | one `be32` word per instruction, transformed (§A3) |
| 1–4 | `B3` … `B0` | offsets of `B`: bytes 3 (most significant) to 0 of their zigzag values |
| 5–7 | `BCOND2` … `BCOND0` | offsets of `B.cond`, `BC.cond`, `CBZ` and `CBNZ`: bytes 2 to 0 |
| 8–9 | `TBZ1`, `TBZ0` | offsets of `TBZ` and `TBNZ`: bytes 1 and 0 |
| 10 | `TAIL` | the last `len mod 4` bytes of the section, as they are |

## A3. Decoding

For `k` from 0 to `floor(len / 4) − 1`, with `pc = origin + 4k`: read `w` as a `be32` from
`CODE`, rebuild it by the first row of the table that its bits match, and append it to `out`
as a `le32`. `x[h:l]` are bits `h` down to `l` of `x`; `m(n) = 2^n − 1`.

| instruction | match | rebuilt |
|---|---|---|
| `BL` | `w & FC000000 = 94000000` | `w[25:0] ← (w[25:0] − (pc >> 2)) & m(26)` |
| `ADRP` | `w & 9F000000 = 90000000` | with `a = w[23:5] · 4 + w[30:29]`, `r = (a − (pc >> 12)) & m(21)`: `w[30:29] ← r & 3`, `w[23:5] ← r >> 2` |
| `B` | `w & FC000000 = 14000000` | local branch: field `w[25:0]` (26 bits), streams `B3`… |
| `B.cond`, `BC.cond` | `w & FF000000 = 54000000` | local branch: field `w[23:5]` (19 bits), streams `BCOND2`… |
| `CBZ`, `CBNZ` | `w & 7E000000 = 34000000` | as `B.cond` |
| `TBZ`, `TBNZ` | `w & 7E000000 = 36000000` | local branch: field `w[18:5]` (14 bits), streams `TBZ1`… |
| anything else | | `w` as it is |

A **local branch** of a field of `n` bits: the field of `w` must be 0. Read one byte from each
of its `ceil(n / 8)` streams, most significant first, into `z`, which must be below `2^n`.
The offset is `v = (z >> 1) XOR −(z & 1)` (unsigned wrapping), and the field becomes
`v & m(n)`.

Then copy `len mod 4` bytes from `TAIL`. The matches only look at bits outside the fields
that are transformed, so a word is rebuilt by the same row it was encoded by.

## A4. Encoding *(encoder)*

`aqr_exe::arch::arm64::encode` takes the code and its origin, and never fails. Each whole
word, read as a `le32`, is matched against the table of §A3 in order and transformed the
other way: for `BL` and `ADRP` the target is added to `pc >> 2` or `pc >> 12` instead of
subtracted; for a local branch, the field is cleared and its value `v`, sign-extended from
its `n` bits to `s`, is written as `z = (s << 1) XOR (s >> 31)` (arithmetic shift), a byte to
each stream. The word goes to `CODE` big-endian, and the bytes after the last whole word to
`TAIL`.

aqr-compress filters the sections with instructions of Mach-O images of `CPU_TYPE_ARM64`
(`S_ATTR_PURE_INSTRUCTIONS` or `S_ATTR_SOME_INSTRUCTIONS`, not zero-filled) and the
executable sections of PE images whose machine is `AA64`.

Why this layout (measured on 298 MB of code from Mach-O files: Rust, Go, C and C++, LLVM;
zstd -9, each stream on its own): the code alone, 114.6 MB; with xz's ARM64 BCJ filter (`BL`
and `ADRP` absolute), 100.7 MB (−12.1%); with this filter, 94.5 MB (−17.6%). Splitting every
instruction into its fields by encoding class (registers by role, immediates by kind) archived
larger than the code alone: words repeat as a whole and lose their matches once apart. Taking
`BL` or `ADRP` targets or immediates out of the words, or making local branches absolute, is
larger too; zigzag coding and byte planes together take 2.6 points off the offsets' bytes
as they are.

## A5. Validation

The module fails if:

- the params are not 8 bytes long, or there are resources, or not exactly 11 inputs;
- a stream runs out where a byte is to be read;
- a local branch's field in `CODE` is not 0, or its zigzag value is not below `2^n`;
- any stream has bytes left once the output is `len` bytes long.

# The `exe-recons/cil` module

This part specifies the module `exe-recons/cil`, which rebuilds the section of a .NET image
(ECMA-335 §II.24 and §II.25) that holds its metadata. The metadata root and the headers in it
are kept as they are; the heaps go to streams of their own; the metadata tables go column by
column, a byte plane of each column at a time, columns in ascending order as deltas, and the
RVAs of method bodies as indices into the bodies; the IL of the method bodies is split by
operand, its tokens through a move-to-front cache of each table, and the row numbers that miss
it as deltas. Everything else in the section (the CLR header, resources, the strong name
signature, native code, padding) is kept as it is in a stream of its own. Any metadata the
module reads is rebuilt bit for bit; what it does not take is kept. The conventions of §1
hold.

## C1. The module

- **Module**: name `exe-recons/cil`, version 1.
- **Params**: exactly 8 bytes.

  ```text
  le32    origin        the RVA of the section's first byte
  le32    meta          the offset of the metadata root in the section
  ```

- **Resources**: none.
- **Inputs**: exactly 26, the streams of §C2 in order, each of class `cil/NAME`.
- **Output**: the section of §C3, `len` bytes long (the `len` the application declares).

## C2. Streams

| # | name | contents |
|---|---|---|
| 0 | `REST` | the bytes of the section not otherwise taken (§C3.5), in order |
| 1 | `META` | the metadata root through its stream headers, then the header of the tables stream |
| 2–5 | `STRINGS`, `US`, `BLOB`, `GUID` | the heaps `#Strings`, `#US`, `#Blob` and `#GUID` |
| 6 | `TABLES` | the rows of the tables, by column and byte plane (§C3.4) |
| 7 | `COLFLAGS` | a byte for each column but MethodDef's RVAs: 1 where the column is in deltas |
| 8 | `RVAX` | MethodDef RVAs that are not of a body taken, `le32` |
| 9 | `BODYPOS` | for each body taken, its distance from the end of the last (`varint`) |
| 10 | `HDR` | the headers of the bodies, tiny (1 byte) or fat (12 bytes), as they are |
| 11 | `EH` | after the code of fat bodies with more sections: the padding and the data sections |
| 12 | `OP` | opcodes: their bytes, `FE` and the byte after it for two-byte opcodes |
| 13–16 | `I1`, `I2`, `I4`, `I8` | operands of 1, 2, 4 and 8 bytes (`float32` among the 8-byte ones) |
| 17 | `BR1` | 8-bit branch offsets |
| 18 | `BR4` | 32-bit branch offsets, and the operands of `switch` (count and offsets) |
| 19 | `TOKT` | the table byte (bits 31..24) of each token |
| 20 | `TOKI` | for each token: 0 where it misses the cache, else its index in it plus 1 |
| 21–25 | `TOKR_STR`, `TOKR_METHOD`, `TOKR_FIELD`, `TOKR_TYPE`, `TOKR_OTHER` | the row numbers of tokens that miss the cache, as deltas (§C3.3), by table: `70`; `06 0A 2B`; `04`; `01 02 1B`; any other |

A `varint` is an unsigned LEB128 integer of at most 5 bytes, below 2^32: 7 bits a byte, the
least significant first, bit 7 set on every byte but the last. `zigzag(v) = (v << 1) XOR
(v >> 31)` of a 32-bit `v` (arithmetic shift), and `unzigzag(z) = (z >> 1) XOR −(z & 1)`.

## C3. Decoding

`out` is `len` zero bytes, written at offsets; each part written is **taken**, as a range of
`out`. Read the metadata root (§C3.1), the method bodies (§C3.2, §C3.3), the tables (§C3.4),
then the rest (§C3.5).

### C3.1 The metadata root

Read from `META` the metadata root (§II.24.2.1): 16 bytes, of which `le32` at 12 is the length
`n` of the version string, then `n` bytes, then 4 bytes, of which `le16` at 2 is the count of
streams; then each stream header, `le32 offset`, `le32 size` and a name ending at its first
zero byte, padded with the bytes after it to a multiple of 4. Write it all at `meta`, taken.

Then, for each stream header in order, with `start = meta + offset`:

- the first named `#Strings`, `#US`, `#Blob` or `#GUID` of each name: copy `size` bytes from
  its stream to `start`, taken;
- the first named `#~` or `#-`: read the tables' header (below) from `META` and write it at
  `start`. With `rows` the bytes of the rows of every table (§C3.4), `start + |header| +
  rows` must not exceed `start + size`; `start` to `start + |header| + rows` is taken, and the
  tables start at `start + |header|`;
- any other: nothing.

The tables' header (§II.24.2.6): 24 bytes, of which byte 6 is `HeapSizes` and `le64` at 8 the
mask `Valid`, then a `le32` row count for each bit of `Valid` set, from bit 0 up, then 4 more
bytes where `HeapSizes` has bit `40`. `Valid` must have no bit above `2C` set.

### C3.2 The method bodies

With `at = 0`, while `BODYPOS` has bytes left: read a `varint` `g`; the body is at `b = at + g`.
Read its header from `HDR`, at `b`:

- tiny (byte `h` with `h & 3 = 2`): 1 byte; the code is `h >> 2` bytes;
- fat (`h & 3 = 3`): 12 bytes, whose `le16` at 0 must be `3` in its bits 15..12; the code is
  `le32` at 4 bytes;
- else the module fails.

The code follows the header (§C3.3). For a fat header with bit `08` (`CorILMethod_MoreSects`)
of its byte 0, the data sections follow it, each: as many bytes copied from `EH` as align
its offset in the section to 4, then a section whose first byte `k` tells its size `n` (with `k & 40`, `le24`
of the 3 bytes after `k`; else the byte after it), `n` at least 4, copied from `EH` (`k`
included); another follows while `k & 80`. `b` to the end of the last part is taken, and is
body `i` (counting from 0) at `b`; `at` becomes its end.

### C3.3 IL

The code of `c` bytes is instructions (§III), written one after the other until `c` bytes are
written; an instruction that would end past them makes the module fail. Each: a byte from
`OP`; if it is `FE`, another byte from `OP` after it. Then its operand, by the opcode:

| opcode | operand |
|---|---|
| `0E`–`13`, `1F`; `FE 12`, `FE 19` | 1 byte from `I1` |
| `FE 09`–`FE 0E` | 2 bytes from `I2` |
| `20` | 4 bytes from `I4` |
| `22` | 4 bytes from `I8` |
| `21`, `23` | 8 bytes from `I8` |
| `2B`–`37`, `DE` | 1 byte from `BR1` |
| `38`–`44`, `DD` | 4 bytes from `BR4` |
| `45` | 4 bytes from `BR4`, the count `m` (`le32`), then `4m` bytes from `BR4` |
| `27`–`29`, `6F`–`75`, `79`, `7B`–`81`, `8C`, `8D`, `8F`, `A3`–`A5`, `C2`, `C6`, `D0`; `FE 06`, `FE 07`, `FE 15`, `FE 16`, `FE 1C` | a token (below) |
| any other | none |

A **token**: read its table `t` from `TOKT` and `k` from `TOKI`. Each table has a cache, a
list of row numbers, the most recent first, empty at the start of the section, and a last row
number, 0 at the start. With `k > 0`, the row number is entry `k − 1` of the cache of `t`,
which must exist, and moves to the front. With `k = 0`, read a `varint` `z` from the
`TOKR_*` stream of `t`; the row number is `r = last + unzigzag(z)` (wrapping), which must be
below 2^24, becomes the last, and goes to the front of the cache, which keeps its first 255
entries. Write `le32 (t << 24 | r)`.

### C3.4 The tables

Each table present in `Valid`, from table `00` up to `2C`, is its row count of rows, of the
columns of ECMA-335 §II.22, in the order listed there, without padding. A column is 2 or 4
bytes wide:

- a constant of 2 or 4 bytes (`Constant`'s `Type` and its padding byte as one of 2);
- an index into `#Strings`, `#GUID` or `#Blob`: 4 bytes where `HeapSizes` has bit `01`, `02`
  or `04`, else 2;
- an index into table `x`: 2 bytes where `x` has fewer than 2^16 rows, else 4;
- a coded index of `n` tag bits (§II.24.2.6): 2 bytes where every table it may index has
  fewer than `2^(16 − n)` rows, else 4. The unused tags of `CustomAttributeType` count none.

The tables follow one another from where §C3.1 says they start. For each table in order, for
each of its columns in order, with `R` rows: read the values `v[0..R]` of width `w`, a byte
plane at a time (`R` bytes from `TABLES` for byte 0 of each value, then for byte 1, ...), and
then:

- MethodDef's RVA column (table `06`, column 0) is read as 4 bytes wide whatever its width,
  and each value is mapped, with `p` the index of the last body so mapped (−1 at first): 0 is
  0; 1 is a `le32` read from `RVAX`; any `z ≥ 2` is `origin + b` (wrapping) for the body `i = p +
  1 + unzigzag(z − 2)` at `b` (§C3.2), which must exist, and `p` becomes `i`;
- any other column: read a byte from `COLFLAGS`; 1: each `v[r]` for `r ≥ 1` becomes `v[r] +
  v[r − 1]` modulo `2^(8w)`; 0: as it is; else the module fails.

The value of row `r` is written at its place in the row, `w` bytes little-endian.

### C3.5 The rest

The ranges taken, in order of their start, must not overlap and must lie within `out`. Copy
from `REST` the bytes between them, before the first and after the last, in order.

## C4. Encoding *(encoder)*

`aqr_exe::arch::cil::encode` takes the section, its RVA and the offset of its metadata root,
and fails where there is no `BSJB` signature at that offset, the root or the tables' header
is cut short, the tables have bits above `2C` or rows past their stream, or the parts of
§C3.1 overlap or are not within the section. The bodies it takes are those at the RVAs of
the MethodDef table within the section, in ascending order, each where it is whole within the
section (a fat header of 12 bytes, its data sections each at least 4 bytes), starts at or
after the end of the last taken, overlaps no part of the metadata, and its code is whole
instructions by §C3.3. It writes `0` to `COLFLAGS` but for columns of more than one row in
ascending order (each value at least the one before), which go as deltas, and maps an RVA to a
body only where it is the RVA of one taken.

`aqr_exe::format::pe::locate_code` finds the metadata through the CLR header (data directory
14) of images of IL alone (`COMIMAGE_FLAGS_ILONLY`) and of ReadyToRun images (a
ManagedNativeHeader with the `RTR` signature), of any machine. aqr-compress applies the
module to the section holding the metadata, in place of the x86 or ARM64 filter there; the
native code of a ReadyToRun image of x86-64 (the extents of its runtime functions, those
closer than 64 bytes in one) is filtered by `exe-recons/x86` within `REST`, where whole within
one of its ranges.

Why this layout (measured on the 84 .NET assemblies of IL alone in aqr-exe's sample, 66.1 MB;
zstd -9, each stream on its own): the files, 19.54 MB; their parts each in a stream of its
own, 20.24 MB, of which the tables 6.07 MB and the IL 3.88 MB. The tables by column 4.62 MB,
with deltas 4.36 MB; the IL split by operand alone 3.87 MB, its tokens 2.17 MB of it, which
the cache and deltas bring to 1.12 MB (1.14 MB with 64 entries). In all 17.06 MB, −12.7%.
Grouping the columns into streams by kind or by table makes no difference worth its streams
(17.06 to 17.13 MB).

## C5. Validation

The module fails if:

- the params are not 8 bytes long, or there are resources, or not exactly 26 inputs;
- a stream runs out where bytes are to be read, or a `varint` is longer than 5 bytes or not
  below 2^32;
- a method header is neither tiny nor fat of 12 bytes, a data section is shorter than 4
  bytes, an instruction ends past its code, a cache index is past the cache, a row number is
  not below 2^24, an RVA's body does not exist, or a `COLFLAGS` byte is above 1;
- `Valid` has a bit above `2C`, or the tables' rows run past their stream;
- the ranges taken overlap or are not within `out`;
- any stream has bytes left once the output is `len` bytes long.
