# aqr-fuzz

Fuzz targets of the archive decoder (aqr-archive), for cargo-fuzz (libFuzzer, nightly). Built
by cargo-fuzz, aqr-archive has `cfg(fuzzing)` and skips every checksum (`sum_holds`: blocks',
outputs', mirror blocks', the encrypted header's; chunks' tags as `envelope` tells), so that mutated inputs get past the checks to
what follows. A crate of its own workspace: the main one's builds and tests leave it out.

- `decode`: an archive opened and decoded (its footer, segments, metadata, formula blocks
  evaluated), its archival members, and `inspect`.
- `salvage`: an archive checked for its mirror blocks, repaired, opened salvaging and decoded.
- `envelope`: an encrypted archive opened as `aqr -d` opens it: its envelope parsed (damaged or
  not), unlocked, its payload decrypted as it is read (by its index, and whole), then decrypted
  whole with the chunks that do not authenticate, its mirror blocks checked of the bytes trusted
  (`check_sealed`), repaired and salvaged. Its seeds are `decode`'s as payloads, sealed with
  mirrored headers and without (a tree's blocks mirrored too), and a file of several chunks, cut
  short after a chunk and of its header magic damaged; all of passphrase `aqueous` at work
  factor 12, the only one unlocked (scrypt's keys are kept, derived once for each salt). A
  chunk that does not authenticate is decrypted without it and taken to where its tag's first
  byte is even, so that a mutation of a chunk is one of the payload at the same place, and the
  chunks of the others are the ones not trusted.
- `roundtrip`: a formula made of the input (`src/formula.rs`: up to 3 streams of each kind, up
  to 3 applications of several outputs, of `cut`, a node or a patch split, and a root of slices,
  concatenations, packs, applications of `rev` and `copy`, outputs and patches, each output
  placed once; now and then a slice out of its stream or an application of another size)
  archived by the encoder (stored, zstd 1 and 3, low) and decoded again, whole and streamed: where
  the target's reference evaluator, a recursive walk of its own (not aqr-archive's), gives an
  output, the encoder has to take the formula and the archive has to give it back. Its seeds are
  only bytes to make formulas of.
- `natives`: an archive of recompressed files decoded by aqr-decompress's native modules (a
  module not known fails: no Wasmtime), so each module's decoding of the params and streams an
  archive tells. Its seeds are the codecs' fixtures (`codecs/*/tests/data`, at most 256 KiB)
  recompressed and archived again of stored blocks, the modules by their hashes alone
  (shorthand module blocks), at most 512 KiB.
- `compress`: a file compressed (its format found by aqr-compress and recompressed: every
  recompressor's reading of untrusted files) and decompressed again by the native modules,
  which must give it back. Its seeds are the codecs' fixtures as they are.

`decode`, `salvage` and `natives` mutate with a mutator of their own (`src/lib.rs`'s `mutate`):
half of the time, a block whose data decodes by itself (aqr-archive's `fuzzing`: metadata, data,
module, resource, dictionary and formula blocks, but those of a dictionary's data) is picked, its
data decoded, mutated by libFuzzer's mutator and the block made again of it (Zstandard at level
1, or raw Zstandard blocks), a spacer block after it taking what it no longer does, so that the
blocks after it stay where the footer and the metadata tell; where it is longer than it was, and
otherwise, the input is mutated as it is.

In `decode`, `salvage`, `envelope` and `roundtrip`, the modules are toys known by their stand-ins (`src/lib.rs`: `rev`, `cut` of several outputs,
`copy` as a part module), so that formulas applying modules are evaluated without the codecs.
The global allocator is aqr-testkit's `Refusing` at 1 GiB: a reservation tried for more goes
on without it, as on an exhausted heap, and an allocation that cannot fail aborts, which the
fuzzer takes for a crash.

```sh
cargo install cargo-fuzz
fuzz/run.sh decode -max_total_time=600 -fork=8 -ignore_crashes=1 -ignore_ooms=1
```

(`-fork` goes on past crashes, writing each to `artifacts/`; `-jobs=N` ends each job at its
first, and the run once all have.)

`run.sh` writes the seeds first where `corpus/TARGET` is not there (`src/bin/seeds.rs`, built
without `cfg(fuzzing)`, so the seeds decode with their checksums: single files of each
instruction and of the toy modules, and trees, stored and compressed), and gives libFuzzer the
dictionary `aqr.dict` (the magics and multi-byte tokens; single bytes are left to
`-use_value_profile=1`), with `-max_len` and `-timeout` by target. Panics abort (libFuzzer's
hook), even where the code catches them (a recompressor's, say). Builds take
`-Cllvm-args=-asan-globals=0`, as `run.sh` gives it: ASan's redzones around globals pad
inventory's constructors in `__mod_init_func`, which ld then refuses ("initializer pointer has
no target"). Crashes go to `artifacts/TARGET/`; one is run again by
`target/aarch64-apple-darwin/release/TARGET FILE` (with `ASAN_OPTIONS=allocator_may_return_null=1`,
as `run.sh` sets it), and timed as plain code by `cargo +nightly run --release --bin replay --
TARGET FILE...` (natives, compress, roundtrip): instrumented, `compress` is about 12 times as slow (a seed
of 3.7 s, `aqr-egg-azonumbers.egg`, took 44 s). `corpus/`, `artifacts/` and `target/` are not committed.

Left: that mutator of `envelope`'s payloads (decrypted, mutated so and sealed again); in
`roundtrip`, trees of several leaves (batches, the dictionaries trained for them), resources and
the encoder's other options; native modules against Wasm on the same inputs.
