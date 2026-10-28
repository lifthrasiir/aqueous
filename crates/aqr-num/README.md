# aqr-num

Numbers printed as ASCII text, stored as an integer plus how they were printed, and
printed back bit for bit. Written for aqr-obj, independent of OBJ: any container that
keeps a two-byte descriptor and an integer at a scale per number can use it. Rendering
uses IEEE basic operations and integer arithmetic only, so it gives the same bytes on
every platform.

```rust
use aqr_num::style::{Style, choose};
let st: Style = choose(&tokens);                     // (column, token) pairs from a sample
let (v, s, mode, f) = st.encode(tok, prev_scale, &mut tmp).unwrap();  // None: keep as text
aqr_num::put(&mut out, v, s, mode, f);            // out == tok
```

A container stores `(mode, f)` (often once per template or column) and `v`, plus `s`
when the mode is not fixed (aqr-obj codes it as a change from the previous number's).
The decoder needs nothing else; `valid(mode, f)` checks a descriptor read from untrusted
input.

## Specification

A number is a descriptor (`mode`, `f`: two bytes) and a decimal `D` = `v` × 10^-`s`,
`v` an `i64`, `s` in -60..=60. The token depends on the descriptor and `D` only, not on
how `D` is split into `v` and `s`.

### Descriptor

`mode` = render | source << 4. Valid pairs (526 in all):

| render | | sources | flags `f` |
|---|---|---|---|
| 0 | fixed `%.Nf` | decimal | bits 0-4 N (0..=18), 5 point, 6 no integer part, 7 negative zero; see below |
| 1 | `%.Pg` | any | bits 0-4 P (1..=17), 5 three-digit exponents, 6 negative zero |
| 2 | `%.Pe` | any | as `%g`, P in 1..=19 |
| 3 | Python `repr` | any | 0, or 64 (negative zero) |
| 4 | JavaScript `toString` | any | 0 or 64 |
| 5 | positional (Rust `Display`) | any | 0 or 64 |

Sources: 0 decimal (the value is `D`), 1 `f32`, 2 `f64`.

Fixed format flags: the point is present if bit 5 is set, else N = 0 and bit 6 is clear;
bit 6 needs N > 0.

### Fixed

`D` = `v` × 10^-N (`s` is not used). The token is `-` if `v` < 0, or `v` = 0 with bit 7;
then the decimal digits of |`v`|, left-padded with zeros to N + 1 digits (N with bit 6);
the point (bit 5) goes before the last N.

### Values

- Decimal source: `D`; a zero is negative with the negative zero flag.
- `f64`: `D` rounded to the nearest double, ties to even.
- `f32`: that double rounded to the nearest `f32`, ties to even (two roundings, as a
  writer converting a double would; not always `D` rounded to `f32` directly).
- A float source of a zero `D` is -0.0 with the negative zero flag, else +0.0. A value
  out of range prints `inf` (an encoder never codes one).

### Digits

The digits `d1…dn` and the exponent `E` of the first one (value = 0.d1…dn × 10^(E+1)):

- decimal source: those of `D`, without trailing zeros;
- `%g`, `%e` of a float: exactly P significant digits of the float's exact value, rounded
  half to even (as C's `printf` and Rust's `{:.*e}`, checked against the latter); `%g`
  then drops trailing zeros;
- `repr`, `toString`, positional of a float: the shortest digits that round back to the
  value as a double (also for an `f32` source: its widened double), the nearest to it
  among those, ties to an even last digit. This is Python's `repr` and ECMAScript's
  `Number::toString`; the Rust standard library's `{}` and `{:e}` differ on such ties
  (about 0.1% of random doubles). Implemented with Żmij, checked against Python on 4M
  doubles.

Zero has the digit `0` and `E` = 0.

### Layouts

A `-` comes first for a negative value (and a negative zero), except in `toString`.
Exponents are `e`, a sign (`+` or `-`), and at least two digits (three with flag bit 5;
one in `toString`). Positional: for `E` < 0, `0.` then -`E`-1 zeros then the digits; for
`E` < n-1, the digits with a point after the first `E`+1; else the digits and `E`+1-n
zeros.

- `%.Pg`: a zero is `0`; scientific (`d1[.d2…dn]e±XX`) when `E` < -4 or `E` ≥ P, else
  positional. A decimal with more than P digits prints all of them (C would round).
- `%.Pe`: `d1`, then for P > 1 a point and the next P-1 digits (cut, or padded with
  zeros), then the exponent.
- `repr`: positional when zero or -4 ≤ `E` < 16, `.0` added to integers; else scientific.
- `toString`: positional when zero or -6 ≤ `E` < 21; else scientific.
- positional: always.

### Encoder conventions (not normative)

`dec::encode` codes a token in a given mode when it prints back to the same bytes. The
scale is that of `D`'s last digit, except for decimal `%g` and `%e`, where it is P-1-`E`
(it changes with the magnitude only); a zero gets the scale passed in (the previous
number's, so that the change costs nothing). Tokens of more than 18 or 19 significant
digits, or with a scale outside -60..=60, are not coded. `Style::encode` tries the
group's mode, then fixed, then `%g`, then `%e` and the shortest forms for tokens with an
exponent. `style::choose` picks the mode of a group from a sample: the one with the
fewest digits and scale changes over the first 512 tokens.

## Testing

```sh
cargo test --release
```

`%g` of decimals against the C library, exact digits against the standard library,
shortest digits against the standard library (with the ties above), every valid
descriptor rendering random values (as from corrupt input) and coding back, writer
samples, the encoder's choices. aqr-obj's `scripts/format-check.sh` checks that its
output is unchanged.

## Layout

| File | Contents |
|---|---|
| `src/fixed.rs` | Fixed tokens, scales |
| `src/general.rs` | `%g` of decimals |
| `src/dec.rs` | Decimals, floats, renders, coding a token in a mode |
| `src/style.rs` | Mode of a group of numbers, fallbacks (encoder) |
