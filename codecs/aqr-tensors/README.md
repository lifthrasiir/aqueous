# aqr-tensors

Files of tensors rebuilt bit for bit (`FORMAT.md`): GGUF model files (llama.cpp's), PyTorch's
files of `torch.save` (its ZIP format: `.pth`, `.pt`, `.ckpt`) and safetensors. The header (of a
`.pth`, its pickle and the entries' headers) goes as it is, and the tensor data in segments of
16 MiB, each an application of the part module `tensors-recons` (until 2026-11-12 the crate was
aqr-gguf and the module `gguf-recons`), in which

- the floats' (BF16, F16, F32) exponents are symbols, a table for each tensor's part of a
  segment (the 15 or 16 commonest exponents, the rest escapes), and their signs and mantissas
  go as they are, without the low bits that are zero in all of them (F16 of BF16's precision:
  3 bits; the few floats that are not, subnormals, are escapes whole);
- the quantised blocks' quants are symbols: the nibbles of Q4_0, Q4_1, Q4_K, IQ4_NL, IQ4_XS
  and MXFP4, Q6_K's low nibbles and its 2-bit high parts, Q8_0's high nibbles and the high 4
  bits of Q5_0's, Q5_1's and Q5_K's quants (their low nibbles and lowest bits as they are),
  Q2_K's 2-bit quants and Q3_K's 3-bit ones; the blocks' scales go to the side, for the archive
  to compress;
- of the types of codebooks (IQ1_S, IQ1_M, IQ2_XXS, IQ2_XS, IQ2_S, IQ3_XXS, IQ3_S), the
  scales of 3 or 4 bits are symbols (and Q2_K's scales and minimums), the grids' indices and the
  signs as they are;
- everything else (other types, padding, runs too short or that would not come to less) goes as
  it is, and runs of 1 MiB or more of it are left out of the applications.

Weights are close to noise: what is left to take is the order-0 entropy of the exponents
(BF16's 8 bits come to about 2.6) and of the quants (a Q4_K nibble 3.86 bits, Q8_0's high
nibble 3.6–3.7, Q6_K's 6 bits 5.86 as nibble and crumb). Contexts were measured and give
little: the exponent before (2.594 -> 2.592 bits), the low nibble of Q8_0 given its high one
(3.92 of 4). So the model is order-0 by table, and the work is in the speed.

## The coder

A static rANS of 32 interleaved 32-bit states, 16-bit words, probabilities of 15 bits, over
an alphabet of 16 symbols, in groups of 32 symbols of one table (FORMAT.md §3). As all the
states of a group have the same table, a symbol is found by comparing the slot with the 15
cumulative frequencies, and its frequency and start are looked up by a permutation (AVX2) or a
byte shuffle (SIMD128): no gather. The renormalisation reads the words of the states that need
one by a table of the mask of those states. Natively the AVX2 kernels are taken where the
processor has them (else one state at a time); the Wasm module runs SIMD128.

On a Ryzen 7 9700X, one thread, 64M symbols of a table of 3.67 bits (`timing::speed`):

| | Msym/s |
|---|---|
| encode, scalar (u32 division) | 374 |
| encode, scalar, branchless renormalisation, Lemire's division | 726 |
| encode, AVX2 (division by doubles, exact below 2^32) | 1555 |
| decode, AVX2 | 1900 |

## How it does

Measured 2026-11-12 on a Ryzen 7 9700X (16 threads, 125 GB), through `aqr -c` and `aqr -d` (the
default level, verification included), each file an archive of its own, the files read in place
from another disk; "before" is `aqr` before aqr-tensors (zstd on the file as it is). Every file came
back the same. (Q5_0, Q5_1 and Q5_K were coded after: Qwen3.5-4B Q4_K_M's 519 MB of Q5_K come to
2.3% less, by `aqr-tensors-bench`.)

| file | size | archive / input | before | compress | before | decompress | heap peak |
|---|---|---|---|---|---|---|---|
| gemma-4-26B mmproj BF16 | 1.19 GB | 0.6395 | 0.7614 | 428 MB/s | 150 MB/s | 1075 MB/s | 0.80 GB |
| Qwen3.8-Flash mmproj F16 | 0.90 GB | 0.6606 | 0.7643 | 407 MB/s | 124 MB/s | 925 MB/s | 0.78 GB |
| Qwen3.8-27B mmproj Q8_0 | 0.63 GB | 0.8078 | 0.8572 | 297 MB/s | 102 MB/s | 692 MB/s | 0.74 GB |
| translategemma-4b Q4_K_M | 2.49 GB | 0.9626 | 0.9830 | 350 MB/s | 135 MB/s | 1179 MB/s | 1.56 GB |
| gemma-4-E4B Q6_K | 6.22 GB | 0.9669 | 0.9955 | 330 MB/s | 134 MB/s | 1215 MB/s | 1.31 GB |
| gemma-4-E4B Q8_0 | 8.03 GB | 0.9412 | 0.9556 | 372 MB/s | 94 MB/s | 1314 MB/s | 0.79 GB |
| gemma-4-26B QAT Q4_0 | 14.44 GB | 0.8921 | 0.9283 | 321 MB/s | 140 MB/s | 1237 MB/s | 1.48 GB |
| Qwen3.5-9B BF16 | 17.92 GB | 0.6608 | 0.7788 | 392 MB/s | 160 MB/s | 1879 MB/s | 0.73 GB |
| Qwen3.8-27B Q4_K_M | 16.81 GB | 0.9624 | 0.9824 | 362 MB/s | 139 MB/s | 1454 MB/s | 2.00 GB |
| Qwen3.8-Flash UD-Q4_K_XL, 4 of 4 | 12.09 GB | 0.9656 | 0.9783 | 355 MB/s | 129 MB/s | 1315 MB/s | 2.16 GB |
| Qwen3.8-27B UD-IQ1_S | 6.19 GB | 0.9714 | 0.9952 | 458 MB/s | 393 MB/s | 2764 MB/s | 1.47 GB |
| Qwen3.8-27B UD-Q2_K_XL | 9.95 GB | 0.9765 | 0.9915 | 440 MB/s | 379 MB/s | 2980 MB/s | 1.30 GB |

The last two (types below 4 bits, IQ1–IQ3 most of them) were measured after, written directly
(below), "before" aqr-tensors without those types (and not written directly), decompressing by
`aqr -K` (2405 and 2450 MB/s before).

The coding itself (`aqr-tensors-bench`, 16 threads, the file cached) runs at 1.6–3.8 GB/s and
decodes at 5–6 GB/s (one thread: 330–670 MB/s coding, 550–1080 decoding). Through `aqr`, the
rest is the archive's (a 5.3 GB Q4_K_M, `--stats`): the coded streams compressed whole at zstd's
probe level to see whether they save (about 19 thread-seconds), then writing the batch and
verifying it, about 4.5 s of 13.6 on one thread.

A file of 1 GiB or more archived alone to a file is written directly
(`docs/streaming-encoder-design.md`, "출력에 바로 쓰기"): each segment's formula is sealed into the
archive's sink as it is made, its blocks written to the archive as they fill, and the formula
block put into a room left before them (its bound, `formula::formula_bound`), the rest a
spacer. A shard of 49.9 GB (Qwen3.8-Flash UD-Q4_K_XL, 2 of 4), which before grew to 80 GB of
memory (the spill's regions mapped, 155 of 256 MiB, and the input) and was stopped, now
archives to 47.72 GB (0.9572) in 188 s, resident peak 1.22 GB, spill peak 18.7 KB: 72 s to
write the archive, then 116 s verifying it on 1.8 threads. `aqr -K` gives it back at 1055 MB/s.
The 5.3 GB Q4_K_M: 18.65 -> 13.1–13.5 s, resident peak 15.89 -> 1.26 GB, spill peak 5.66 GB ->
15.8 MB, the archive 5,131,102,954 -> 5,131,401,818 B (the room's spacer, and the blocks in the
order they filled).

A `.pth` is a ZIP archive of stored entries: its pickle `data.pkl` first, then each storage an
entry `data/KEY`. The storages are found by the central directory and the persistent ids of the
pickle (`('storage', torch.FloatStorage, KEY, …, NUMEL)`), read by a machine of the opcodes that
keeps only strings, integers, globals and tuples: no Python object is made, nothing else of the
pickle is run. A safetensors file's JSON header is read for each tensor's `dtype` and
`data_offsets` alone.

Segment Anything's `sam_vit_h_4b8939.pth` (2.56 GB, F32: 594 storages, the rest 0.08 MB of
pickle and 0.4 MB of ZIP headers between them), measured 2026-11-12 the same way; before, it went
as a ZIP archive (aqr-zip, its entries zstd'd):

| | archive | archive / input | compress | decompress (`aqr -K`) | heap peak |
|---|---|---|---|---|---|
| before | 2,373,844,060 B | 0.9256 | 25.3 s (101 MB/s) | 736 MB/s | |
| aqr-tensors | 2,135,171,466 B | 0.8326 | 4.77 s (537 MB/s) | 2548 MB/s | 0.82 GB |

Its F32 come to their exponents' entropy (8 bits to about 2.6) and the 24 bits of sign and
mantissa as they are (no mantissa's low bits are zero throughout): `aqr-tensors-bench`, 213 MB of
symbols and 1923 MB raw of 2564.5 MB, coding at 4.1 GB/s, decoding at 5.2 GB/s (16 threads).

## Status

Left:

- The grids' indices of IQ1–IQ3 (8 to 11 bits) are as they are: they are near their entropy
  (measured on Qwen3.8-27B's UD-IQ1_S and UD-Q2_K_XL: IQ1_S's 11 bits 10.77, IQ2_XXS's 8 bits
  7.84–7.86, IQ2_S's 10 bits 9.63, IQ3_XXS's 8 bits 7.76–7.80; the index before as context
  10.77 -> 10.72), worth about 1–2% of those files more, for a coder of alphabets past 16
  symbols. The signs are at their entropy. The trellis types (ik_llama.cpp's KT) are kept as
  they are.
- The heap peak of the encoder reaches the budget on files of Q4_K and Q6_K (2.0–2.16 GB): the
  symbols of every segment in flight (17 of 16 MiB) and their words.
- Files of tens of GB in an archive of several files: a file is one leaf there, its batch held
  whole (docs/streaming-encoder-design.md, step 7); written directly only alone, to a file
  (not to standard output, encrypted, or mirrored).
- Within the archive, the streams coded are compressed at zstd's probe level all the same,
  whole, to see whether they would save (`How::Light`), which costs as many thread-seconds as
  the coding; and the archive's writing and verification of a large file are mostly on one
  thread (see "How it does").
- Native SIMD on AArch64 (NEON): the decoder and encoder run one state at a time there.
- `torch.save`: the legacy format (before PyTorch 1.6: pickles one after another, then the
  storages) and the pickles of protocols 0 and 1 are not taken; storages of other types (F64,
  integers, FP8) go as they are. F32's mantissas are kept whole: their top bits are not quite
  uniform within an exponent, which a model of them could take (not measured).
- safetensors: F64, FP8 (`F8_E4M3`, `F8_E5M2`) and integers go as they are. No real file was
  measured (the made-up ones of `synth` alone).
- Other containers of raw tensors (ONNX, NumPy's `.npy`/`.npz`, Keras' HDF5) are not taken.

## Tests

`cargo rt -p aqr-tensors` (made-up GGUF files of every type coded, files of `torch.save` and
safetensors, `synth`); `aqr-tensors-bench FILE [BYTES]` (the formula's streams, the speed of
encoding and of decoding natively, the file rebuilt and compared; `THREADS` for the threads);
`cargo test -r -p aqr-tensors --lib timing -- --ignored --nocapture` (the coder's speed).
aqr-decompress's `tensors_plans_hold` runs the Wasm module on them within its plan. Samples
(models of LM Studio's and Hugging Face's, Segment Anything's `.pth`) are read in place, not
copied.
