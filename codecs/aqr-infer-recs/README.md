# aqr-infer-recs

Records inferred in files whose format nothing knows (a header, entries, a footer), and their
fields transposed for a general-purpose coder. It is the port of `scripts/rec_probe.py`, the
prototype; `scripts/rec_oracle.py` measures the prototype against the layouts of known formats.

- **Tables of fixed-size records** ([`fixed`](src/fixed.rs)): a record size k per window, by
  the code length of k adaptive order-0 column models against one; the window grown into a
  region by a maximum-sum run of the columns' log-ratio, and again with the records coded by
  fields (a sorted table ends where its deltas stop fitting).
- **Entries with a length field** ([`chain`](src/chain.rs)): chains of length fields followed
  from every field position in spans of the file, for each width, endianness and mask, to the
  next fields where the bytes around them are most like these (and by the rules that put
  them there: b and alignment); scored by how consecutive entries agree column by column
  against a control; the best followed through the file, extended backwards, rotated (a chain
  fixes entries only up to where they start) and trimmed. Each entry's head (a width chosen
  by zstd) is a record; its tail follows.
- **Fields** ([`fields`](src/fields.rs)): DP over the record, each field raw, a delta or xor
  against the record before, or a difference or xor against an earlier field, costed by
  adaptive order-0 models on a sample of the records. A region is written as its fields'
  planes (or its bytes transposed), then the tails.
- **Choice** ([`choose`](src/lib.rs)): each candidate measured alone under zstd -3 (on itself
  and 64 KiB around it, on its first MiB), the set apart with the largest sum of gains by weighted interval
  scheduling, then each checked in context.

## Status

A library and a bench; no module and no formula yet, so no `FORMAT.md`. `Region::restore`
inverts a form from what a decoder would know (the shape, the count, the fields).

The inference takes 0.03 to 1.3 s a file (on one thread), mostly a constant: the window scan
and the growth of the regions found (`fixed`), and the chain search.

## Giving up early

[`probe::estimate`](src/probe.rs) estimates in 3–100 ms what the inference may save: 48
windows of 8 KiB over the file, each at the record size its bytes repeat at, compressed as
they are, transposed and split into fields. A saving of 10% of a file needs records over much
of it, which the windows find; [`infer_if`](src/lib.rs) infers only where the estimate is at
least a given saving. The estimate is low where the saving is small (it misses small tables
and chains of entries), so a threshold gives up some of those:

| estimate at least | files inferred | zstd -3 of the files | CPU s |
|---|---|---|---|
| (always infer) | 17/17 | 0.954 | 8.08 |
| 1/32 | 8/17 | 0.975 | 3.87 |
| 0.05 | 6/17 | 0.981 | 2.80 |
| 0.10 | 5/17 | 0.984 | 1.91 |

(One thread; the oracle corpus and six more files: the prototype's struct.bin and pcap.bin,
macOS's DataDetectors cache, two ResponseKit files and SFNS.ttf. The five files saving 10%
or more, 13–58%, are estimated at 6.2–50%, all above 0.05; no other is above 7.5%.)

## Results

On an M4, `aqr-infer-recs-bench` on 11 files (114.6 MB: ELF and Mach-O binaries, a font, a
pcap of tcpdump, git's index and pack index, ld.so.cache), 10 threads, CPU seconds of each
file's thread:

| | CPU s | zstd -3 of the files transformed |
|---|---|---|
| `scripts/rec_bench.py` (numpy, 10 processes) | 605 | 0.967 |
| this crate, as ported (85a8610) | 106.9 | 0.967 |
| this crate | 7.1 | 0.968 |

On one thread, 5.3 s (21.5 MB/s); the five files over 5 MB at 27.9 MB/s (the 58.7 MB ELF at
53 MB/s), a 1.7 MB libc at 3.7 MB/s. Peak RSS 527 MB (the prototype's: 4.8 GB).

Fewer candidates do not pay: at most 32 regions of fixed-size records (two from each seed);
16 or 8 save 10% or 19% of the time and give 0.974 or 0.976 (python3.12 0.943 -> 0.956,
0.969), one region a seed 6% and 0.974.

## Usage

```sh
cargo build -r -p aqr-infer-recs --examples
target/release/examples/aqr-infer-recs-bench [-j THREADS] [-v] [-t] FILE...  # -t: time of each step
```
