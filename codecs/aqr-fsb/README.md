# aqr-fsb

FMOD's FSB5 sound banks. FSB5 keeps a Vorbis stream without its setup header, only the CRC-32 of
the header it was encoded with. The crate has the Vorbis setup database
(`data/vorbis-setups.bin`) and `setups`, which parses it, assembles a setup header from a
skeleton and `end`s, and finds the setup of a CRC-32: by its row, or by a search over all
skeletons that uses CRC-32's affinity (meet in the middle over the residues' ends).

`fsb5` reads a bank's layout (its head, then each sample's data and header), and `formula`
splits a bank into a stream per sample, the Vorbis samples rebuilt by aqr-vorbis's module from
the records of their packets' values with their setups from the database, the IMA ADPCM samples
by aqr-ima's from the records of their blocks.

```sh
aqr-fsb vorbis FILE... | -    # Vorbis samples parsed, written back, recoded; bits by field
aqr-fsb ima FILE... | -       # IMA ADPCM samples recoded and back; size and speed
```

`FORMAT.md` specifies the database, the assembly and the search (§3–5), the split (§7), the
Vorbis samples (§8) and the IMA ADPCM samples (§9).
