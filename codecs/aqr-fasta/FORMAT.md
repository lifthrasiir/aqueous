# The `fasta-recons` module

This document specifies the module `fasta-recons` of aqr-formula entry formulas. The module
rebuilds a FASTA file of nucleotide sequences from its parts: the headers, the layout of the
lines, the case of the letters, the letters other than A, C, G and T, and the bases A, C, G and
T coded by an adaptive model. The document is meant to be complete: an implementation of the
module can be written from it alone. Sections marked *(encoder)* describe the choices this
implementation makes when it builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Parts](#3-parts)
4. [Bases](#4-bases)
5. [Rebuilding the file](#5-rebuilding-the-file)
6. [Encoding](#6-encoding-encoder)

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first, at most
  10 bytes. A writer always uses the shortest form.
- `LF` is the byte 0x0A. A *letter* is an ASCII letter (`A`–`Z`, `a`–`z`); its upper case is
  the letter with bit 0x20 cleared.
- The *bases* are the letters A, C, G and T, case aside, numbered 0, 1, 2 and 3.

## 2. The module

- **Module**: name `fasta-recons`, version 1.
- **Params**:

  ```text
  varint  W        the usual length of a sequence line
  u8      final    1 if the file ends with LF, else 0
  ```

  Nothing else may follow.
- **Resources**: none.
- **Inputs**: five, in order: `headers`, `layout`, `case`, `other` (§3) and `bases` (§4).
- **Output**: the file of the declared length (§5). Any inconsistency (a part ending early
  or with bytes left, a length other than the declared one) is an error.

## 3. Parts

A FASTA file here is a sequence of *records*. A record is a header line (`>` and any bytes but
LF) and zero or more sequence lines (letters only). Lines are separated by LF; the file ends
with LF or not (`final`). The *sequence* of the file is the letters of all sequence lines of
all records, in order.

- **headers**: of each record, its header line without the `>`, followed by LF.
- **layout**: of each record, in order:

  ```text
  varint  N        the letters of the record
  u8      kind     0: the lines are regular, 1: they are given
  if kind = 1:
    varint  n      the lines
    varint  len    each line's letters, n times; they add up to N
  ```

  A regular record of N letters has no lines if N = 0, else ⌈N / W⌉ lines, all of W letters but
  the last, of N − (⌈N / W⌉ − 1) · W. W = 0 with a regular record of letters is an error.
- **case**: the runs of the sequence's letters of one case, alternately upper and lower, from an
  upper one (which may be of length 0), as varints. They add up to the sequence's length. Only
  the first run may be of length 0.
- **other**: the runs of one letter other than the bases (case aside), in order, each:

  ```text
  varint  gap      the bases in the sequence before it since the end of the run before
  varint  len      its letters, at least 1
  u8      c        its letter, in upper case, not a base
  ```

  The sequence is the bases (§4) with these runs put in where `gap` tells; bases follow the last
  run to the end.

## 4. Bases

The bases of the sequence, B in number (the sequence's letters but those of the runs of
`other`), are coded by an adaptive context model and a range variant of ANS.

**Model.** The context of base i is the number of the 9 bases before it, the most recent in the
lowest 2 bits: `h_i = (h_{i-1} · 4 + b_{i-1}) mod 4^9`, `h_0 = 0`. Each of the 4^9 contexts has
four 16-bit probabilities `p[0..4]`, all 16384 at the start. Base b in context h is coded with
the frequencies

```text
f[a] = p[a] >> 4  for a = 0, 1, 2;    f[3] = 4096 − f[0] − f[1] − f[2]
```

and the starts `s[a] = f[0] + … + f[a−1]`; after it, the context's probabilities become

```text
p[a] = p[a] − ((p[a] − 64) >> 6)  for every a;    p[b] = p[b] + 65536 − (p[0] + p[1] + p[2] + p[3])
```

(every `p[a]` stays at least 64, and they add up to 65536).

**Coder.** A 32-bit rANS of 12-bit frequencies with byte-wise renormalization, its state x in
[2^23, 2^31). The input `bases` is the state at the start, as a big-endian u32, then the bytes
it reads, in order. Decoding a base:

```text
slot = x mod 4096
b    = the a with s[a] ≤ slot < s[a] + f[a]
x    = f[b] · (x >> 12) + slot − s[b]
while x < 2^23: x = x · 256 + the next byte
```

After the B bases, x must be 2^23 and every byte of `bases` read; else it is an error.

## 5. Rebuilding the file

For each record, in order: unless it is the first, LF; then `>`, its header (the next one of
`headers`, up to its LF); then each of its lines: LF, and the line's letters, the next ones of
the sequence (§3), each in the case of its run. After the last record, LF if `final` is 1.
Every part must be read to its end: the headers (but the empty piece after the last LF), the
layout, the case, the other runs and the bases.

## 6. Encoding *(encoder)*

- A file is taken where it starts with `>`, has no CR byte, every sequence line is of letters,
  and its runs of other letters are at most one per 64 letters, and 16 (nucleotides; protein
  sequences are left as they are).
- W is the most common length of a sequence line (the longer of those as common).
- The probabilities are those the model gives in the order of the bases; the coder writes the
  bases backward, so that they decode forward.
- The other parts are as they are, for the archive to compress; the bases are of
  `Kind::Compressed`.
