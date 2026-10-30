# aqr-bcj modules

aqr-bcj's modules apply the encoders of the filters of xz and 7z: the branch converters
(BCJ), Delta and 7z's BCJ2. A container whose stream went through a filter before its
compressor (an `.xz` block of `x86` then LZMA2, a 7z folder of BCJ2 and LZMA) keeps the data
before the filter, and the module gives back what the compressor was given. The document is
normative: an implementation of the modules can be written from it.

External reference: the LZMA SDK (Igor Pavlov, public domain): `Bra.c`, `Bra86.c`,
`BraIA64.c`, `Delta.c` and `Bcj2.c`, of which the filters below are what one call of the encoder
on the whole data does; xz's filters (`xz-file-format.txt`) are the same.

## Conventions

- Arithmetic on `u32` is wrapping (modulo 2^32); `>>` and `<<` are shifts of `u32`, bits
  shifted out dropped. `le16`, `le32`, `be32` read (and write) the bytes at a position.
- `D` is the input, `N` its length, converted in place; positions are byte offsets in it.
  `pc` is the start address (the module's parameter, else 0).
- "Converting" a value `v` by `c` is `v + c` (the module encodes).

## Branch converters

Modules `bcj-x86`, `bcj-powerpc`, `bcj-ia64`, `bcj-arm`, `bcj-armthumb`, `bcj-sparc`,
`bcj-arm64`, `bcj-riscv`, each version 1. **Parameters**: none, or the start address `pc`
(le32), whose bits of the architecture's alignment must be 0: 1 for ARM Thumb and RISC-V, 15
for IA-64, 3 for the others, none for x86. **Inputs**: one, the data. **Output**: the data
converted, of its length. **Resources**: none.

**x86.** `mask = 0`, `pos = 0`, `ip = pc + 5`; nothing if `N < 5`. Repeat:

```text
p = the first position ≥ pos below N − 4 with D[p] & 0xFE = 0xE8; none: stop
d = p − pos;  pos = p
if d > 2: mask = 0
else:
  mask >>= d
  if mask ≠ 0 and (mask > 4 or mask = 3 or ms(D[p + (mask >> 1) + 1])):
    mask = (mask >> 1) | 4;  pos += 1;  continue
if ms(D[p + 4]):
  v = le32(D, p + 1);  c = ip + pos;  pos += 5;  v += c
  if mask ≠ 0:
    sh = (mask & 6) << 2
    if ms((v >> sh) & 0xFF): v ^= (0x100 << sh) − 1;  v += c
    mask = 0
  D[p + 1 .. p + 4] = the low 3 bytes of v, le;  D[p + 4] = 0 − ((v >> 24) & 1) (a byte)
else:
  mask = (mask >> 1) | 4;  pos += 1
```

where `ms(b)` is whether `b` is 0x00 or 0xFF.

The others look at instructions at `i` = 0, 4, 8, … while `i + 4 ≤ N` (ARM Thumb: halfwords,
IA-64: bundles of 16 bytes, RISC-V below):

- **PowerPC** (`be32`): where `v & 0xFC000003 = 0x48000001`: `v = ((v + pc + i) & 0x03FFFFFF)
  | 0x48000000`.
- **ARM** (`le32`): where `D[i + 3] = 0xEB`: `v = ((v + ((pc + i + 8) >> 2)) & 0x00FFFFFF) |
  0xEB000000`.
- **ARM Thumb**: at `i` = 0, 2, … while `i + 4 ≤ N & ~1`: where `D[i + 1] & 0xF8 = 0xF0` and
  `D[i + 3] & 0xF8 = 0xF8`: `v = (le16(i) << 11) | (le16(i + 2) & 0x7FF)`, `v += (pc + i + 4)
  >> 1`, `le16(i) = ((v >> 11) & 0x7FF) | 0xF000`, `le16(i + 2) = (v & 0x7FF) | 0xF800`, and
  `i += 4`; else `i += 2`.
- **SPARC** (`be32`): `t = ((v + (5 << 29)) ^ (7 << 29)) + (1 << 22)`; where `t & 0xFF800000 =
  0`: `t = ((t << 2) + pc + i) & 0x01FFFFFF`, `v = ((t − (1 << 24)) >> 2) | (1 << 30)`.
- **ARM64** (`le32`, `a = pc + i`): where `(v − 0x94000000) & 0xFC000000 = 0` (BL): `v = ((v +
  (a >> 2)) & 0x03FFFFFF) | 0x94000000`. Else with `t = v − 0x90000000`, where `t & 0x9F000000 =
  0` and `(t + 2^20) & 0x00E00000 = 0` (ADRP): `t += 2^20`, `z = ((t & 0xFFFFFFE0) | (t >> 26)) +
  ((a >> 9) & ~7)`, `v = (t & 0x1F) | 0x90000000 | (z << 26) | (0x00FFFFE0 & ((z & 0x1FFFFF) −
  2^20))`.
- **IA-64**: `q = (pc − 16) >> 3`. For each bundle at `b` (while `b + 16 ≤ N`): `q += 2`, `m =
  (0x334B0000 >> (D[b] & 0x1E)) & 3`; if `m ≠ 0`, for slots `m`, …, 3, at `p = b + 5m − 4` (5
  more for each next slot): `t = le16(p)`, `z = le32(p + 1) >> m`; where `(t >> m) & 0xE0 = 0`
  and `(z − 0x0A000000) & 0x1E000000 = 0`: `v = z & 0x011FFFFF`, `z ^= v`, `q &= 0x3FFFFF`
  (kept for the next ones), `v = (((v + q) & ~0xC00000) + 0xE00000) & 0x011FFFFF`, `le32(p + 1) =
  (z | v) << m`.
- **RISC-V**: as the LZMA SDK's `z7_BranchConv_RISCV_Enc` (its JAL, and its AUIPC with the
  instruction after it), with `pc`: a scan of halfwords below `(N & ~1) − 6` (none if that is
  not above 0) for JAL (`(le16 ^ 0x10) + 1` of `& 0x77 = 0`, bit 3 clear) and AUIPC (bit 3 set);
  a JAL's 20-bit offset made absolute and its bits reordered; an AUIPC pair whose second
  instruction uses its register made into the register, the opcode and the absolute address
  (big-endian) in the second word, else the other kind of pair swapped likewise.

## Delta

Module `bcj-delta`, version 1. **Parameters**: one byte, the distance `n` less one (1 ≤ `n` ≤
256). **Inputs**: one, the data. **Output**: `D[i] − D[i − n]` for each `i` (bytes, wrapping;
`D[i]` itself for `i < n`).

## BCJ2

Module `bcj2`, version 1. **Parameters**: none. **Inputs**: two: the data `D`, and the stream
of BCJ2's range coder `R`. **Output**: the main, call and jump streams, one after another, of
`N` bytes in all. A container cuts them by their lengths (a Split instruction).

`R` starts with a byte 0, then `code` (be32, not `2^32 − 1`); `range = 2^32 − 1`. A bit of a
probability `p` (11 bits, initially 1024): first, if `range < 2^24`, `range <<= 8` and `code =
code << 8 | ` the next byte of `R` (failing past its end); then `bound = (range >> 11) × p`; 0 if
`code < bound` (`range = bound`, `p += (2048 − p) >> 5`), else 1 (`range −= bound`, `code −=
bound`, `p −= p >> 5`). There are 258 probabilities.

```text
i = 0, prev = 0
while i < N:
  b = D[i];  main += b;  i += 1
  if not (b = 0xE8 or b = 0xE9 or (prev = 0x0F and b & 0xF0 = 0x80)): prev = b; continue
  bit of probability [2 + prev if b = 0xE8; 1 if b = 0xE9; 0 otherwise]
  0: prev = b; continue
  1: fails if i + 4 > N
     a = le32(D, i) + i + 4;  (calls if b = 0xE8, else jumps) += be32(a)
     prev = D[i + 3];  i += 4
```

What follows the last bit in `R` is not read (the container keeps `R` as it is).

**Resources**: none, for every module. A module fails on other parameters or inputs, or an
output length other than its input's (BCJ2: `N`).

## Encoder choices (informative)

- A container takes a filter's data where decoding the filtered data and encoding it again
  gives it back (else it keeps the filtered data, and no module).
- BCJ2's bits are not chosen again: `R` is the archive's, so whatever encoder made it, its
  streams are rebuilt.
