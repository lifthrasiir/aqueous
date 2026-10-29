# aqr-hdf5

HDF5 recompression (netCDF-4, NeXus, NWB, AnnData, Keras weights and the like are HDF5 files).
The deflated chunks of an HDF5 file's datasets are rebuilt bit for bit by aqr-flate's module
(`flate-set`, one application for all of them), so that the archive stores their data instead;
the rest of the file is a stream as it is. The records are of aqr-flate's profile as
aqr-compress picks it for any DEFLATE stream: the default (static, fast) below level 7, the
adaptive one with zlib emulation from 7 on. The HDF5 library deflates with zlib's `compress2`,
so that with the adaptive one every chunk of the corpus is emulated exactly (levels 1, 4, 6
and 9 seen), for next to no record; with the default one, the records of zlib's level 1 (GOES,
GWOSC) come to about a sixth of the streams, and the archive mostly keeps such files as they
are.

```sh
cargo run --release -p aqr-hdf5 --example aqr-hdf5-check -- [-v] [-t N] [-p PROFILE] FILE_OR_DIR...
```

```rust
use aqr_hdf5::formula;
// None where it is not an HDF5 file, or no chunk is rebuilt
let f = formula::encode(&file, 8).unwrap();
// `f` rebuilds `file` when evaluated with `formula::apply` (aqr-flate's module)
```

`FORMAT.md` tells what is read of a file and what the formula stores. The file is read from
its superblock (versions 0 to 3, a user block before it) through the groups (symbol tables,
link messages, dense links in fractal heaps) to the datasets' chunk indexes (v1 B-trees,
single chunk, fixed arrays); on the corpus, it finds every filtered chunk that h5py lists.

## Results

Measured 2026-10-30 on Windows (16 threads), `aqr -c` one archive per file, modules included,
against `aqr` before (which kept HDF5 files as they are), on a corpus of 60 files (271.2 MB;
not in the repository, fetched from the public archives): GWOSC strain (6 files of 32 s and
one of 4096 s, 129 MB: the cmz test set's among them), GOES-16 ABI and GLM netCDF-4 (5), NOAA
OISST (1), Keras weights (3), a 10x Genomics matrix and an AnnData file (2), NWB (2), NeXus
samples (9), and the HDF5 library's and PyTables' test files (31, small).

| | before | default level | `-9` |
|---|---|---|---|
| all, 271.2 MB | 224.02 MB (0.826) | 222.84 MB (0.822) | 218.13 MB (0.804) |
| GOES (5, shuffle + deflate 1) | 0.928 | 0.928 | 0.837 |
| AnnData, 10x (2, deflate 4) | 0.949 | 0.911 | 0.845 |
| NeXus (9) | 0.412 | 0.412 | 0.373 |
| NOAA OISST (1) | 0.976 | 0.976 | 0.965 |
| GWOSC, 32 s (6, deflate 1) | 1.000 | 0.997 | 0.976 |
| GWOSC, 4096 s (1, 129.0 MB) | 1.000 | 1.000 | 0.997 |
| cmz: H-H1_GWOSC_4KHZ_R1-1126259447-32.hdf5 | 1040814 | 1040814 | 1040814 |

For reference, `xz -9e` makes 1018200 bytes of the cmz file. Files without deflated chunks
(Keras weights, NWB, most NeXus samples, the test files) are as before.

## Status

Left:

- The data as it is gains little over its DEFLATE streams where it is noise: GWOSC's strain
  (float64 of full mantissas) is the archive's to compress as bytes, about as zlib's level 1
  did. A float model (linear prediction of order 32 leaves residuals of 0.004 of the signal,
  some 8 bits of 64 a sample) is the next step; then integer images (GOES's 12-bit radiances,
  by aqr-fits' pixel model) and undoing shuffle.
- Chunks of other filters (LZF, Blosc, Zstandard, szip, LZO) are kept as they are; LZF's and
  Blosc's (of LZ4) encoders are deterministic and could be emulated.
- Extensible array and v2 B-tree indexes (layout 4: datasets of unlimited dimensions written
  by libver 1.10 on) are not read, nor implicit ones (no filters).
- Files with a user block (MATLAB 7.3) are taken at the start of a file only; none is in the
  corpus.

## Tests

`cargo rt -p aqr-hdf5`. The fixtures in `tests/data` are synthetic data written by h5py
(`tests/data/make.py`).
