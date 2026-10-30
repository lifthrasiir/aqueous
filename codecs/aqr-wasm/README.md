# aqr-wasm

WebAssembly modules rebuilt bit for bit from streams, for the archive to compress: the
instructions of the code split by field (opcodes, local indices, labels, block types, offsets,
constants), local indices coded by their ranks among those used last, the sizes of the
functions and the code and data sections derived; the data segments' bytes and the custom
sections apart, as content for aqr-compress to find files in. Everything else is copied as it
is, LEB128s of any length included, so that the module `wasm-recons` needs to know nothing of
WebAssembly but where an instruction's immediates are. Functions it cannot walk (an opcode it
does not know) are kept as they are.

It is meant for the modules that archives carry above all (aqr-modules' `wasm/*.wasm`, its
corpus), and takes any module of the binary format's version 1, found anywhere in data.

```sh
cargo run --release -p aqr-wasm --example aqr-wasm-bench -- [-l LEVEL] FILE...  # round trip, zstd before and after, by stream
```

`FORMAT.md` specifies the module.

## Results

On the 56 modules of aqr-modules (3,596,538 B; a first build of wasm-recons among them), on an
M4:

| | as they are | aqr-wasm | |
|---|---|---|---|
| each alone, zstd -22 (as module blocks are made) | 1,025,036 B | 862,632 B | −15.8% |
| the directory, `aqr -c` at mid | 679,019 B | 539,527 B | −20.5% |
| the directory, `aqr -c` at high | 678,513 B | 531,247 B | −21.7% |
| each file alone, `aqr -c` at mid | 1,120,743 B | 1,072,373 B | −4.3% |

The archives are against 29fcaf4 and hold the module `wasm-recons` (7,871 B as its block),
which costs each archive of one file nearly as much as it saves on most of them. Splitting takes
169 MB/s, joining 185 MB/s (one thread). By stream (zstd -22, each stream of each module alone):
`op` 2.41 MB → 471 KB, `local` 667 → 233 KB, `data` 176 → 67 KB, `label` 106 → 31 KB, the
other ten 337 → 48 KB.

A dictionary would add as much again: zstd's of 64 KiB trained on half of the modules (and the
other way around) takes the modules as they are from 1,016,872 B to 838,752 B, and their
streams from 855,599 B to 717,779 B (−16.1%; 55 modules then).
