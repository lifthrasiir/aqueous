# aqr-num

Numbers printed as ASCII text, stored as an integer plus how they were printed, and
printed back bit for bit. Written for aqr-obj (aqr-xsv uses it too), independent of OBJ: any container
that keeps a two-byte descriptor and an integer at a scale per number can use it. Rendering
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

`mode` = render | source << 4 | comma << 6; bit 7 is zero. Valid pairs (1204 in all, twice
those below: each with or without the decimal comma):

| render | | sources | flags `f` |
|---|---|---|---|
| 0 | fixed `%.Nf` | decimal | bits 0-4 N (0..=18), 5 point, 6 no integer part, 7 negative zero; see below |
| 1 | `%.Pg` | any | bits 0-4 P (1..=17), 5 three-digit exponents, 6 negative zero |
| 2 | `%.Pe` | any | as `%g`, P in 1..=19 |
| 3 | Python `repr` | any | 0, or 64 (negative zero) |
| 4 | JavaScript `toString` | any | 0 or 64 |
| 5 | positional (Rust `Display`) | any | 0 or 64 |
| 6 | padded fixed `%.Nf` | decimal | as fixed |

Sources: 0 decimal (the value is `D`), 1 `f32`, 2 `f64`.

Decimal comma: with mode bit 6 (0x40) set, the token has a comma in the place of its decimal
point: every `.` that the layouts below print (a fixed token's point, `%g`'s, `%e`'s and the
positional and shortest forms', `repr`'s `.0`) is `,` instead (`4,216`, `-0,05`, `1,5e-07`,
`1,0`). Nothing else changes: no digit grouping, the exponent and sign as they are, the value
the same. A mode with bit 7 set is invalid.

Fixed format flags: the point is present if bit 5 is set, else N = 0 and bit 6 is clear;
bit 6 needs N > 0.

### Fixed

`D` = `v` × 10^-N (`s` is not used). The token is `-` if `v` < 0, or `v` = 0 with bit 7;
then the decimal digits of |`v`|, left-padded with zeros to N + 1 digits (N with bit 6);
the point (bit 5) goes before the last N.

### Padded fixed

The same tokens as fixed with N = bits 0-4, but the integer `v` is at the scale `s`, not at N:
`D` = `v` × 10^-`s`. The token is as fixed prints the digits of `v` followed by max(0, N − `s`)
zeros, with max(N, `s`, 0) digits after the point (more than N where `s` > N, a token that
has a point then also with bit 5 clear; with `s` < 0 the integer part gets the extra zeros) and
the other flags as in fixed. An encoder codes a fixed token with `s` the scale of its last
nonzero digit within the fraction, so that `42283.58000000` is `v` = 4228358, `s` = 2, N = 8:
the zeros a writer pads are not in the integer.

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

A comma mode (`dec::COMMA`) codes only tokens with no point: the token with its commas as
points is coded as the mode without the bit says, and kept if it prints back (with the comma)
to the same bytes. `style::choose` picks the comma modes where more tokens (with a digit) of
the sample have a comma than a point (a tie goes to points), choosing from the tokens with
points; `Style::encode` of such a style gives the comma modes, fixed included, and no mode to
a token with a point.

`dec::encode` codes a token in a given mode when it prints back to the same bytes. The
scale is that of `D`'s last digit, except for decimal `%g` and `%e`, where it is P-1-`E`
(it changes with the magnitude only); a zero gets the scale passed in (the previous
number's, so that the change costs nothing). Tokens of more than 18 or 19 significant
digits, or with a scale outside -60..=60, are not coded. `Style::encode` tries the
group's mode, then fixed, then `%g`, then `%e` and the shortest forms for tokens with an
exponent. `style::choose` picks the mode of a group from a sample: the one with the
fewest digits and scale changes over the first 512 tokens.

## Times

A time printed by a *pattern* is one integer `v` (an `i64`) of *ticks*; a container keeps the
pattern's bytes and the integer, `time::valid(pattern)` checks a pattern read from untrusted
input, and `time::put(out, pattern, v)` prints (false: `v` is not of a time the pattern can
print, a decoding error, never a panic).

### Patterns

A pattern is bytes, read from the start as items:

| item | prints |
|---|---|
| `Y` | the year, 4 digits (0000 to 9999) |
| `m` | the month, 2 digits (01 to 12) |
| `b` | the month's English name abbreviated: `Jan` `Feb` `Mar` `Apr` `May` `Jun` `Jul` `Aug` `Sep` `Oct` `Nov` `Dec` |
| `d` | the day, 2 digits (01 to 31) |
| `e` | the day, 2 characters: a digit with a space before it for 1 to 9 (`Dec  1`) |
| `D` | the day, with no padding (1 to 31) |
| `a` | the weekday's English name abbreviated: `Mon` `Tue` `Wed` `Thu` `Fri` `Sat` `Sun` |
| `H` | the hour, 2 digits (00 to 23) |
| `h` | the hour, with no padding (0 to 23) |
| `M` | the minute, 2 digits (00 to 59) |
| `S` | the second, 2 digits (00 to 59) |
| `f` *p* *n* | a fixed fraction: the point byte *p* (`.` or `,`) and the *n* digits (*n* a digit `1` to `9`) of the fraction of a second, with zeros to *n* digits |
| `t` *p* *n* | a trimmed fraction: as `f`, but the trailing zeros of the *n* digits are dropped, and nothing at all, not even *p*, is printed where all are zeros |
| `\` *c* | the byte *c* (any but 0x01 to 0x04 and LF) |
| *c* | the byte *c*, any but an ASCII letter, `\`, 0x01 to 0x04 and LF |

Any other pattern is invalid: an ASCII letter that is no item (or `f`, `t` not followed by
a point and a digit `1` to `9`), a byte 0x01 to 0x04 or LF, and a combination that is no time:

- each of: `Y`; `m` or `b`; `d`, `e` or `D`; `a`; `H` or `h`; `M`; `S`; a fraction (`f` or
  `t`), at most once;
- a date is `m` or `b` with `d`, `e` or `D` (with `Y`, or not, and with `a`, or not); the year,
  month, day, or weekday alone is no date;
- the time of day is `H` or `h`, then `M` (not without the hour), then `S` (not without
  `M`), then a fraction (not without `S`): each may end it;
- at least a date or an hour;
- at most 64 bytes (what is printed is then at most 190).

The pattern prints, in the order of its items, the fields of an instant of the proleptic
Gregorian calendar, whose weekday is derived from its date (a pattern with `a` prints the
weekday the date has). Dates without a year are of the year 2000, a leap year.

### The integer

With *n* the digits of the fraction (0 without one), the ticks are those of the finest unit
the pattern has: *days* since 1970-01-01 (a date without a time of day), else with *days* the
days of the date (0 for a pattern with no date)

```
ticks = (((days × 24 + hour) × 60 + minute) × 60 + second) × 10^n + fraction
```

with the factors of the units the pattern has only: a pattern of hours and minutes counts
minutes (`(days × 24 + hour) × 60 + minute`), of an hour hours, of a date alone days. The
fraction is the digits as an integer of *n* digits (a trimmed one of fewer digits is padded
with zeros on the right to *n*). A tick is a day, an hour, a minute, a second or 10^-*n* of a
second.

To print, the integer is divided by those factors, from the last, flooring (the remainders are
the fraction, second, minute and hour; the hour counted modulo 24 and so on); what is left is
the days (those of the integer, for a pattern with a date). The integer cannot be printed,
and `put` says so, if: the days are not 0 for a pattern without a date (a time of day before
midnight or after the end of the day); the date is before year 0 (-719528 days) or after
year 9999 (2932896); the pattern has no `Y` and the date is not in the year 2000; or the
pattern is invalid. Seconds are 00 to 59: there are no leap seconds.

### Encoder conventions (not normative)

`time::parse(pattern, text)` gives the integer of the time that `text` starts with and its
length, if it is a valid date whose weekday (if the pattern has `a`) is right; the encoder
codes it where `put` prints the same bytes (a trimmed fraction does not print a trailing
zero, nor a fixed one fewer digits).

## Testing

```sh
cargo test --release
```

Times: the calendar against every day of years 0 to 9999, the patterns of the writers in
the corpus, the values printed back and the ones that are not. `%g` of decimals against the C library, exact digits against the standard library,
shortest digits against the standard library (with the ties above), every valid
descriptor rendering random values (as from corrupt input) and coding back, writer
samples, the encoder's choices.

## Layout

| File | Contents |
|---|---|
| `src/fixed.rs` | Fixed and padded fixed tokens, scales |
| `src/general.rs` | `%g` of decimals |
| `src/dec.rs` | Decimals, floats, renders, coding a token in a mode |
| `src/style.rs` | Mode of a group of numbers, fallbacks (encoder) |
| `src/time.rs` | Times by a pattern: calendar, `put` and `parse` |
