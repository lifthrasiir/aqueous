# aqr-fuzz

Fuzz targets of the archive decoder (aqr-archive), for cargo-fuzz (libFuzzer, nightly). Built
by cargo-fuzz, aqr-archive has `cfg(fuzzing)` and skips every checksum (`sum_holds`: blocks',
outputs', mirror blocks', the encrypted header's), so that mutated inputs get past the checks to
what follows. A crate of its own workspace: the main one's builds and tests leave it out.

- `decode`: an archive opened and decoded (its footer, segments, metadata, formula blocks
  evaluated), its archival members, and `inspect`.
- `salvage`: an archive checked for its mirror blocks, repaired, opened salvaging and decoded.

The modules are toys known by their stand-ins (`src/lib.rs`: `rev`, `cut` of several outputs,
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
`-use_value_profile=1`). Crashes go to `artifacts/TARGET/`; one is run again by
`target/aarch64-apple-darwin/release/TARGET FILE` (with `ASAN_OPTIONS=allocator_may_return_null=1`,
as `run.sh` sets it). `corpus/`, `artifacts/` and `target/` are not committed.

Left: targets of each codec's decoding (`module.rs`, from the inputs of its `tests/edge.rs`),
encrypted archives (`envelope`), a mutator that mutates blocks' decompressed data and frames it
again, and the round trip of formulas made by `arbitrary` through the encoder.
