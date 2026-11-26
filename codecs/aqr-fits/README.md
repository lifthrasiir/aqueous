# aqr-fits

FITS files rebuilt bit for bit, HDU by HDU. `FORMAT.md` specifies the three modules
(`fits-recons-image`, `fits-recons-rice`, `fits-recons-table`) and how they work.

`aqr-fits-bench FILE...` measures a file against zstd alone (the formula's size with zstd -19 on
the streams that are not coded; `LEVEL` changes it).

## What is taken

- Integer images (`BITPIX` 8, 16, 32) and float images (-32, where that pays): `fits-recons-image`.
- Tile-compressed images of Rice tiles (fpack's `RICE_1`, quantised floats included):
  `fits-recons-rice`. Every tile of the corpus (fpack's, SDO/AIA synoptic images', the Legacy
  Survey's) is rebuilt.
- Binary tables' rows: `fits-recons-table`. Headers as text, and everything else as it is.
- Not taken: `GZIP_1`, `GZIP_2`, `HCOMPRESS_1` and `PLIO_1` tiles and compressed tables (kept as
  they are).

## Results

Apple M4, one thread, `aqr -c -T 1`, 2026-10-28, one archive per file (modules included), on 68
files (96.2 MB): the three FITS files of the cmz test set; SDO/AIA synoptic images (27, Rice);
Digitized Sky Survey cutouts (6, 16-bit) and those through `fpack`; Kepler light curves and
target pixel files (10, binary tables); NASA's FITS samples (11); an SDSS frame and a Legacy
Survey image (Rice of quantised floats). Sizes against the archive without this crate:

| | before | after |
|---|---|---|
| all, 96.2 MB | 68.83 MB (0.715) | 56.24 MB (0.584) |
| AIA (27, Rice tiles) | 1.000 | 0.937 |
| DSS (6, 16-bit) | 0.505 | 0.172 |
| DSS through fpack (6, Rice tiles) | 0.685 | 0.261 |
| Kepler (10, tables) | 0.591 | 0.307 |

The three cmz files: AIA20240101_0000_0171.fits 1258594 -> 1190578, dss_orion_10arcmin.fits
342640 -> 146858, kplr011904151-2009131105131_llc.fits 35644 -> 28002 (`xz -9e`: 1237472,
250564, 24088). Compressing the corpus took 13.5 s, decompressing 4.4 s. The largest file (the
Legacy Survey's 11.1 MB) takes 299 MB at peak to compress and 285 MB to decompress.

## Tests

`cargo rt -p aqr-fits`. The fixtures in `tests/data` are synthetic images, through fpack where
`.fz`; fpack/funpack (cfitsio 4.7) were the black boxes. The corpus (`corpus/`, not kept) is
fetched from the public archives (JSOC, STScI's DSS and MAST, fits.gsfc.nasa.gov, SDSS DR17,
the Legacy Surveys DR10).

## Left

- **Prediction mode.** Plain: AIA's tiles go to 0.95 of their Rice streams. A blend of
  predictors and better contexts are the next step.
- **Sampling.** Both modes are tried on a sample, which doubles the time of an image's coding.
- **Sparse values.** Images whose values are a sparse set (photographic plates' calibrations)
  would gain from coding the values' ranks.
- **Float images.** Left as they are where the model does not pay (most: noise in full
  mantissas). SDSS's frames (mantissas cut to 9 bits, sky around zero) would want an exact
  fixed-point mode (MED residuals of 10.0 bits a pixel against zstd's 12.4) coding each
  residual's trailing zeros first.
- **Module size.** The blocks (7.3 and 13.4 KB for `fits-recons-image` and `fits-recons-rice`,
  2.8 KB for `fits-recons-table`) outweigh the gains on small files.
- **Other tile compressions.** `GZIP_1`, `GZIP_2`, `HCOMPRESS_1`, `PLIO_1` and compressed
  tables (FITS Standard section 10.3).
