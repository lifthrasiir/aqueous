# aqr-hdf5

HDF5 recompression (netCDF-4, NeXus, NWB, AnnData, Keras weights and the like are HDF5 files).
The deflated chunks of an HDF5 file's datasets are rebuilt bit for bit by aqr-flate's module
(`flate-set`, one application for all of them), so that the archive stores their data instead;
the rest of the file is a stream as it is. Datasets of floats get a model of their own
(`hdf5-recons-float`). `FORMAT.md` specifies the module and what the formula stores, and how it
works.

```sh
cargo run --release -p aqr-hdf5 --example aqr-hdf5-check -- [-v] [-t N] [-p PROFILE] FILE_OR_DIR...
```

```rust
use aqr_hdf5::formula;
// None where it is not an HDF5 file, or no chunk is rebuilt
let f = formula::encode(&file, 8).unwrap();
// `f` rebuilds `file` when evaluated with `formula::apply` (`FLOAT` and aqr-flate's module)
```

## What is taken

- HDF5 files from the superblock (versions 0 to 3, a user block before it) through the groups
  to the datasets' chunk indexes (v1 B-trees, single chunk, fixed arrays): every filtered chunk
  that h5py lists on the corpus is found.
- Deflated chunks (with shuffle and Fletcher-32 where they are): rebuilt by `flate-set`, at the
  profile asked for; below level 7 the adaptive profile with zlib emulation where it predicts
  the chunks (the HDF5 library deflates with zlib's `compress2`).
- Datasets of floats (binary32 and binary64, shuffled or not): a linear predictor of order 32,
  the module `hdf5-recons-float`. A GWOSC strain sample takes 48 bits of 64 (the noise in the
  mantissa does not compress).
- Not taken: chunks of other filters (LZF, Blosc, Zstandard, szip, LZO) are kept as they are.

## Results

Windows, 16 threads, 2026-10-30 (the default column 2026-11-10), `aqr -c`, one archive per
file, modules included, decompression checked, on 60 files (271.2 MB; fetched from the public
archives, not in the repository): GWOSC strain (6 files of 32 s and one of 4096 s, 129 MB: the
cmz test set's among them), GOES-16 ABI and GLM netCDF-4 (5), NOAA OISST (1), Keras weights
(3), a 10x Genomics matrix and an AnnData file (2), NWB (2), NeXus samples (9), and the HDF5
library's and PyTables' test files (31, small). "Before" is the archive keeping HDF5 files as
they are.

| | before | default level | `-9` |
|---|---|---|---|
| all, 271.2 MB | 224.02 MB (0.826) | 197.49 MB (0.728) | 190.97 MB (0.704) |
| GWOSC, 4096 s (1, 129.0 MB) | 1.000 | 0.819 | 0.799 |
| GWOSC, 32 s (6, deflate 1) | 1.000 | 0.826 | 0.812 |
| GOES (5, shuffle + deflate 1) | 0.928 | 0.928 | 0.837 |
| AnnData, 10x (2, deflate 4) | 0.949 | 0.905 | 0.848 |
| NeXus (9) | 0.412 | 0.412 | 0.373 |
| NOAA OISST (1) | 0.976 | 0.976 | 0.965 |
| cmz: H-H1_GWOSC_4KHZ_R1-1126259447-32.hdf5 | 1040814 | 880760 | 880672 |

`xz -9e` makes 1018200 bytes of the cmz file. Of its archive, 76 KB are the modules
(`flate-set` 71.7 KB, `hdf5-recons-float` 4.5 KB), which archives of several files share: four
GWOSC files of 32 s make 3.20 MB together. Files without deflated chunks (Keras weights, NWB,
most NeXus samples, the test files) are as before. Compressing the corpus (one file at a time,
the round trip included) took 31.5 s by default and 44.5 s at `-9`; a float dataset encodes at
about 35 MB/s a thread.

## Tests

- `tests/hdf5.rs`: files that h5py wrote of synthetic data (`tests/data`, made by
  `tests/data/make.py`) through the formula, evaluated back through aqr-flate's module;
  corrupted inputs.
- `tests/edge.rs`: edge cases built in the test (the four superblocks, float32 and big-endian
  datasets, sizes the file lies about, a fractal heap whose sizes overflow) and damage to all of
  them: everything taken evaluates back, nothing panics, hangs or allocates what the data
  cannot hold.

`cargo rt -p aqr-hdf5`.

## Left

- **Module size.** The archive of a single small file pays for `flate-set` (47.3 KB) to rebuild
  zlib's streams. A module of exact records alone was measured (36.6 KB) and not kept: an
  archive holding both pays for both, and 10 KB is little where it alone is used.
- **Integer data.** Images (GOES's 12-bit radiances, OISST) by a 2D predictor (aqr-fits' pixel
  model), shuffle undone; floats of few values (AnnData's counts) by their ranks. The float
  model predicts along the chunk as one line: images of floats would want the row above.
- **Other filters.** LZF's and Blosc's (of LZ4) encoders are deterministic and could be
  emulated.
- **Other indexes.** Extensible array and v2 B-tree indexes (layout 4: datasets of unlimited
  dimensions written by libver 1.10 on) are not read, nor implicit ones (no filters).
- **Float speed.** About 35 MB/s to encode (prediction of order 32 and the arithmetic coding of
  each sample) on one thread a dataset.
