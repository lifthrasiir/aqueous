# aqr-fsb

FMOD's FSB5 sound banks. FSB5 keeps a Vorbis stream without its setup header, only the CRC-32 of
the header it was encoded with, which aqr-vorbis's setup database finds. The bank is split into a
stream per sample: Vorbis samples are rebuilt by aqr-vorbis's module from the records of their
packets' values with their setups from the database, IMA ADPCM samples by aqr-ima's from the
records of their blocks. [FORMAT.md](FORMAT.md) specifies the split (§2), the Vorbis samples
(§3) and the IMA ADPCM samples (§4); the database is aqr-vorbis's FORMAT.md §8.

```sh
aqr-fsb vorbis FILE... | -    # Vorbis samples parsed, written back, recoded; bits by field
aqr-fsb ima FILE... | -       # IMA ADPCM samples recoded and back; size and speed
```

## Results

See aqr-vorbis (The Crew's banks: 90.7-93.2% of the framed packets) and aqr-ima (57.8% of the
blocks).

## Tests

- `tests/fsb5.rs`: banks made here: layout, recognition by the start, non-banks, the formula of
  their head and samples giving them back (Vorbis through the module, IMA ADPCM, version 0).

## Left

- Nothing known.
