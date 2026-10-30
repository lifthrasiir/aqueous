# aqr-wasm format

This document specifies the module `wasm-recons` of aqr-formula entry formulas, which rebuilds
a WebAssembly module in the binary format (WebAssembly Core, "Binary Format"; version 1, as of
WebAssembly 3.0) from streams: the instructions of its code split by field, its data segments'
bytes and its custom sections apart. It is meant to be complete: the module can be implemented
from it alone, without knowing more of WebAssembly than its LEB128s. Sections marked
*(encoder)* describe the choices this implementation makes when it builds formulas; the module
does not depend on them.

Nothing is derived from what the module means: every byte comes back from the streams as it
was, LEB128s of any length included, but for the sizes of the sections and functions split
(§5) and the local indices (§4.3), which are written in the shortest form. A module the encoder
cannot split in part (an instruction it does not know, a LEB128 longer than it needs to be where
it is derived) keeps that part as it is.

## 1. Conventions

- Integers are unsigned unless said otherwise.
- **LEB128**: 7 bits per byte, least significant group first, bit 7 set on every byte but the
  last. A LEB128 "of at most `m` bytes" is read as the bytes up to the first without bit 7,
  which must be within `m` bytes; its **value** is the 64 bits of the groups (groups beyond the
  64th bit dropped). **Copying** a LEB128 from a stream copies its bytes as they are. Writing
  `v` writes it in the shortest form. `L32` is 5 bytes, `L64` 10.
- **zigzag**: `zz(v) = 2v` for `v ≥ 0`, `−2v − 1` for `v < 0`; `unzz` its inverse.
- A stream is read from its start, each read taking the next bytes; reading past its end is an
  error.

## 2. The module

- **Module**: name `wasm-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly 14 streams, in order (classes `wasm/<name>`):

  | # | name | holds |
  |---|---|---|
  | 0 | `skeleton` | the header; the sections' ids, sizes and counts; how each function is kept |
  | 1 | `sections` | the sections kept as they are (but custom ones), the functions kept as they are |
  | 2 | `custom` | the custom sections' contents |
  | 3 | `locals` | the functions' declarations of locals |
  | 4 | `op` | opcodes, memory arguments' flags and the first bytes of their offsets, `i32.const` and `call` immediates |
  | 5 | `local` | local indices' codes (§4.3) |
  | 6 | `label` | the labels of `br` and `br_if` |
  | 7 | `brtable` | the labels of `br_table` |
  | 8 | `blocktype` | block types |
  | 9 | `offset` | memory arguments' offsets but their first bytes |
  | 10 | `i64` | `i64.const` immediates |
  | 11 | `bytes` | float and vector constants, shuffle lanes, lane indices |
  | 12 | `misc` | the other immediates; the data segments but their bytes |
  | 13 | `data` | the data segments' bytes |

- **Output**: the module, `len` bytes (the declared length).

## 3. Sections

The output starts with the first 8 bytes of `skeleton`, which must be `00 61 73 6D 01 00 00 00`.
Then, until `skeleton` is used up, a section:

1. Copy a byte of `skeleton`, the section's `id`.
2. If `id` is 10 (code) or 11 (data), read a byte `k` of `skeleton` (not copied): 0 means the
   section is split (§5), 1 that it is kept as it is; anything else is an error.
3. A section kept as it is (any other `id`, or `k = 1`): copy a LEB128 `n` of at most `L32`
   bytes from `skeleton`, then `n` bytes of `custom` where `id` is 0, else of `sections`.

The output must then be `len` bytes, and every stream used up.

## 4. Instructions

### 4.1 Expressions

An **expression** is copied an instruction at a time, keeping a nesting depth `d` from 0 and a
list of locals (§4.3) made anew for it, until its end. An instruction is its opcode, a byte
copied from `op`, then its immediates as the table tells, in order. "a LEB128 of `s`" is one of
at most `L32` bytes copied from stream `s` (`L64` where marked). A byte not in the table is an
error.

| opcode | immediates |
|---|---|
| `02` `03` `04` (block, loop, if) | a LEB128 of `blocktype`; then `d += 1` |
| `0B` (end) | none; the expression ends if `d = 0`, else `d −= 1` |
| `0C` `0D` (br, br_if) | a LEB128 of `label` |
| `0E` (br_table) | a LEB128 `n` of `brtable`, then `n + 1` LEB128s of `brtable` |
| `10` `12` (call, return_call) | a LEB128 of `op` |
| `11` `13` (call_indirect, return_call_indirect) | two LEB128s of `misc` |
| `1C` (select t) | a LEB128 `n` of `misc`, then `n` bytes of `misc` |
| `20` `21` `22` (local.get, local.set, local.tee) | a local index (§4.3) |
| `23`–`26`, `3F`, `40`, `D0`, `D2` | a LEB128 of `misc` |
| `28`–`3E` (loads and stores) | a memory argument (§4.2) |
| `41` (i32.const) | a LEB128 of `op` |
| `42` (i64.const) | a LEB128 of `i64`, of at most `L64` bytes |
| `43` (f32.const) | 4 bytes of `bytes` |
| `44` (f64.const) | 8 bytes of `bytes` |
| `00` `01` `05` `0F` `1A` `1B` `45`–`C4` `D1` | none |
| `FC` | a LEB128 `x` of `op`, then by `x`: 0–7 none; 8, 10, 12, 14 two LEB128s of `misc`; 9, 11, 13, 15–17 a LEB128 of `misc`; else an error |
| `FD` | a LEB128 `x` of `op`, then by `x`: 0–11, 92, 93 a memory argument; 12, 13 16 bytes of `bytes`; 21–34 a byte of `bytes`; 84–91 a memory argument then a byte of `bytes`; 14–20, 35–83, 94–255 none; else an error |

### 4.2 Memory arguments

A LEB128 `a` of `op`; if bit 6 of `a` is set, a LEB128 of `misc`; then the offset, a LEB128 of
at most `L64` bytes whose first byte is copied from `op` and, where that byte has bit 7 set,
the others (at most 9) from `offset`.

### 4.3 Local indices

A local index is coded by a list of indices (at most 256, the last used first) and `next`, the
greatest index used so far plus 1 (0 at first), both of the expression. A LEB128 `c` of at most
`L64` bytes is read from `local` (not copied):

- `c` < the list's length: the index `i` is the list's `c`-th (from 0);
- else `i = next + unzz(c − length)`, which must be in [0, 2³²).

Write `i` to the output (in the shortest form). Then `i` is taken out of the list where it is
there, put at its front, and the list cut to 256; `next` becomes `max(next, i + 1)`.

## 5. Code and data sections split

The section is written as its `id`, its size (written, §1) and its content:

1. Copy a LEB128 `count` of at most `L32` bytes of `skeleton`.
2. `count` times, an item:
   - **code** (`id` 10): read a byte `f` of `skeleton` (not copied).
     - `f = 0`: a function written as its size and its body. The body: copy a LEB128 `n` of
       `locals`, then `n` times a LEB128 of `locals` and a byte of `locals`; then an expression
       (§4.1).
     - `f = 1`: a function kept as it is: copy a LEB128 `n` of at most `L32` bytes of
       `sections`, then `n` bytes of `sections`.
     - Anything else is an error.
   - **data** (`id` 11): a segment: a LEB128 `kind` of `misc`; if `kind` is 2, a LEB128 of
     `misc`; unless `kind` is 1, an expression (§4.1); then a LEB128 `n` of `misc` and `n`
     bytes of `data`.

The sizes written are those of the bytes of the content and of the body, and must be below
2³².

## 6. Validation

The module fails where there are params or resources, there are not exactly 14 inputs, the
header is not the one of §3, a stream ends early or has bytes left, a LEB128 runs past its
bound, an opcode or a `k` or `f` byte is not one of those given, a local index falls outside 32
bits, a size reaches 2³², or the output is not `len` bytes.

## 7. Encoding *(encoder)*

`aqr_wasm::formula::encode` takes a module starting with the header of §3 whose sections end
where their sizes tell, in it (or `None`). It splits the code and data sections whose sizes are
in the shortest form, and of the code section each function whose size is, whose instructions
the walk of §4.1 takes to the end of its body exactly, and whose local indices are in the
shortest form. Where a section or a function cannot be split, its streams are as they were
before it and it is kept as it is. The walk takes any byte the table allows, whether WebAssembly
does or not; the module rebuilds it the same.

The data segments' bytes and the custom sections are content (`Stream::content`), for
aqr-compress to find files in. aqr-compress finds modules by their header, anywhere in data, to
the end of their last section of an id up to 13 that ends within the data.

Why the streams are so (zstd -22 of each module alone, the streams one after another after
their lengths in a frame, on the 55 modules of aqr-modules then, 3.57 MB, 1,016,872 B as
they are):

- The fields apart: 903,580 B (0.889).
- Local indices by their ranks: 884,554 (0.870). The same fragment of code in two functions
  numbers its locals differently, and the same by their ranks.
- The fields that act as opcodes in the opcode stream: the memory arguments' flags (0.861;
  mostly the natural alignment, then as of a longer opcode), the constants of `i32.const` and
  the functions of `call` (0.855 with them). The offsets' first bytes in it and their other
  bytes apart: 0.843 (all of an offset in the opcode stream is worse than all apart). Labels,
  local indices, block types and `i64.const` are better apart.
- An index used for the first time coded from the one after the greatest so far rather than as
  itself: 0.8414 against 0.8426. A list of 256 loses nothing to one unbounded; 64 gives 0.8422,
  16 0.8468, 8 0.8595.
- An adaptive order-1 to order-4 context model of the opcodes alone (bitwise, mixed) codes them
  in 604 KB at best against zstd's 280 KB: the code repeats at long distances (inlined and
  monomorphized copies), which LZ77 takes, and a coder of its own would need a match model.
