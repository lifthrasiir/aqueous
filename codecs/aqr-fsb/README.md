# aqr-fsb

FMOD's FSB5 sound banks. FSB5 keeps a Vorbis stream without its setup header, only the CRC-32 of
the header it was encoded with, which aqr-vorbis's setup database (`setups`) finds.

`fsb5` reads a bank's layout (its head, then each sample's data and header), and `formula`
splits a bank into a stream per sample, the Vorbis samples rebuilt by aqr-vorbis's module from
the records of their packets' values with their setups from the database, the IMA ADPCM samples
by aqr-ima's from the records of their blocks.

```sh
aqr-fsb vorbis FILE... | -    # Vorbis samples parsed, written back, recoded; bits by field
aqr-fsb ima FILE... | -       # IMA ADPCM samples recoded and back; size and speed
```

`FORMAT.md` specifies the split (§2), the Vorbis samples (§3) and the IMA ADPCM samples (§4);
the database is aqr-vorbis's FORMAT.md §8.
