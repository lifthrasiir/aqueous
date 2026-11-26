This directory hosts various design documents produced and maintained during the development.
Most of them are currently written in Korean, sorry. They keep what is useful now: rationale not
in the spec or the crates' READMEs, final measurements, rejected approaches, and what is left;
the history of finished work is in git.

- `crates.md`, `modules.md` (English): what each crate holds; the rules for code a module links (CLAUDE.md's details).
- `glossary.md` (English): words of the codebase not obvious at a glance, informally.
- `10gb-todo.md`, `bench-scenarios.md`, `cli-todo.md`, `edge-tests-todo.md` (English),
  `macos-app-todo.md`: what is left for the 10 GB benchmark, other benchmark scenarios, the
  command line, the edge cases left untested, macOS application bundles.
- `file-formats.md` (English): measurements by format, formats taken and not yet taken.
- `xform-design.md`, `eval-design.md`: formulas, modules and their execution; the decoder's evaluator.
- `streaming-encoder-design.md`, `arena-design.md`, `sandbox-design.md`: the streaming encoder, modules' memory plans, the sandbox for modules.
- `append-design.md`, `recovery-design.md`, `special-files-design.md`, `names-design.md`, `zip-extras-design.md`: appending and journaling, recovery, special files, names on extraction, ZIP extra fields.
- `convert-design.md`, `lossy-design.md`, `hints-design.md`: `--convert`, `--lossy`, attributes by path.
- `cross-file-design.md`: duplicates across files seen only once their streams are decoded (versions of an installer).
- `disk-image-design.md`: disk images (file systems, partition tables, virtual disks).
- `log-design.md`, `xml-design.md`, `div3-design.md`, `pak-inference-design.md`, `azo-x86-filter-search.md`: notes behind aqr-log, aqr-xml, aqr-div3, aqr-infer-pak and AZO's x86 filter.
- `sfx-config-design.md`, `encrypted-sfx-design.md`: self-extracting programs' settings (`.aqr/sfx/`), and those of encrypted archives (mostly not done).
- `win-shell-ext-design.md`: archives browsed in Explorer as folders (a shell namespace extension), decoded by `aqr --serve` behind a COM local server.
- `other-projects.md`: survey of other projects.
- `rar3-decoding.md`, `rar5-decoding.md`, `installshield-cab.md` (English): notes on the RAR formats for the clean-room aqr-rar, and on InstallShield cabinets.
