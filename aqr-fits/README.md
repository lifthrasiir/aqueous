# aqr-fits

FITS files rebuilt bit for bit (`FORMAT.md`), HDU by HDU:

- integer images (`BITPIX` 8, 16, 32) and float images (−32, where that pays) coded by a pixel
  model: a median predictor with a bias correction, the residuals' lengths in bits in a
  context of the activity around them by adaptive binary arithmetic coding, their lower bits
  as they are;
- tile-compressed images of Rice tiles (fpack's `RICE_1`, quantised floats included), their
  pixels taken out and coded by the same model, the tiles' streams rebuilt from them and from
  the codes of their blocks, which are kept (coded in the context of the shortest code);
- binary tables' rows as byte planes of their columns, perhaps as differences from the row
  above, for the archive to compress;
- headers as text, and everything else as it is.

The Rice coder's choices are not reproduced: no encoder's code was read for this crate but
the FITS Standard's description, and fpack/funpack (cfitsio 4.7) were used as black boxes for
the tests' fixtures. Every tile of the corpus (fpack's, the SDO/AIA synoptic images', the
Legacy Survey's) is rebuilt.

## How it does

Measured 2026-10-28 on an Apple M4, one thread, through `aqr -c -T 1` (one archive per file,
modules included), on a corpus of 68 files (96.2 MB): the three FITS files of the cmz test
set; SDO/AIA synoptic images (27, Rice); Digitized Sky Survey cutouts (6, 16-bit) and those
through `fpack`; Kepler light curves and target pixel files (10, binary tables); NASA's FITS
samples (11); an SDSS frame (float image and tables) and a Legacy Survey image (Rice of
quantised floats).

| | before | after |
|---|---|---|
| all, 96.2 MB | 68.83 MB (0.715) | 63.37 MB (0.658) |
| AIA (27, Rice tiles) | 1.000 | 0.951 |
| Kepler (10, tables) | 0.591 | 0.307 |
| cmz: AIA20240101_0000_0171.fits | 1258594 | 1197140 |
| cmz: dss_orion_10arcmin.fits | 342640 | 340940 |
| cmz: kplr011904151-2009131105131_llc.fits | 35644 | 28002 |

Compression of the corpus took 9.0 s (2.3 before), decompression 3.2 s (0.6).

## Status

Left:

- The pixel model is plain; AIA's tiles go to 0.95 of their Rice streams. A blend of
  predictors and better contexts are the next step.
- Digitized Sky Survey images were H-transform coded (hcompress) before they were
  distributed: their 2×2 blocks' coefficients, at the right offset, have half the entropy of
  the pixels' residuals. The model does not see it, and zstd does better on them for now.
- Images whose values are a sparse set (photographic plates' calibrations) would gain from
  coding the values' ranks.
- Float images are left as they are where the model does not pay (most of them).
- The modules' blocks (6.5 and 12.6 KB for image and Rice) outweigh the gains on small files.
- `GZIP_1`, `GZIP_2`, `HCOMPRESS_1` and `PLIO_1` tiles are kept as they are, and so are
  compressed tables (§10.3).

## Tests

`cargo rt -p aqr-fits`; `aqr-fits-bench FILE...` (the formula's size with zstd -19 on the
streams that are not coded, `LEVEL` to change it, against zstd alone). The fixtures in
`tests/data` are synthetic images, through fpack where `.fz`. The corpus (`corpus/`, not
kept) is fetched from the public archives (JSOC, STScI's DSS and MAST, fits.gsfc.nasa.gov,
SDSS DR17, the Legacy Surveys DR10).
