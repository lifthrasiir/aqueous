# aqr-fuzz

Fuzz targets of the archive decoder (aqr-archive), for cargo-fuzz (libFuzzer, nightly). Built
by cargo-fuzz, aqr-archive has `cfg(fuzzing)` and skips every checksum (`sum_holds`: blocks',
outputs', mirror blocks', the encrypted header's; chunks' tags as `envelope` tells), so that mutated inputs get past the checks to
what follows. A crate of its own workspace: the main one's builds and tests leave it out.

- `decode`: an archive opened and decoded (its footer, segments, metadata, formula blocks
  evaluated), its archival members, and `inspect`; `footer_of`, `trim`, `locate` (what it finds
  opened) and `pe_certificate_table` of it. Where it decodes, of a tree, it has to decode alike
  read otherwise (`src/decode.rs`): by its index, as it was after each segment (whole and by
  its index), of every other file's path alone (`open_indexed_at`, the files taken by
  `contents_of`, as `aqr -d` takes those of paths given), and written again by `rewrite`
  (`--compact`) of its tree without those files. It found the whole reading taking a footer's
  metadata block before the archive's start.
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
  concatenations, packs, applications of `rev` and `copy` (now and then of resources), outputs
  and patches, each output placed once; now and then a slice out of its stream or an
  application of another size) archived by the encoder (stored, zstd 1 and 3, low; its modules
  written by default, as their bytes, as blocks made beforehand or as shorthand blocks of their
  hashes, the toys' runner knowing them) and decoded again, whole and streamed, and now and then
  sealed too (its size hint filled in, its blocks mirrored or not, mirrored headers or not) and
  decoded as `aqr -d` decodes it, decrypted whole and as it is read; or a tree of 1 to 3 segments committed one after another (now and
  then appended to by a writer of its own, of what `footer_of` tells, as `aqr -a` appends:
  `SegmentWriter::appending` after the footer, or `replacing` over it), each of 1 or 2 batches of such formulas, of 1 to 4 leaves of 1 to
  3 outputs (an application's outputs all in one leaf, as `SegmentWriter::batch` wants them),
  with dictionaries or without; its entries (files, directories, links, tombstones as `aqr -s`
  writes them) at paths of up to 3 names of `a` to `d`, so that later segments meet earlier
  ones' paths, each of an entry extra or none (times, the mode, the owners, a comment, extended
  attributes, extension keys, ZIP extra fields; the root too; of files, link groups of their segment or archive-wide, and forks, the contents of the
  segment after them; of links, their Windows types), decoded again and checked by a model of the spec's merging of segments (directories
  merged, their extras kept where a later one is null, any other node replacing what is at its
  path with what is under it, a tombstone removing it): forks by their bytes, link groups by
  the files of each (a segment's own told apart from another's of the same number). Where
  the target's reference evaluator, a recursive walk of its own (not aqr-archive's), gives the
  outputs, the encoder has to take them and the archive has to give them back. Its seeds are only
  bytes to make formulas of. It found `batch` taking an application's outputs spread over
  leaves, whose blocks then did not decode.
- `unpack`: an archive of another format (its first byte telling which: ZIP, 7z, RAR, ALZ, EGG,
  tar alone or under each compressor, a compressor's stream of a single file) read by
  aqr-unpack as `aqr -d` reads it, unlocked by its seeds' passphrases where it is encrypted,
  into a sink checking what it is told (each entry after its directory, each content of a file
  or a fork told); where it unpacks, unpacked again of every other file's path alone, those
  files as they were. Its seeds are aqr-unpack's test archives and the codecs' of those
  formats. It found contents of files left out as they were added told still.
- `modules`: a WebAssembly module, as an archive may carry any, checked against the profile
  and run by aqr-wasmrt (Wasmtime, in this process) as the decoder runs one it does not know,
  of an application of the input's parameters, resources, inputs, outputs' sizes and limits:
  of the input's bytes (its seeds aqr-modules' own modules), or made by wasm-smith of a shape
  of the input (a whole module, a part module or one of tasks, of task slots; the memories and
  host functions of the profile of its kind, the entry point of the application's arity), its
  loops bounded by fuel, its `memory.grow` of other memories than the working memory made
  grows that fail (the profile refuses them). Nothing is checked of what it gives (the decoder
  checks the outputs' sizes): that it fails or ends, and does not crash. Slow (Wasmtime
  compiling each module, instrumented).
- `pack`: a tree made of the input (files, directories and links at paths of up to 3 names,
  ASCII and Hangul) written by aqr-pack as an archive of another format (its first byte telling
  which: ZIP, 7z, ALZ, EGG, tar alone or under each compressor, a single file's stream; a low
  level; ZIP's and 7z's encrypted now and then) and read back by aqr-unpack: its files as they
  were by their paths, and where the writer told nothing left out (all but ALZ's and EGG's of
  links), its directories and links too. Names not UTF-8 are left out (a reader takes their
  encoding as it guesses it).
- `natives`: an archive of recompressed files decoded by aqr-decompress's native modules (a
  module not known fails: no Wasmtime), so each module's decoding of the params and streams an
  archive tells. Its seeds are the codecs' fixtures (`codecs/*/tests/data`, at most 256 KiB)
  recompressed and archived again of stored blocks, the modules by their hashes alone
  (shorthand module blocks), at most 512 KiB.
- `wasm`: `natives`' archives decoded twice, by the native modules and by the same modules as
  WebAssembly (Wasmtime): both fail alike (invalid, unsupported), or both give the same; a
  resource failure of either is let
  be (natively, some modules decode whole on threads, not held to their plans; in Wasm, an
  application over its limits fails). Its seeds are `natives`'. It found the executor's deadlock
  of aqr-wasmrt (a future dropped with the executor locked, waking another).
- `compress`: a file compressed (its format found by aqr-compress and recompressed: every
  recompressor's reading of untrusted files) and decompressed again by the native modules,
  which must give it back. Its seeds are the codecs' fixtures as they are.

`decode`, `salvage`, `natives` and `wasm` mutate with a mutator of their own (`src/lib.rs`'s `mutate`;
`envelope` with `mutate_sealed`, of the payload decrypted by the seeds' passphrase, the changes
put back into the ciphertext where they are, as ChaCha20's keystream is the same, and the
changed chunks' tags made to be taken):
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
TARGET FILE...` (natives, wasm, compress, roundtrip; with `RUSTFLAGS="--cfg fuzzing"` and a
`CARGO_TARGET_DIR` of its own, the checksums skipped as the fuzzer skips them). Instrumented,
`compress` is about 12 times as slow (a seed of 3.7 s, `aqr-egg-azonumbers.egg`, took 44 s). A
timeout that does not come again alone may be a race: the file given many times over, to either,
runs it so many times in one process (`wasm`'s deadlock hung once in about 200 runs). `corpus/`, `artifacts/` and `target/` are not committed.

Left: the SFX stub's reading of what follows its image (the bundle of blobs, its settings).
