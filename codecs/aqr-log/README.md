# aqr-log

Lines of text (logs, tables of numbers between spaces) rebuilt bit for bit from a *template* for
each line and its fields stored column by column. A line is cut into tokens at its spaces; a
token that is a whole number ([`aqr-num`](../../crates/aqr-num): `-0.05`, `12.50`, `1.5E-07`) is a
slot of the template coded as an integer and how it was printed; a token whose form (its digit
runs masked) is rare in the file is a word kept as it is in its column; any other token stays in
the template, its digit runs slots of integers (zero padding in a width byte). A column is the
values of one slot of one template, each number as it is, as the change from the column's
previous one, or as the change from the same slot of the line before, whichever zstd packs
smaller. Lines that cannot be so (a byte 0x01 to 0x03 in them, or past the limits) are kept as
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

Measured on 2026-11-14 on an Apple M4, one thread, with `aqr-log-bench` (the streams as zstd -3
compresses them, against the file through zstd -3); the numbers through `aqr` are in the commits
that made them.

| file | MB | zstd -3 | streams | ratio | encode | decode |
|---|---|---|---|---|---|---|
| openssh.log (loghub) | 73.42 | 5,135,097 | 947,259 | 0.184 | 112 MB/s | 795 MB/s |
| apache_error.log (loghub) | 5.14 | 269,952 | 172,900 | 0.640 | 42 MB/s | 757 MB/s |
| nasa_access_jul95.log | 205.24 | 20,981,215 | 14,066,475 | 0.670 | 84 MB/s | 527 MB/s |
| intel_lab_sensors.txt | 150.91 | 38,583,429 | 18,348,227 | 0.476 | 62 MB/s | 359 MB/s |

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
| `src/module.rs` | Params, limits, and the file rebuilt from its streams (the module's code), its plan |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Next steps

- Times as numbers (`Dec 10 06:55:46`, `[01/Jul/1995:00:00:01 -0400]`, `00:59:16.02785`): a
  pattern and an integer (docs/log-design.md §4); the same in aqr-xsv.
- Fields at positions of words (Drain's clusters) beyond the rare-form rule.
- The decimals of a token inside a word (`4.216;0.418`) as whole numbers; a table of `;` or
  commas is aqr-xsv's.
