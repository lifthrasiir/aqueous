# aqr-log

Lines of text (logs, tables of numbers between spaces) rebuilt bit for bit from a *template* for
each line and its fields stored column by column. A timestamp (`Dec 10 06:55:46`,
`[01/Jul/1995:00:00:01 -0400]`, `2004-02-28 00:59:16.02785`, `16/12/2006`) is a slot of the
template coded as one integer by a pattern ([`aqr-num`](../../crates/aqr-num)'s "Times"). The rest of a
line is cut into tokens at its spaces (and, where the first MiB of streams comes out smaller,
at `;`, TABs and commas, but not commas where the numbers have a decimal comma); a token that is a
whole number (`-0.05`, `4,216`, `12.50`, `1.5E-07`, `42283.58000000`, as aqr-num prints them) is a
slot coded as an integer and how it was printed; a token whose form (its digit runs masked) is
rare in the file is a word kept as it is in its column; any other token stays in the template,
its digit runs slots of integers (zero padding in a width byte). A column is the
values of one slot of one template, each number as it is, as the change from the column's
previous one, or as the change from the same slot of the line before (taken at the scale of the
number, which varies in padded fixed and `%g`), whichever zstd packs smaller. A time with a fraction
is two columns, the seconds and the fraction. Lines that cannot be so (a byte 0x01 to 0x04 in them, or past the limits) are kept as
they are, so any file round-trips.

```rust
use aqr_log::formula::{self, MODULE};
// None where the file is not lines this crate takes.
let f = formula::encode(&log, 1 /* threads */).unwrap();
// `f` rebuilds `log` when evaluated with `formula::decode` as the module `MODULE`.
```

aqr-compress takes text as lines of templates where its first lines are such (`formula::starts`),
and only where the streams of its first MiB, compressed by zstd -3, are no larger than the MiB
itself (prose and source code come to 120% and more), and then against aqr-xsv by the same
measure (the smaller of the two); everything else of text goes to aqr-jsonl, aqr-xsv or
aqr-wrt as before. The streams of words, of templates and of the lines kept are text, which
aqr-wrt takes in turn. Without the default feature `encode`, the crate is the module's code
alone (`module`), which aqr-modules builds into the Wasm module `log-recons`.

A file over 64 MiB (`formula::SEGMENT`) is cut at line starts into segments, one application
each, whose templates and columns start afresh (FORMAT.md, "Segments"). The format is specified
in [FORMAT.md](FORMAT.md); `docs/log-design.md` has the reasons and the prototype.

## Results

Measured on 2026-11-14 on an Apple M4, through `aqr` (archived bytes at the default level,
before = the commit before this crate's tokens were cut at `;`, TAB and `,`, and numbers and
times were coded as of aqr-num; the file sizes in the commits that made them):

| file | bytes | before | after | module |
|---|---|---|---|---|
| openssh.log (loghub) | 73,417,506 | 892,530 (1.22%) | 797,722 (1.09%) | log |
| apache_error.log (loghub) | 5,135,876 | 187,002 (3.64%) | 134,924 (2.63%) | log |
| nasa_access_jul95.log | 205,242,368 | 12,449,657 (6.07%) | 11,828,937 (5.76%) | log |
| intel_lab_sensors.txt | 150,910,946 | 17,465,531 (11.57%) | 16,248,071 (10.77%) | log |
| binance_btc_klines_1m.csv | 6,699,071 | 1,496,268 (22.34%) | 1,287,704 (19.22%) | log |
| ghcn_daily_centralpark.csv | 16,277,171 | 441,326 (2.71%) | 384,954 (2.36%) | log |
| binance_btc_trades.csv | 122,428,696 | 10,880,669 (8.89%) | 9,770,213 (7.98%) | xsv -> log |
| household_power.txt | 132,960,755 | 7,166,559 (5.39%) | 6,683,029 (5.03%) | xsv -> log |

`aqr-log-bench` (the streams as zstd -3 compresses them) gives the sizes of the streams.

## Testing

```sh
cargo rt -p aqr-log
```

Unit tests: round trips of edge cases (no last LF, CR LF, empty lines and tokens, numbers of
many writers, marks and zero bytes in lines, runs of more than 18 digits), the templates made,
the limits (more templates than a segment holds), predictors, segments at several lengths,
files not taken, and damaged streams and params (errors, never panics). The module's memory
plan is checked to the page by `log_plans_hold` in aqr-decompress's `tests/wasm.rs`.

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module's formula of a file: templates, columns, predictors (encoder) |
| `src/times.rs` | Timestamps found in a line, with their patterns (encoder) |
| `src/module.rs` | Params, limits, and the file rebuilt from its streams (the module's code), its plan |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Next steps

- The same times in aqr-xsv (whose date and time columns are text).
- Epoch seconds and milliseconds (`1704067200000`) as times, zones (`-0400`) in the pattern.
- Thousands grouping (`1.187,25`, `1,234`) as part of numbers.
- Choosing between a number and its digit runs where a fraction is low in entropy (`,00 ,25
  ,50 ,75`), and the modes of padded fixed, by the same trial as the separators'.
- Fields at positions of words (Drain's clusters) beyond the rare-form rule.
