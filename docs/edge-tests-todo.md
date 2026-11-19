# Edge-case tests: what is left

An audit (2026-11-19) of the tests, by reading them and the code, listed the edge cases they
missed, about 430 items (the list as it was: this file at 4016b556). They were taken in turn,
each by its test first (failing, or pinning what is right), then the fix: what is below is
what is left.

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

### Housekeeping
- The SFX stub (aqr-sfx-shim) and the pack are to be rebuilt for a release (`cargo aqr` runs
  pack.py).
- The wwise pad-byte heuristic (wem.rs) is unchecked on real `.wem` files (none in the corpus).
