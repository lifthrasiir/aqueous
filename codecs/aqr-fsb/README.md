# aqr-fsb

FMOD's FSB5 sound banks. FSB5 keeps a Vorbis stream without its setup header, only the CRC-32 of
the header it was encoded with. So far the crate has the Vorbis setup database
(`data/vorbis-setups.bin`) and `setups`, which parses it, assembles a setup header from a
skeleton and `end`s, and finds the setup of a CRC-32: by its row, or by a search over all
skeletons that uses CRC-32's affinity (meet in the middle over the residues' ends).

`fsb5` reads a bank's layout (its head, then each sample's data), and `formula` splits a bank
into a stream per sample; aqr-compress does not use it until a sample recompressor does.

`FORMAT.md` specifies the database, the assembly and the search (§3–5), and the split (§7).

Tests take banks of a local corpus from the directory `AQR_FSB_CORPUS` names, if set; game banks
are not distributed with the repository.
