# aqr-xpress

XPRESS Huffman streams split into their data and a record of their parse, and rebuilt bit for
bit. The archive then stores the data, which compresses better than the streams, which
aqr-compress recompresses in turn (a PE file by aqr-exe, and so on) and which other files may
share. Windows writes XPRESS Huffman as WOF's XPRESS4K, XPRESS8K and XPRESS16K (CompactOS,
`compact /exe`), WIM resources, ESE databases and hibernation files; a container (WOF in
aqr-disk, WIM) takes the chunks apart and makes the applications.

No encoder is reproduced (Windows' is closed): any stream has a record, 3-7% of the compressed bytes for
Windows'. It was written from the public description of the algorithm ([MS-XCA]), without any
implementation's code. `FORMAT.md` specifies the streams, the record, the module `xpress-recons`, and
how Windows' encoders behave.

```sh
cargo run --release -p aqr-xpress --example aqr-xpress-check -- [-n K] FILE...   # split, rebuilt; record share and speeds
```

```rust
use aqr_xpress::recons;
// a WOF stream's (or a buffer's) chunks, each its bytes and its data's length
let sp = recons::split(&chunks)?;
assert_eq!(recons::join(&sp.data, &sp.record)?, chunks.concat());
```

## What is taken

- WOF's XPRESS4K, 8K and 16K files (CompactOS, `compact /exe`) taken apart by aqr-disk from NTFS
  volumes in disk images (streams of 8 MiB of data at most; aqr-disk's README has the figures), and
  `RtlCompressBuffer` XPRESS_HUFF buffers, the standard and the maximum engines.
- Not taken: WIM's XPRESS resources (aqr-wim does not use it yet; chunks of 32 KiB, the same format, no
  sample here), ESE databases and hibernation files, XPRESS without Huffman (plain LZ77).

## Results

Windows 11 samples, 2026-11-09 (`aqr-xpress-check -n 3`, an M-series Mac): `RtlCompressBuffer`
XPRESS_HUFF, the standard and the maximum engines, whole buffers, and WOF's chunks as NTFS holds them,
on text (712,895 bytes of Rust source), a repeating byte pattern (`gen`, 1 MiB), text, pattern and text
again (`mixed`, 2 MiB) and noise (256 KiB). Every stream was rebuilt. Record share of the compressed
bytes:

| sample | 4 KiB chunks | 8 KiB | 16 KiB | 64 KiB blocks, standard | maximum |
|---|---|---|---|---|---|
| text | 2.98% | 3.84% | 4.69% | 6.87% | 3.55% |
| gen | 0.02% | 0.06% | 0.05% | 0.81% | 0.06% |
| mixed | 2.23% | 3.14% | 4.05% | 6.78% | |
| noise | | | | 0.03% | |
| all | | | | | 3.38% (all 15 files: 134,745 of 3,988,408 bytes) |

Speed, one thread, MB/s of data: splitting 12 (text, standard), 22 (maximum), 34 to 66 (4 to 16 KiB
chunks of text, mixed), up to 350 (repeats); rebuilding 44 (text, standard), 61 (maximum), 72 to 125
(chunks), up to 870. The module `xpress-recons` is 70,631 bytes of Wasm (16,108 bytes as its module
block).

## Tests

```sh
cargo rt -p aqr-xpress
```

Windows' streams in `tests/data` (zstd'd: two blocks of text of each engine, the pattern, WOF's
chunks of 4, 8 and 16 KiB with a stored chunk): read, written back, split and rebuilt. Streams of
a small encoder of the tests (greedy matches from all positions or from the tokens' starts,
random literals and shorter matches, code lengths off now and then, blocks cut by the rule,
lengths of every form up to 200,000, the end symbol or not, stored chunks): the same.
Damaged records, data and streams do not panic. aqr-decompress runs the module as WebAssembly
on `wof4k.chunks` and `gen.chunks` and checks its memory plan.

## Left

- **Windows' candidate set.** The decisions between a literal and a match are most of the standard
  engine's record (2.5-4.2% of the compressed bytes on text): Windows' hash table is not known, and the
  positions of the chain it misses are guessed from the nearest candidate's length and distance.
- **Splitting speed.** 12 MB/s on text (standard), two trial walks on the first 256 KiB.
- **WIM's XPRESS resources.** aqr-wim does not use the module yet; no sample here.
- **Threads.** A large buffer is split on one thread.
- **Plain XPRESS.** XPRESS without Huffman (LZ77 alone) is not taken.
