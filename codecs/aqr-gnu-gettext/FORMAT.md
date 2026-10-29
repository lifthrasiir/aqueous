# aqr-gnu-gettext format

This document specifies the module `gnu-gettext-recons` of aqr-formula entry formulas, which
rebuilds a GNU gettext message catalog (a `.mo` file, as the GNU gettext manual's "The Format
of GNU MO Files" describes it) from the file without its tables. It is meant to be complete:
the module can be implemented from it alone. Sections marked *(encoder)* describe the choices
this implementation makes when it builds formulas; the module does not depend on them.

## 1. Conventions

- Integers are unsigned. A **word** is 32 bits, little-endian or big-endian as the file's
  magic tells (§2).
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes.

## 2. The file

A `.mo` file starts with a header of 7 words: the magic `950412de`, the revision, `N` (the
strings' pairs), `O` and `T` (the offsets of the tables of the originals and of the
translations), `S` and `H` (the size and the offset of the hash table). The magic read as a
little-endian word tells the file little-endian; read as a big-endian one, big-endian. Each
table is `N` entries of two words, a string's length and its offset; the hash table is `S`
words. The strings are anywhere in the file, each followed by a NUL; a string holds NULs
where it is of several (plural forms). Anything else in the file (the header's other words
after the 7th, system-dependent strings) is whatever it is.

The module requires the tables in order and apart: `O ≥ 28`, `T ≥ O + 8N`, and, where `S` is
not 0, `S ≥ 3` and `H ≥ T + 8N`. The **parts derived** are the originals' table (`8N` bytes at
`O`), the translations' (`8N` at `T`) and the hash table (`4S` at `H`; none where `S = 0`).

## 3. The module

- **Module**: name `gnu-gettext-recons`, version 1.
- **Params**: `varint start`, where the strings start; nothing after it.
- **Resources**: none.
- **Inputs**: exactly 2: `rest` (class `gnu-gettext/rest`), the file without the parts
  derived; `nuls` (class `gnu-gettext/nuls`), for each of the `2N` strings, the originals in
  order then the translations, a varint, the NULs within it.
- **Output**: the file, `len` bytes (the declared length).

## 4. Decoding

1. Read the header (§2) from the start of `rest`; it must be valid as §2 requires, and
   `len` must be the length of `rest` plus the bytes of the parts derived, and not less than
   the end of any of them.
2. `out` is `rest` with the parts derived put in at their offsets, in order, as zero bytes.
3. The strings, originals then translations: the first at `start`, each at the byte after the
   one before ends. A string at `p` with `k` NULs within it (from `nuls`) ends at the
   `(k + 1)`-th NUL of `out` from `p` (which must exist); its length is the bytes from `p`
   to that NUL. A string must not overlap a part derived. `nuls` must be used up exactly.
4. Each table's entry `i` is the `i`-th original's (or translation's) length and offset, as two
   words.
5. Where `S > 0`, the hash table is GNU msgfmt's (below), as `S` words.

The **hash** of a string is GNU gettext's `hash_string` of its bytes up to its first NUL: `h =
0`; for each byte `c`, `h = (h << 4) + c` (32 bits, wrapping), and if `g = h & F0000000` is
not 0, `h ^= g >> 24` then `h ^= g`. The **hash table** has `S` slots of 0, and `N` must be
below `S`. For each original `i` in order, with `h` its hash: the slot is `h mod S`, and while
it is not 0 the next is `slot + 1 + (h mod (S − 2))`, less `S` where that is `S` or more. Slot
gets `i + 1`.

## 5. Validation

The module fails where the params are not one varint, there are resources or not exactly two
inputs, the header is not valid (§2), the lengths do not agree (§4.1), a string runs out of
NULs or overlaps a part derived, `nuls` has a bad varint or is not used up, or `N ≥ S > 0`.

## 6. Encoding *(encoder)*

`aqr_gnu_gettext::formula::encode` takes a file whose header is valid (§2), whose parts derived
are within it, and whose strings lie one after another from the first original's offset in
the order of §4.3, each followed by a NUL; it takes `start` as that offset, and the formula
only where the module gives the file back (the hash table being msgfmt's, among others).
aqr-compress finds `.mo` files by their header (anywhere in data), and takes a file to the end
of its tables or of its last string, whichever is later.

Why this layout (measured on the 1,170 distinct `.mo` files of the 10 GB corpus, 79.7 MB; zstd
-3, all in one): the tables are 11.0 MB, 4.30 MB under zstd, and the hash tables 3.69 MB, 1.04
MB; all the files lay their strings out so and have msgfmt's hash tables. The strings'
lengths alone as varints are 0.77 MB under zstd, and their NULs (none but in 2,560 of 1.38
million strings) next to nothing.
