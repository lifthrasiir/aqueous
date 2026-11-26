# aqr-wim

WIM files (Windows install and recovery images) taken apart into their resources' data, so that
the data, which compresses far better together than in chunks of 32 KiB, is recompressed in turn
by aqr-compress. The LZX chunks of each resource are rebuilt bit for bit by aqr-lzx's module
`lzx-recons`, resources of no compression are as they are, the rest (header, chunk tables, lookup
table, XML data) is kept. The formula is applications of `lzx-recons` and slices, so this crate
has no module of its own; [FORMAT.md](FORMAT.md) tells what it holds, and how it works.

```sh
cargo run --release -p aqr-wim --example aqr-wim-check -- FILE [OFFSET LEN]   # taken apart, evaluated natively, timed
```

```rust
use aqr_wim::formula;
// None where it is no WIM file of LZX or of no compression, or no resource is taken apart
let f = formula::encode(&wim, threads).unwrap();
// `f` rebuilds `wim` with aqr-lzx's `formula::MODULE`
```

## What is taken

WIM files of LZX (chunks of 32768 bytes) and of no compression, in aqr-compress (at the start of
data). Not taken: XPRESS and LZMS (ESD files, solid LZMS), spanned WIMs (`.swm`).

## Results

The Windows Server 2008 R2 DVD (2026-10-30, 16 threads), every file given back:

| file | taken apart | formula, evaluated natively |
|---|---|---|
| install.wim, 2,809.4 MB (40,124 resources, 38,986 of LZX) | raw 6,857.7 MB, rest 16.8 MB, nothing kept | 32.1 s, 52.7 s |
| boot.wim, 168.6 MB | | 2.1 s, 2.4 s |

Through `aqr` (via aqr-iso, which gives install.wim as a file of its own) the DVD (3.17 GB)
archives to 2,483.1 MB, compressing in 170 s at 22.9 GB of peak commit and extracting in 70 s at
1.72 GB. aqr-lzx's README has what the records cost.

## Tests

`cargo rt -p aqr-wim`:

- WIM files made of aqr-lzx's fixture chunks (a resource once and twice, one of no compression,
  the lookup table and XML data), taken apart and given back.
- A chunk table off by one (kept), XPRESS (not taken), files cut short or changed.

## Left

- **XPRESS and LZMS.** WIMs of other compressions (and ESD files) are not taken.
- **Derived tables.** The lookup table's SHA-1s and the chunk tables are kept, not derived from the
  data.
- **Files of the image.** The resources are not cut into the image's files by its metadata, so
  files within a resource are not told apart.
- **Spanned WIMs** (`.swm`).
- **Compressing memory.** Compressing the DVD peaks at 22.9 GB of commit.
