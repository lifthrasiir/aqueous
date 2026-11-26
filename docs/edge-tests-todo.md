# Edge-case tests: what is left

An audit (2026-11-19) of the tests, by reading them and the code, listed the edge cases they
missed, about 430 items (the list as it was: this file at 4016b556). They were taken in turn,
each by its test first, then the fix; the history is in git. What is below is what is left
(spot-checked against the code 2026-11-27: the tests and limits named below still exist).

Decided: no fuel or epoch interruption in aqr-wasmrt (neither deterministic nor exact); a
module that never ends hangs the decoder, by design for now.

What the tests have for these cases: `profile.release-test` checks overflows; tests needing a
tool (sqlite3, ffmpeg) or the SFX pack fail without it, and those needing a corpus are
`#[ignore]`; crates/aqr-testkit (`damaged`, `within`, `Refusing`); aqr-decompress's
`tests/common` (`fail_alike`: native and Wasm alike on damaged inputs, tests/alike*.rs) and
`tests/host.rs` (the host functions' violations).

## Left

### Coverage not reached
- Native vs Wasm on damaged inputs: Vorbis over 2 MiB and packets of 0xFFFF bytes or more,
  Wwise's `mod_packets == false` and its other chunks (aqr-wwise is no dependency of
  aqr-decompress), tensors' lying headers and pickles; 7z only of lzma2, BCJ2 and Delta, AZO
  only of a short stream (time).
- Past 4 GiB: only level 0 (stored) of ZIP and 7z (aqr-pack's Contents are held in memory);
  SQLite's lock-byte page only of 4096-byte pages.
- aqr-pdb: a real PDB of several type modules; aqr-hdf5's superblocks 1 and 2 beyond
  `superblocks()`; whole-file DWARF (`.dwo` sections kept) through aqr-compress.
- aqr-bundle: compressed payloads (unity-bundle's LZ4/LZO blocks, unreal's chunks) are not
  damaged deeply (the builders make stored blocks).
- aqr-exe through aqr-compress: the CIL, Inno and NSIS paths (inputs of 12-24 KiB); a Mach-O
  arch at an offset inside the fat header; FAT_MAGIC arch sizes over 4 GiB. SHN_XINDEX is not
  read (e_shnum 0 taken as it is): a feature, if wanted.
- aqr-wasmrt's interpreter path (wasm/interp.rs, the pack's interpreter blob): only a round
  trip (aqr's tests/sfx.rs, `wasm_through_the_interpreter`), not the host's violations nor
  damaged inputs. The blob is of aqr-wasmrt as the pack was made: `cargo aqr` runs pack.py
  again for known.rs's modules, not for aqr-wasmrt's sources, so the host's fixes reach aqr.exe
  and the SFX only once the pack is rebuilt by hand.
- A real crash between an append's writes is not simulated (the op log's replay cuts at each).

### Known limits, pinned
- aqr-div3, aqr-ac3: the over-read check is per frame or record, so one huge frame still
  decodes from zeros before it is caught.
- aqr-wim: a valid LZX stream of a real high ratio still makes a large formula.
- The sandbox: an idle worker's death fails the next application once (not retried).
- aqr: a footer of more than 16 MiB is left a spacer on a cancel (`FOOTER_RESTORE`); a second
  Ctrl-Break of a graceful append leaves the last commit (the console handler does not undo);
  an input named on the command line with a trailing dot or space, or a device name, is not
  read verbatim (only the walk's names are).
- aqr-xsv: tables wider than about 1000 columns are not recognised (the dialect's sample of
  64 KiB holds too few records).
- aqr-archive, found by fuzzing (`fuzz/`), to revisit:
  - `mirror::repair` salvaging fills the gap up to a block restored past the end with zeros as
    long as its mirror block tells, and a mirror block's checksum is no authentication: a file
    of a few hundred bytes can make `aqr -d` take as much memory as is had (a resource failure
    beyond it, no longer an abort). A target past the end is one after its mirror block, which
    the spec allows ("Mirrored archives": the last metadata block's mirror block placed before
    the blocks held back before it, those of 4096 bytes at least) with no bound on how far after
    it the target is; 4 MiB is only the distance recommended (`mirror-block`). So no bound
    follows from the format, as one did of Zstandard's declared sizes (`decoded_upto`).
    aqr's own writer puts a mirror block before its target only for the last metadata block,
    holding back `mirror::FAR` (4 MiB) of the archive before it (`finish_mirrored`; the mirror
    blocks of earlier segments come after theirs, between the next segment's blocks): a gap of
    about that, never more. Either:
    - the spec bounds how far a mirror block placed before its target may be from it (e.g. 64
      MiB, `FAR` and room), `repair` refusing a target past it: the gap bounded by the format,
      a writer holding back more not conforming; or
    - `repair` takes a budget (`-M`, as the decoder's other memory), the gap a resource failure
      beyond it: no change to the format, but a genuine archive's gap needing the budget given.

- aqr, found by `tests/extract_safety.rs` (`over_another_with_force`, ignored): extracted with
  `-f` over a directory another archive was extracted into, an archive's links are checked
  against its own tree alone, not against what is there, so a link may come to lead out of the
  directory: of 2000 random trees on macOS, 3. A link of the first (`A/é -> x/../../outside`,
  `x` a directory then) led out once the second made `A/x` a link (`A/..`); a link of the
  second (`a/é -> A/..`, `A` not of its tree) led out through the first's link `a/a -> ..`, `A`
  and `a` one name on APFS. Files are never written through what is there (a link where a
  directory goes is taken away, `force_over_links_to_directories`), but a link leading out is
  left, which the default `--links inside` means never to make. To decide: links of an
  archive checked against what is there too (resolved as the system resolves them, as the test
  does), and the links there that a replaced name leads through checked again (or taken away);
  or `-f` told to promise less over what is there. One archive into a directory of its own:
  5000 trees, none.

### Found by the conformance suite (`conformance/`, 2026-11-24)
`conformance/tools/check.py` (through the root's .venv, for its `cryptography`) runs the cases
against `aqr -Z interpret`: of 342, 309 pass, 24 fail, 1 is a resource failure and 8 are
skipped (overlay archives given a base). Where aqr differs from the spec:
- Import blocks (58-5F) are not taken: `import-*`, `fail-import-*` (14 cases, their expectations
  not checked by any decoder yet).
- Overlay archives are not taken: `overlay-no-base`, `encrypted-overlay`,
  `unsupported-overlay-before-base` (and `fail-overlay-invalid-before-base` passes only because
  of that); the 8 overlay cases given a base are skipped, as `aqr` takes no base.
- Timestamps (`aqr-time`) are not checked: `fail-extra-time-tag`, `-time-float`, `-time-offset`,
  `fail-extra-date-offset`.
- `segment-status`'s map takes keys other than 0 and 1: `fail-segment-status-key` (a positive
  key not known, an error as integer keys were meant to be; see below).
- A table of 64-bit addresses passes the module profile: `fail-profile-table64`.
- A passphrase from standard input is read as UTF-8: `encrypted-bytes-passphrase` (see below).
- A shorthand block of a registered dictionary is a resource failure ("not supported yet"), not
  unsupported where the hash is not known: `unsupported-shorthand-dictionary`.

To decide, in the spec:
- "Partial access": a failure in what a decompressor doesn't read is exempt, of any kind, and
  for one computing the full result too (a block no part of the value depends on). The cases
  give that result as `partial`; the spec's wording (and "Semantics and failures", the step 2
  of interpreting) still asks every decompressor to detect them.
- Integer keys of the optional metadata not known: meant to be an error where positive and
  skipped where negative; neither is what the metadata now does for negative ones.
- A passphrase: a byte sequence (age takes bytes; rage takes UTF-8)? Then aqr takes standard
  input's line as bytes.
- An empty fork name: the archive model forbids it, `aqr-entry-forks`'s CDDL doesn't.
- A text string of bad UTF-8 in the optional metadata: well-formed CBOR, not valid; the spec
  asks for well-formed only.

### Housekeeping
- The wwise pad-byte heuristic (wem.rs) is unchecked on real `.wem` files (none in the corpus).
