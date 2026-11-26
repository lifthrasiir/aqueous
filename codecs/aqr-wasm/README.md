# aqr-wasm

WebAssembly modules rebuilt bit for bit from streams, for the archive to compress: the code split
by field, local indices by rank, sizes derived, and the data segments and custom sections apart
as content for aqr-compress to find files in. Functions it cannot walk are kept as they are.
`FORMAT.md` specifies the module `wasm-recons`, and how it works.

It is meant for the modules that archives carry above all (aqr-modules' `wasm/*.wasm`, its
corpus), and takes any module of the binary format's version 1, found anywhere in data.

```sh
cargo run --release -p aqr-wasm --example aqr-wasm-bench -- [-l LEVEL] FILE...  # round trip, zstd before and after, by stream
```

## Results

On the 56 modules of aqr-modules (3,596,538 B; a first build of wasm-recons among them), on an
M4:

| | as they are | aqr-wasm | |
|---|---|---|---|
| each alone, zstd -22 (as module blocks are made) | 1,025,036 B | 862,632 B | −15.8% |
| the directory, `aqr -c` at mid | 679,019 B | 539,527 B | −20.5% |
| the directory, `aqr -c` at high | 678,513 B | 531,247 B | −21.7% |
| each file alone, `aqr -c` at mid | 1,120,743 B | 1,072,373 B | −4.3% |

The archives hold the module `wasm-recons` (7,871 B as its block), which costs each archive of one
file nearly as much as it saves on most of them. Splitting takes 169 MB/s, joining 185 MB/s (one
thread).

## Tests

```sh
cargo rt -p aqr-wasm
```

- `tests/wasm.rs`: round trips of aqr-modules' modules, odd LEB128s, unknown opcodes and sections kept as
  they are, corrupted streams never panicking.

## Left

- **Dictionary.** A zstd dictionary of 64 KiB trained on half of the modules takes the other half's
  streams from 855,599 B to 717,779 B (-16.1%, 55 modules): about as much again, at a cost of the
  dictionary's own bytes in the archive; not used.
