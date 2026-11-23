# Conformance suite (draft)

Cases of interpreting an archive (`spec/index.bs`, "Interpreting an archive"): the bytes of an
archive written field by field in EDN, and the archive value or failure they interpret to.

- `case.cddl` — the case format and its notation (labels, differences, inference).
- `cases/*.edn` — the cases.
- `tools/assemble.py CASE.edn [-o OUT]` — makes the bytes of a case (Python 3, no packages).
  A prototype: unencrypted archives of spacer, size hint, metadata and data blocks; any other
  block is given as its bytes.
- `tools/check.py [CASE.edn...]` — checks the cases against `cargo aqr` (`--aqr` for another
  command): the failure by its exit code; an archive value by `-t --json` and `-d`, which leave
  out extras, forks, link groups and the order of children. A case given a base is skipped.

Left: a runner of aqr-archive's decoder giving the archive value itself (to compare extras
and order), encrypted archives, formulas and modules, imports, mirror and recovery blocks,
partial access, and the suites of extraction and of compressors.
