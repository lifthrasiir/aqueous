# Conformance suite (draft)

Cases of interpreting an archive (`spec/index.bs`, "Interpreting an archive"): the bytes of an
archive written field by field in EDN, and the archive value or failure they interpret to.

- `case.cddl` — the case format and its notation (labels, differences, inference).
- `cases/*.edn` — the cases.
- `tools/assemble.py CASE.edn [-o OUT]` — makes the bytes of a case (Python 3, no packages).
  A prototype: unencrypted archives of spacer, size hint, metadata and data blocks; any other
  block is given as its bytes.
- `tools/check.py [CASE.edn...]` — checks the cases against `aqr -Z interpret` (through
  `cargo aqr`; `--aqr` for another command), which prints the archive value an archive is
  interpreted to in the notation of `case.cddl`: the failure by its exit code, an archive value
  in full (extras, forks, the order of every map; link groups up to a renaming). A case given a
  base is skipped.

Left: bases (overlay archives), encrypted archives, formulas and modules, imports, mirror and
recovery blocks, partial access, and the suites of extraction and of compressors.
