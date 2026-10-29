# aqr-hdf5

HDF5 recompression (netCDF-4, NeXus, NWB, AnnData, Keras weights and the like are HDF5 files).
The deflated chunks of an HDF5 file's datasets are rebuilt bit for bit by aqr-flate's module
(`flate-set`, one application for all of them), so that the archive stores their data instead;
the rest of the file is a stream as it is. The records are of aqr-flate's profile as
aqr-compress picks it for any DEFLATE stream: the default (static, fast) below level 7, the
adaptive one with zlib emulation from 7 on. The HDF5 library deflates with zlib's `compress2`,
so that with the adaptive one every chunk of the corpus is emulated exactly (levels 1, 4, 6
and 9 seen), for next to no record; with the default one, the records of zlib's level 1 (GOES,
GOES) come to about a sixth of the streams, and the archive keeps such files as they are.

```sh
cargo run --release -p aqr-hdf5 --example aqr-hdf5-check -- [-v] [-t N] [-p PROFILE] FILE_OR_DIR...
```

```rust
use aqr_hdf5::formula;
// None where it is not an HDF5 file, or no chunk is rebuilt
let f = formula::encode(&file, 8).unwrap();
// `f` rebuilds `file` when evaluated with `formula::apply` (`FLOAT` and aqr-flate's module)
```

`FORMAT.md` specifies the module `hdf5-recons-float` and what the formula stores. The file is
read from its superblock (versions 0 to 3, a user block before it) through the groups (symbol
tables, link messages, dense links in fractal heaps) to the datasets' chunk indexes (v1
B-trees, single chunk, fixed arrays); on the corpus, it finds every filtered chunk that h5py
lists.

Datasets of floats (binary32 and binary64, shuffled or not) are coded by a model of their own,
whose module `hdf5-recons-float` gives aqr-flate their chunks' data: a linear predictor of
order 32 of the samples before (its coefficients by least squares for each block of 65536),
the difference from the prediction taken between the samples' integers of the floats' order,
its length in bits arithmetic-coded in the context of the lengths before, the two bits below
its leading one too and the others as they are. The low bits zero in every sample are left out
(GWOSC's Virgo strain is binary32 values in binary64). The noise in a float's mantissa does not
compress, but its sign, exponent and leading bits are predicted well: a GWOSC strain sample
takes 48 bits of 64.

## Results

Measured 2026-10-30 on Windows (16 threads), `cargo aqr -c` one archive per file, modules
included, decompression checked, against `aqr` before (which kept HDF5 files as they are), on a
corpus of 60 files (271.2 MB; not in the repository, fetched from the public archives): GWOSC
strain (6 files of 32 s and one of 4096 s, 129 MB: the cmz test set's among them), GOES-16 ABI
and GLM netCDF-4 (5), NOAA OISST (1), Keras weights (3), a 10x Genomics matrix and an AnnData
file (2), NWB (2), NeXus samples (9), and the HDF5 library's and PyTables' test files (31,
small).

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

For reference, `xz -9e` makes 1018200 bytes of the cmz file. Of its archive, 86 KB are the
modules (`flate-set`'s block is 78.7 KB, `hdf5-recons-float`'s 7.2 KB), which archives of
several files share: four GWOSC files of 32 s make 3.20 MB together. Files without deflated
chunks (Keras weights, NWB, most NeXus samples, the test files) are as before. Compression of
the corpus took 31.5 s by default and 44.5 s at `-9` (one file at a time, the round trip
included).

## Status

Left:

- The archive of a single small file pays for `flate-set` (78.7 KB) to rebuild zlib's streams;
  a module of zlib's deflate alone would be a fraction of it.
- At the default level, aqr-flate's records of zlib's level 1 come to about a sixth of GOES's
  streams (shuffled 16-bit integers), which are kept as they are; GWOSC's (floats) cost little.
- Integer data: images (GOES's 12-bit radiances, OISST) by a 2D predictor (aqr-fits' pixel
  model), shuffle undone; floats of few values (AnnData's counts) by their ranks. The float
  model predicts along the chunk as one line: images of floats would want the row above.
- Chunks of other filters (LZF, Blosc, Zstandard, szip, LZO) are kept as they are; LZF's and
  Blosc's (of LZ4) encoders are deterministic and could be emulated.
- Extensible array and v2 B-tree indexes (layout 4: datasets of unlimited dimensions written
  by libver 1.10 on) are not read, nor implicit ones (no filters).
- The float model's speed: about 35 MB/s to encode a file (prediction of order 32 and the
  arithmetic coding of each sample) on one thread a dataset.

## Tests

`cargo rt -p aqr-hdf5`. The fixtures in `tests/data` are synthetic data written by h5py
(`tests/data/make.py`).
