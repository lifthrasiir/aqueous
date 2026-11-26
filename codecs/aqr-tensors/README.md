# aqr-tensors

Files of tensors rebuilt bit for bit: GGUF model files (llama.cpp's), PyTorch's files of
`torch.save` (its ZIP format: `.pth`, `.pt`, `.ckpt`) and safetensors. The header (of a `.pth`,
its pickle and the entries' headers) goes as it is, and the tensor data in segments of 16 MiB,
each an application of the part module `tensors-recons`. Weights are close to noise, so what is
taken is the order-0 entropy of the floats' exponents and of the quantised blocks' quants, at
the speed of a SIMD rANS (500 MB/s through `aqr`). `FORMAT.md` specifies the module and how it
works.

```sh
cargo rt -p aqr-tensors
aqr-tensors-bench FILE [BYTES]   # THREADS sets the threads
cargo test -r -p aqr-tensors --lib timing -- --ignored --nocapture   # the coder's speed
```

`aqr-tensors-bench` prints the formula's streams, the speed of encoding and of decoding
natively, the file rebuilt and compared, and the size of the whole file with the side bytes and
the bytes not coded at zstd's level 3.

## What is taken

- **Floats** (BF16, F16, F32, F64, FP8 E4M3 and E5M2): exponents as symbols, the signs and
  mantissas as they are (without the low bits zero throughout); the top mantissa bits, zeros
  and subnormals, and rows of their own scale as symbols where a sample says it pays.
- **Grids**: floats that are a lower precision's values scaled by tiles (NVFP4, MXFP4, FP8 of
  blocks, NF4, GPTQ's and AWQ's INT4, INT8 of rows, converted to BF16, F16 or F32), found by a
  sample of the tensor and coded as indices and the tiles' scales.
- **Quantised blocks** (GGUF): the quants of Q2_K to Q6_K, Q8_0, IQ4_NL, IQ4_XS and MXFP4 as
  symbols, the scales to the side; of the codebook types (IQ1_S to IQ3_S) the scales of 3 or 4
  bits as symbols and the grids' indices and signs as they are.
- **Integers and bytes** (packed FP4, E8M0 scales): lanes of bytes, or their nibbles as symbols.
- Everything else (other types, padding, runs too short or that would not come to less) goes as
  it is; runs of 1 MiB or more of it are left out of the applications.
- A tensor of the same bytes as one before it in the file (tied embeddings: `lm_head.weight` as
  `model.embed_tokens.weight`) is that one's applications again; aqr-compress puts files of
  such a tensor in common in one batch (`shared_keys`, up to 16 GiB together).
- A file of 1 GiB or more archived alone to a file is written directly
  (`docs/streaming-encoder-design.md`): each segment's formula is sealed into the archive as it
  is made.
- Data of no header (an app's weights as they are) is taken where the attributes say so
  (docs/hints-design.md): `aqr-format=tensors` with `aqr-dtype=TYPE` (all the file one tensor of
  `bf16`, `f16`, `f32`, `f64`, `e4m3`, `e5m2`, or `i8` to `u64` as lanes; the bytes after its
  last whole element as they are) or `aqr-layout=FILE` (TOML, relative to the attributes'
  directory: each `[[tensor]]` of `shape` (outermost first; the last dimension its rows, the
  first the rows' classes) or `count`, `dtype` (else the file's top-level `dtype`, else
  `aqr-dtype`'s) and `offset` in bytes (else where the one before ends); other keys are left;
  tensors past the file's end or overlapping the one before are left out). A layout gives each
  tensor its own tables, which pays (`tensors::headerless`).
- Not taken: the trellis types (ik_llama.cpp's KT) as they are; `torch.save`'s legacy format
  (before PyTorch 1.6) and pickles of protocols 0 and 1; storages of quantised types
  (`QInt8Storage` and the like); ONNX, NumPy's `.npy`/`.npz`, Keras' HDF5; FP6's and FP4's
  bytes of safetensors are lanes of a byte (FP6's 4 values in 3 bytes not taken apart).

## Results

Ryzen 7 9700X (16 threads, 125 GB), 2026-11-12, through `aqr -c` and `aqr -d` (the default
level, verification included), each file an archive of its own, the files read in place from
another disk; every file came back the same. "Before" is `aqr` before aqr-tensors (zstd on the
file as it is).

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

The coding itself (`aqr-tensors-bench`, 16 threads) runs at 1.6-3.8 GB/s and decodes at 5-6
GB/s (one thread: 330-670 MB/s coding, 550-1080 decoding); the rest of `aqr`'s time is the
archive's (probing the coded streams, writing and verifying). On the 11 files of tenz's README
(github.com/nsiddiqui248/tenz; 0.4-17.65 GB, 35.26 GB in all; 2026-11-14), `aqr -c` takes 71.7
s (492 MB/s) and `aqr -K` 18.4 s. A shard of 49.9 GB (Qwen3.8-Flash UD-Q4_K_XL, 2 of 4) archives
to 47.72 GB (0.9572) in 188 s, resident peak 1.22 GB; `aqr -K` gives it back at 1055 MB/s.

**`.pth`.** Segment Anything's `sam_vit_h_4b8939.pth` (2.56 GB, F32; 594 storages), same
machine, against the ZIP archive of before (aqr-zip, its entries zstd'd):

| | archive | archive / input | compress | decompress (`aqr -K`) | heap peak |
|---|---|---|---|---|---|
| before | 2,373,844,060 B | 0.9256 | 25.3 s (101 MB/s) | 736 MB/s | |
| aqr-tensors | 2,135,171,466 B | 0.8326 | 4.77 s (537 MB/s) | 2548 MB/s | 0.82 GB |

**Tensors stored twice** (`aqr -c`, 2026-11-24): Qwen3-0.6B 997,171,204 -> 791,437,270 B (0.6633
-> 0.5265); Qwen3-1.7B's two shards 2,685,508,834 -> 2,277,827,611 B (0.6609 -> 0.5606).

**Conversions** (2026-11-12, the same way; "before" is `aqr` before the grids, lanes and the
other floats). Samples are in `F:\Data\aqr-corpus\tensors` (from Hugging Face; `fetch.sh`
there): real conversions are rare there, so `dequant/` has checkpoints of NVFP4
(compressed-tensors' and ModelOpt's), FP8 of 128 x 128 tiles, NF4 and GPTQ dequantised as the
conversion scripts do (`scale * value` in F32, then to BF16 or F16); gpt-oss-20b's BF16
(unsloth's, its MXFP4 experts dequantised; its first 2.5 GiB) is a real one.

| file | size | archive / input | before | compress | before | decompress | before |
|---|---|---|---|---|---|---|---|
| Qwen3-0.6B NVFP4 (c-t) to BF16 | 1.50 GB | 0.4340 | 0.5298 | 355 MB/s | 568 MB/s | 2951 MB/s | 3200 MB/s |
| Qwen2.5-0.5B GPTQ to F16 | 1.17 GB | 0.4176 | 0.6846 | 355 MB/s | 412 MB/s | 2886 MB/s | 1629 MB/s |
| gpt-oss-20b BF16 (MXFP4), 2.5 GiB | 2.68 GB | 0.2627 | 0.2828 | 237 MB/s | 179 MB/s | 2384 MB/s | 1986 MB/s |
| Qwen3-0.6B BF16 | 1.50 GB | 0.6633 | 0.6633 | 582 MB/s | 582 MB/s | 3360 MB/s | 3289 MB/s |
| Qwen3-0.6B FP8 (E4M3 weights) | 1.06 GB | 0.7328 | 0.7322 | 430 MB/s | 372 MB/s | 2451 MB/s | 2680 MB/s |

Whole files by `aqr-tensors-bench`: NVFP4 (ModelOpt) to BF16 0.6580 -> 0.3773, NF4 to BF16
0.6552 -> 0.3623, FP8 of tiles to BF16 0.6635 -> 0.5163. The checkpoints themselves (FP8, NVFP4,
NF4, MXFP4, GPTQ's and AWQ's I32) come to what zstd made of them, within 0.1-0.3%; natural
weights (BF16, F32) are unchanged. The grids' runs code at 0.8-0.9 GB/s (16 threads) and decode
at about 1 GB/s a thread.

**Zeros and rows of their own scale** (`cargo aqr -c`, 2026-11-24): the F16 weights of an OCR
model's two networks (PP-OCR's detector and recogniser, `docs/hints-design.md` section 5,
wrapped as safetensors):

| | det (30,999,306 B) | rec (38,244,272 B) |
|---|---|---|
| before | 14,005,585 | 31,747,761 |
| now | 12,480,081 (-10.9%) | 29,601,597 (-6.8%) |
| xz -6 | 13,285,052 | 30,962,616 |

They decode at 400-600 MB/s on 16 threads. Natural weights take neither (Llama-3.2-1B's BF16
codes to the same 1637.7 MB).

## Tests

- `tests/tensors.rs`: made-up GGUF files of every type coded, whole and in segments, the
  formula the same whatever the threads, damaged inputs told; made-up files of `torch.save` and
  safetensors of every float and integers; weights converted from six lower precisions coded as
  grids, whole and cut, and each as a run of the grid it was made of (`synth`).
- `tests/special.rs`: floats of every type with every bit pattern, the escaped tiles of grids,
  tensors that overlap or are unsorted, lying GGUF and safetensors headers, damaged parameters,
  a tensor over the 16 MiB segment's boundary: never a panic, hang or allocation the data
  cannot hold.
- `tests/edge.rs`: ZIP64 `torch.save` files, header lies, damaged inputs, the same promises.
- aqr-decompress's `tensors_plans_hold` runs the Wasm module on them within its plan.

Samples (models of LM Studio's and Hugging Face's, Segment Anything's `.pth`,
`F:\Data\aqr-corpus\tensors`) are read in place, not copied.

## Left

- **IQ1-IQ3 indices.** The grids' indices (8 to 11 bits) are as they are: near their entropy
  (IQ1_S's 11 bits 10.77, IQ2_XXS's 8 bits 7.84-7.86, IQ2_S's 10 bits 9.63, IQ3_XXS's 8 bits
  7.76-7.80; the index before as context 10.77 -> 10.72), worth about 1-2% of those files, for
  a coder of alphabets past 16 symbols.
- **Encoder heap.** The peak reaches the budget on files of Q4_K and Q6_K (2.0-2.16 GB): the
  symbols of every segment in flight (17 of 16 MiB) and their words.
- **Several large files in one archive.** A file of tens of GB is one leaf there, its batch
  held whole (docs/streaming-encoder-design.md, step 7); written directly only alone, to a file
  (not to standard output or mirrored).
- **Probing the coded streams.** The archive compresses them at zstd's probe level, whole, to
  see whether they save (`How::Light`): as many thread-seconds as the coding (zstd's
  long-distance matching). Not probing `tensors/syms` would make Qwen2.5-7B 8.65 -> 7.57 s,
  but 1.19 MB larger.
- **Side stream planning.** Each segment's `tensors/side` is planned by itself (`plan_of`: a
  sample tried at five levels), four times the thread-seconds of compressing it (Qwen2.5-7B:
  13.6 of 17.1; gemma-4-31B's take most of its 38 s). Alike streams of a class planned once,
  deterministically, would take that.
- **Reading and verifying.** The file's checksum on one thread (4-5 GB/s) and the archive's
  verification take 2-3 s of Gemma's 38.
- **Grids in Wasm.** A grid's values are decoded one at a time (SIMD128 has no conversion to
  F16; BF16's and F32's would take a lookup of 16 values by byte shuffles, as NEON's does).
- **Grids' speed and reach.** Coding (0.8-0.9 GB/s on 16 threads) is under the 500 MB/s
  through `aqr` where verification is added; the indices and the check of a tile's fit could
  be 8 at a time. Not fitted: zero points that are not integers (HQQ's), and conversions
  computed in F16 or BF16 step by step (`(q - z)` rounded, then times the scale rounded);
  those are escaped tiles, or floats. The rows of a `.pth`'s tensors are not read (its
  pickle's sizes), so its grids are of rows of the whole tensor alone. The scales go to the
  side as F32s (2 bits a weight of NVFP4's before the archive compresses them, to about 0.3);
  their own grid (FP8 times a global scale) is not looked for.
- **Lower-precision checkpoints.** FP8, packed FP4 and their scales come to about what zstd
  makes of them: their values are near their entropy order 0, and nothing is shared between a
  tensor and its scales.
