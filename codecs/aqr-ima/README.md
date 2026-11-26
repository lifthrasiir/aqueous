# aqr-ima

IMA ADPCM blocks as a record of their headers and nibbles, and back bit for bit: Xbox ADPCM (of
FSB5 banks, through aqr-fsb) and Microsoft's IMA ADPCM (of WAV files, through aqr-raw) alike, by
their channels and bytes per channel. The module `ima-recons` rebuilds whole blocks;
[FORMAT.md](FORMAT.md) specifies it and how it works: a binary arithmetic code of adaptive bits
in contexts of the IMA decoder's state.

## What is taken

Any bytes of whole blocks (a tail that is not a whole block is the caller's).

## Results

| Input | Record / blocks | Compressing | Decompressing |
|---|---|---|---|
| The Crew's FSB5 banks (817 samples, 100.2 MB of blocks) | 57.8% (`xz -9e`: about 67%) | 33 MB/s | 25 MB/s |
| IMA ADPCM WAV of ffmpeg and sox, stereo / mono | 0.58-0.61 / 0.78 (aqr-raw's README) | | |

One thread, Windows (date not recorded).

## Tests

- `tests/codec.rs`: blocks of IMA ADPCM encoded here (mono, stereo of equal channels, six
  channels) and of noise (headers of any index and reserved byte), in the Xbox and Microsoft
  layouts, through the record and back; records that are not one.
- `tests/edge.rs`: the extremes (255 channels, saturated predictors and step indices, every
  nibble), records that lie about the length, damaged records.

## Left

- Nothing known.
