# Conformance suite (draft)

Cases of interpreting an archive (`spec/index.bs`, "Interpreting an archive"): the bytes of an
archive written field by field in EDN, and the archive value or failure they interpret to.

- `case.cddl` — the case format and its notation (labels, differences, inference).
- `cases/*.edn` — the cases.
- `cases/data/` — contents too long to write in a case (`80003(path)`), and the modules of the
  cases: each `NAME.wat` with its `NAME.wasm`, made by `cargo run -r -p aqr-decompress --example
  aqr-decompress-wat2wasm -- cases/data/*.wat` (committed, so that the tools need no Rust).
- `tools/wasmsplit.py` — the split form of a module: a splitter, and the join as the spec has it.
- `tools/age.py` — what an encrypted archive takes of age v1, ChaCha20-Poly1305 in Python.
- `tools/assemble.py CASE.edn [-o OUT]` — makes the bytes of a case (Python 3, no packages).
  Encrypted and unencrypted archives of every block but recovery blocks (given as their bytes); module blocks of
  the stored form. What it infers of a formula block (num_outputs, sizes, checksums) it takes from
  running the formula on what it knows; a Compressed_Block tells what it decodes to (`decoded`).
- `tools/check.py [CASE.edn...]` — checks the cases against `aqr -Z interpret` (through
  `cargo aqr`; `--aqr` for another command), which prints the archive value an archive is
  interpreted to in the notation of `case.cddl`: the failure by its exit code, an archive value
  in full (extras, forks, the order of every map; link groups up to a renaming). An overlay
  archive given a base is skipped.

The cases are named by what they test, `fail-*` and `unsupported-*` those of a failure. A case
that `aqr` doesn't pass is not wrong for that: `check.py` lists where `aqr` differs from the
specification as well as where a case is wrong.

Left: registered modules applied, registered dictionaries, partial access,
and the suites of extraction and of compressors.
