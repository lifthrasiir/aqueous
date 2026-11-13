# aqr-wwise

Audiokinetic Wwise's Vorbis media (`.wem`, and the media of banks, aqr-bundle's `wwise-bnk`).
A medium is a RIFF WAVE of format `FFFF` whose Vorbis stream has no identification or comment
header and a stripped setup header: each codebook is a 10-bit id into a library that Wwise's
runtime carries, and the fields that never vary are left out (a floor's and a mapping's type, a
mode's window and transform, the time-domain list, the framing flag; a residue's type in 2
bits). The audio packets leave out the packet type and the window flags. The floors, residues,
mappings and modes are all there, so only the ids' codebooks are missing to rebuild the setup.

- `wem` reads a medium's chunks, its `vorb` fields (in `fmt ` of 0x42 bytes; size 0x2A only)
  and its packets (each after a u16 size).
- `stripped` reads the stripped setup into its ids and the Vorbis setup's bits after the
  codebooks, assembles the setup from codebooks given for the ids, and puts the audio packets'
  first bits back.

```sh
aqr-wwise ids FILE...   # .wem and .bnk: setups grouped, the ids' codebooks found
```

`ids` tries every skeleton of aqr-fsb's database (libvorbis's setups) that has as many
codebooks on each setup, and takes those under which every packet parses to its end (no
packet cut short, at most 7 zero bits left). A wrong skeleton parses almost none (0 to 27 of
400).

## Status

On two games' media (Unpacking: 66 `.wem` and 3 banks; Chants of Sennaar: 3 banks; 16,834
Vorbis media, 2.13 M packets, 8 distinct setups of 1 and 2 channels, 44.1 and 48 kHz), each
setup has exactly one skeleton under which all of its packets parse exactly, and the ids agree
across setups: 118 ids found, each one codebook. Two ids (423, 438) are the same codebook, as
libvorbis's tables have it twice, which suggests that the library is libvorbis's books in the
order of its tables. The ids found are not yet stored as a table.

Left: the table of ids (and those of other Wwise versions, where the library differs: older
ones have aoTuV's or no library, `vorb` chunks of other sizes, packets with granules); the
recompressor itself, as aqr-fsb's: the setup rebuilt, the packets recoded by aqr-vorbis.
