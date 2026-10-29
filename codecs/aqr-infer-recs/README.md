# aqr-infer-recs

Records inferred in files whose format nothing knows (a header, entries, a footer), and their
fields transposed for a general-purpose coder. It is the port of `scripts/rec_probe.py`, the
prototype; `scripts/rec_oracle.py` measures the prototype against the layouts of known formats.

- **Tables of fixed-size records** ([`fixed`](src/fixed.rs)): a record size k per window, by
  the code length of k adaptive order-0 column models against one; the window grown into a
  region by a maximum-sum run of the columns' log-ratio, and again with the records coded by
  fields (a sorted table ends where its deltas stop fitting).
- **Entries with a length field** ([`chain`](src/chain.rs)): every rule (offset d, width,
  endianness, mask, b, alignment) followed from starts in spans of the file, scored by how
  consecutive entries agree column by column against a control; the best chains followed
  through the file, extended backwards, rotated (a chain fixes entries only up to where they
  start) and trimmed. Each entry's head (a width chosen by zstd) is a record; its tail follows.
- **Fields** ([`fields`](src/fields.rs)): DP over the record, each field raw, a delta or xor
  against the record before, or a difference or xor against an earlier field, costed by zstd
  on a sample of the records. A region is written as its fields' planes (or its bytes
  transposed), then the tails.
- **Choice** ([`choose`](src/lib.rs)): each candidate measured alone under zstd -3 (on itself
  and 1 MiB around it), the set apart with the largest sum of gains by weighted interval
  scheduling, then each checked in context.

## Status

A library and a bench; no module and no formula yet, so no `FORMAT.md`. `Region::restore`
inverts a form from what a decoder would know (the shape, the count, the fields).

The inference is slow: about 100 ms to 10 s per file, most of it a constant. The chain
search tries every rule from every start (29 million lanes on a file of 1.7 MB), and the
window scan computes the code length of each k up to 1024. Both are to be narrowed.

## Results

On an M4, `aqr-infer-recs-bench` on 11 files (114.6 MB: ELF and Mach-O binaries, a font, a
pcap of tcpdump, git's index and pack index, ld.so.cache), 10 threads, CPU seconds of each
file's thread:

| | CPU s | zstd -3 of the files transformed |
|---|---|---|
| `scripts/rec_bench.py` (numpy, 10 processes) | 605 | 0.967 |
| this crate | 106.9 | 0.967 |

Peak RSS 813 MB (the prototype's: 4.8 GB).

## Usage

```sh
cargo build -r -p aqr-infer-recs --examples
target/release/examples/aqr-infer-recs-bench [-j THREADS] [-v] FILE...
```
