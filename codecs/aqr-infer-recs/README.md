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

## Results

On an M4, `aqr-infer-recs-bench` on 11 files (114.6 MB: ELF and Mach-O binaries, a font, a
pcap of tcpdump, git's index and pack index, ld.so.cache), 10 threads, CPU seconds of each
file's thread:

| | CPU s | zstd -3 of the files transformed |
|---|---|---|
| `scripts/rec_bench.py` (numpy, 10 processes) | 605 | 0.967 |
| this crate, as ported (85a8610) | 106.9 | 0.967 |
| this crate | 8.6 | 0.969 |

On one thread, 6.6 s (17.4 MB/s; the 58.7 MB ELF at 44 MB/s, a 1.7 MB libc at 3.6 MB/s).
Peak RSS 579 MB (the prototype's: 4.8 GB).

## Usage

```sh
cargo build -r -p aqr-infer-recs --examples
target/release/examples/aqr-infer-recs-bench [-j THREADS] [-v] [-t] FILE...  # -t: time of each step
```
