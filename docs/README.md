This directory hosts various design documents produced and maintained during the development.
Most of them are currently written in Korean, sorry.

- `glossary.md` (English): words of the codebase not obvious at a glance, informally.
- `10gb-todo.md`: what is left for the 10 GB benchmark.
- `file-formats.md`: measurements by format, formats taken and not yet taken.
- `xform-design.md`: formulas, modules and their execution.
- `streaming-encoder-design.md`, `arena-design.md`, `sandbox-design.md`: the streaming encoder, modules' memory plans, the sandbox for modules.
- `append-design.md`, `recovery-design.md`, `special-files-design.md`, `names-design.md`, `zip-extras-design.md`: appending, recovery, special files, names on extraction, ZIP extra fields.
- `eval-design.md`, `bench-scenarios.md`: evaluation and benchmark scenarios.
- `cross-file-design.md`: duplicates across files seen only once their streams are decoded (versions of an installer): a batch's map of shared anchors, kin groups merged, decisions made with them.
- `macos-app-todo.md`: what is left for macOS application bundles.
- `disk-image-design.md`: the plan for disk images (file systems, partition tables, virtual disks).
- `other-projects.md`: survey of other projects.
- `encrypted-sfx-design.md`: self-extracting programs of encrypted archives: the subsystem patched per program, decryption and its UI as blobs.
- `win-shell-ext-design.md`: archives browsed in Explorer as folders (a shell namespace extension), decoded by `aqr --serve` behind a COM local server.
- `rar3-decoding.md`, `rar5-decoding.md`: notes on the RAR formats for the clean-room aqr-rar.
