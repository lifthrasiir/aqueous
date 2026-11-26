# aqr-log

Lines of text (logs, tables of numbers between spaces) rebuilt bit for bit from a template for
each line and its fields stored column by column: timestamps, numbers and IPv4 addresses are
coded as integers ([`aqr-num`](../../crates/aqr-num)), words are kept as they are for aqr-wrt.
`FORMAT.md` specifies the module `log-recons`, and how it works (`docs/log-design.md` has the
reasons and the prototype).

```rust
use aqr_log::formula::{self, MODULE};
// None where the file is not lines this crate takes.
let f = formula::encode(&log, 1 /* threads */).unwrap();
// `f` rebuilds `log` when evaluated with `formula::decode` as the module `MODULE`.
```

aqr-compress takes text as lines of templates where its first lines are such (`formula::starts`),
and only where the streams of a sample (the file up to 2 MiB, else 8 windows of 256 KiB from its
start to its end), compressed by zstd -3, are no larger than the sample
itself (prose and source code come to 120% and more), and then against aqr-xsv by the same
measure (the smaller of the two); everything else of text goes to aqr-jsonl, aqr-xsv or
aqr-wrt as before. The streams of words, of templates and of the lines kept are text, which
aqr-wrt takes in turn. Without the default feature `encode`, the crate is the module's code
alone (`module`), which aqr-modules builds into the Wasm module `log-recons`.

## What is taken

Text whose first lines are such (logs, tables of numbers, CSV with decimal commas), of which the
streams of a sample (the file up to 2 MiB, else 8 windows of 256 KiB spread from start to end),
compressed by zstd -3, are no larger than the sample itself (prose and source code come to 120%
and more) and, against aqr-xsv by the same measure, smaller. Lines that cannot be so (a byte 0x01
to 0x05, or past the limits) are kept as they are, so any file round-trips. Not taken: prose and
code (aqr-wrt's).

## Results

Measured on 2026-11-14 on an Apple M4, through `aqr` (archived bytes at the default level;
before = the same without this crate's tokens at `;`, TAB and `,` and without aqr-num's numbers
and times):

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

`aqr-log-bench` gives the sizes of the streams (as zstd -3 compresses them), speeds and the round trip.

## Tests

```sh
cargo rt -p aqr-log
```

- Unit tests: round trips of edge cases (no last LF, CR LF, empty lines and tokens, numbers of
  many writers, marks and zero bytes in lines, runs of more than 18 digits), the templates made,
  the limits (more templates than a segment holds), predictors, segments at several lengths,
  files not taken, and damaged streams and params (errors, never panics).
- `log_plans_hold` in aqr-decompress's `tests/wasm.rs` checks the module's memory plan to the page.

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module's formula of a file: templates, columns, predictors (encoder) |
| `src/module.rs` | Params, limits, and the file rebuilt from its streams (the module's code), its plan |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Left

- **Epoch times.** Epoch seconds and milliseconds (`1704067200000`) stay numbers, not times.
- **IPv6.** IPv6 addresses, and an address with its port as one integer; the same units and
  addresses in aqr-xsv.
- **Clusters.** Fields at the positions of words (Drain's clusters) beyond the rare-form rule.
