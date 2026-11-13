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
aqr-wwise ids [--db FILE] FILE...|@LIST   # setups grouped, the ids' codebooks found
```

`ids` takes `.wem` files, banks, and any other file holding media (Unreal's paks, No Man's
Sky's audio paks: RIFF WAVEs of format `FFFF` found anywhere). It tries every skeleton of
aqr-vorbis's setup database (libvorbis's setups; `--db` another one) that has as many codebooks
on each setup, and takes those under which every packet parses to its end (no packet cut short,
at most 7 zero bits left). A wrong skeleton parses few (0 to 27 of 400). Where none does, it
seeks the codebooks one place at a time.

## Status

On seven games' media (Unpacking, Chants of Sennaar, Satisfactory, ASTRONEER and No Man's Sky
of Steam; Alba and The Alto Collection of Epic): 82,366 Vorbis media, 26 distinct setups of 1
to 6 channels (more than two coded with the mono setups, as FMOD does), 44.1 and 48 kHz. Under
the database's skeletons (those of stock libvorbis 1.3.7 among them: the blocks of 512 and 4096
of two of Alba's setups are not FMOD's), 25 setups have exactly one skeleton under which their
packets parse, and the 286 ids seen agree across setups, each one codebook. One setup (Satisfactory, 9 media: stereo at 6 to 8 kHz, blocks of 512) is not found:
no skeleton fits and the descent over the codebooks known stops at 29 of 200 packets, so its
books are not libvorbis 1.3.7's (aoTuV's, perhaps).

The packets left out under the skeletons found end early with zero bits only, so they do not
tell against them: a stream's last packet often ends 21 to 28 bits early, and in some of No
Man's Sky's media runs of packets end far early (silence, padded). Two ids (423, 438) are the
same codebook, as libvorbis's tables have it twice, which suggests that the library is
libvorbis's books in the order of its tables. The ids found are not yet stored as a table.

Left: the table of ids (and those of other Wwise
versions, where the library differs: `vorb` chunks of other sizes, packets with granules); the
media cut short in banks (prefetched streams: 184 whose packets run past their data); the
recompressor itself, as aqr-fsb's: the setup rebuilt, the packets recoded by aqr-vorbis, the
zero tails kept.
