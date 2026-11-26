# aqr-infer-recs

Records inferred in files whose format nothing knows (a header, entries, a footer), and their
fields transposed for a general-purpose coder: a table of fixed-size records (ELF and Mach-O
tables, caches, indexes, packet captures, fonts, catalogues of stars) compresses better as its
fields' planes. It is the port of `scripts/rec_probe.py`, the prototype (`scripts/rec_oracle.py`
measures the prototype against the layouts of known formats). `FORMAT.md` specifies the modules
`recs-fields` (records from the byte planes of their fields: a transpose any recompressor may
apply to records of a fixed size), `recs-entries` (entries from their heads and tails) and
`recs-floats` (records' floats from their whole numbers and differences), and how the inference
works. [`module`](src/module.rs) is their code, and [`formula`](src/formula.rs) makes the formula of a
file.

aqr-compress tries it on data of 16 KiB or more in which a scan finds no file, where a quick
estimate (2-50 ms) tells 5% at least, and takes each region that makes the file smaller under
zstd -3.

```sh
cargo build -r -p aqr-infer-recs --examples
target/release/examples/aqr-infer-recs-bench [-j THREADS] [-p INNER] [-v] [-t] FILE...  # -t: time of each step, -p: each file on INNER threads (timed by the clock)
```

## What is taken

- Tables of records of a fixed size, any offset, with fields coded as they are, as deltas or xors
  against the record before or an earlier field, and floats that are whole multiples of a scale
  (numbers written in decimal and stored in other units) as whole numbers and small differences.
- Entries each holding its length (chains), which a formula may hold; aqr-compress does not look
  for them.

## Results

On an M4, `aqr-infer-recs-bench` on 11 files (114.6 MB: ELF and Mach-O binaries, a font, a pcap of
tcpdump, git's index and pack index, ld.so.cache), 10 threads: zstd -3 of the files transformed is
0.968 of the files' zstd -3, in 7.1 CPU s (the prototype in numpy: 0.967, 605 s). On one thread
1.77 s (64.7 MB/s) for 0.974, or 3.57 s for 0.972 looking for chains of entries too (`-c`); peak
RSS 527 MB (the prototype's: 4.8 GB). The inference takes 0.01 to 0.5 s a file on one thread
(0.71 s on one thread for 42.6 MB of an APK's library data, 0.29 s on ten).

The SAO star catalogue's binary (7.25 MB: 258,997 stars of 28 bytes, right ascension and
declination as f64 radians, proper motions as f32 radians a year) is 0.432 of zstd -3 inferred
(0.856 without floats); `aqr -c` 5,272,411 -> 2,356,779 B (xz -9e: 4,425,672).

On the 10 GB corpus aqr-compress makes 17,716 estimates (3.1 GB of data) and 2,350 inferences
(717 MB), a quarter of them of data met before, whose regions are kept by its hash.

## Tests

- `tests/infer.rs`: files of known layouts (as `scripts/rec_synth.py` makes): the layout is found,
  and every region taken restores.
- `tests/edge.rs`: special floats, either byte order, tiny and random inputs, the decoders damaged.

## Left

- Nothing known.
