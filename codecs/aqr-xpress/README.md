# aqr-xpress

XPRESS Huffman streams split into their data and a record of their parse, and rebuilt bit for
bit. The archive then stores the data, which compresses better than the streams, which
aqr-compress recompresses in turn (a PE file by aqr-exe, and so on) and which other files may
share. Windows writes XPRESS Huffman as WOF's XPRESS4K, XPRESS8K and XPRESS16K (CompactOS,
`compact /exe`), WIM resources, ESE databases and hibernation files; a container (WOF in
aqr-disk, WIM) takes the chunks apart and makes the applications.

No encoder is reproduced. Windows' is closed. The record codes the parse given the data, under
contexts of what a match finder sees there (the matches whose first position starts a token, or
all positions' for the maximum engine; the longest, the nearest and whether the next position
has a longer one), a match by its rank among the candidates that set records, and the code
lengths against a Huffman code of each block's frequencies (which Windows' are, always). It was
written from the public description of the algorithm ([MS-XCA]), without any implementation's
code. The streams, the record and the module `xpress-recons` are in [FORMAT.md](FORMAT.md).

```sh
cargo run --release -p aqr-xpress --example aqr-xpress-check -- [-n K] FILE...   # split, rebuilt; record share and speeds
```

```rust
use aqr_xpress::recons;
// a WOF stream's (or a buffer's) chunks, each its bytes and its data's length
let sp = recons::split(&chunks)?;
assert_eq!(recons::join(&sp.data, &sp.record)?, chunks.concat());
```

## Results

Windows 11 (`RtlCompressBuffer` XPRESS_HUFF, the standard and the maximum engines, whole
buffers; WOF's chunks as NTFS holds them), on text (712,895 bytes of Rust source), a repeating
byte pattern (`gen`, 1 MiB), the same text, pattern and text again (`mixed`, 2 MiB) and noise
(256 KiB). `aqr-xpress-check -n 3`, an M-series Mac with other work running, measured on
2026-11-09. Every stream was rebuilt. Record share of the compressed bytes:

| sample | 4 KiB chunks | 8 KiB | 16 KiB | 64 KiB blocks, standard | maximum |
|---|---|---|---|---|---|
| text | 2.98% | 3.84% | 4.69% | 6.87% | 3.55% |
| gen | 0.02% | 0.06% | 0.05% | 0.81% | 0.06% |
| mixed | 2.23% | 3.14% | 4.05% | 6.78% | |
| noise | | | | 0.03% | |
| all | | | | | 3.38% (all 15 files: 134,745 of 3,988,408 bytes) |

What the record is made of, on text (percent of the compressed bytes): the decisions between a
literal and a match 2.5 to 4.2 (4 KiB chunks to 64 KiB blocks), the matches' offsets (ranks) 0.5
to 2.7, lengths none (a match is the whole match at its offset), code lengths and the rest below
0.01. The maximum engine's record is nearly all offsets (3.1 of 3.55). The first version, which
looked at every position and coded a length and an offset's rank, took 24.6% on text (standard,
buffer); knowing that Windows' standard engine only looks at the positions where tokens
started, and codes record setters, took it to 6.9%.

Speed, one thread, MB/s of data: splitting 12 (text, standard; it tries both candidate
modes on the first 256 KiB), 22 (maximum), 34 to 66 (4 to 16 KiB chunks of text, mixed), up to
350 (repeats); rebuilding 44 (text, standard), 61 (maximum), 72 to 125 (chunks), up to 870.

The module `xpress-recons` is 70,631 bytes of Wasm (16,108 bytes as its module block).

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

## Status

The module `xpress-recons` (natively and as a Wasm module of aqr-modules) is done. Containers do
not use it yet (aqr-disk's WOF, aqr-wim's XPRESS resources).

Not yet: the engine's own candidate set (the decisions between a literal and a match are most
of the standard engine's record: Windows' hash table is not known, and positions of the
chain it misses are guessed from the nearest candidate's length and distance); speed of
splitting (two trial walks on the first 256 KiB); several threads for a large buffer; XPRESS
without Huffman (plain LZ77, LZNT1); WIM's XPRESS resources (chunks of 32 KiB, the same format)
have no sample here, so none was tested.
