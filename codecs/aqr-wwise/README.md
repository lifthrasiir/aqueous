# aqr-wwise

Audiokinetic Wwise's Vorbis media (`.wem`, and the media of banks, aqr-bundle's `wwise-bnk`).
A medium is a RIFF WAVE of format `FFFF` whose Vorbis stream has no identification or comment
header and a stripped setup header: each codebook is a 10-bit id into a library that Wwise's
runtime carries, and the fields that never vary are left out (a floor's and a mapping's type, a
mode's window and transform, the time-domain list, the framing flag; a residue's type in 2
bits). The audio packets leave out the packet type and the window flags. The floors, residues,
mappings and modes are all there, so only the ids' codebooks are missing to rebuild the setup.

- `wem` reads a medium's chunks, its `vorb` fields (in `fmt ` of 0x42 bytes; size 0x2A only)
  and its packets (each after a u16 size; a medium cut short ends inside one).
- `stripped` reads the stripped setup into its ids and the Vorbis setup's bits after the
  codebooks, assembles the setup from codebooks given for the ids, and strips one back.
- `ids` is the table of the ids known (286), each with its codebook part in aqr-vorbis's setup
  database.
- `formula` splits a medium into its head (up to the first audio packet, the stripped setup
  among it), its packets, rebuilt by aqr-vorbis's module `vorbis-recons` (stripped packets)
  from the record of their values with the setup the stripped one stands for, and its tail.
  A setup with an id the table has not is sought among the database's skeletons by the
  medium's first packets. Parsed setups are cached (a few setups serve many media).

`FORMAT.md` tells the media, the setup rebuilt and the split. aqr-compress takes media by their
header or the extension `.wem` (format `wem`), the banks' media among them.

```sh
aqr-wwise ids [--db FILE] FILE...|@LIST   # setups grouped, the ids' codebooks found
aqr-wwise recode FILE...|@LIST            # each medium through its formula and back; size, speed
```

`ids` takes `.wem` files, banks, and any other file holding media (Unreal's paks, No Man's
Sky's audio paks: RIFF WAVEs of format `FFFF` found anywhere). It tries every skeleton of
aqr-vorbis's setup database (libvorbis's setups; `--db` another one) that has as many codebooks
on each setup, and takes those under which every packet parses to its end (no packet cut short,
at most 7 zero bits left). A wrong skeleton parses few (0 to 27 of 400). Where none does, it
seeks the codebooks one place at a time.

## Status

On seven games' media (Unpacking, Chants of Sennaar, Satisfactory, ASTRONEER and No Man's Sky
of Steam; Alba and The Alto Collection of Epic): 82,570 Vorbis media, 26 distinct setups of 1
to 6 channels (more than two coded with the mono setups, as FMOD does), 44.1 and 48 kHz. Under
the database's skeletons (those of stock libvorbis 1.3.7 among them: the blocks of 512 and 4096
of two of Alba's setups are not FMOD's), 25 setups have exactly one skeleton under which their
packets parse, and the 286 ids seen agree across setups, each one codebook. One setup
(Satisfactory, 9 media: stereo at 6 to 8 kHz, blocks of 512) is not found: no skeleton fits and
the descent over the codebooks known stops at 29 of 200 packets, so its books are not libvorbis
1.3.7's (aoTuV's, perhaps).

`recode` on all of them (16 threads, Windows, Ryzen 7 9700X): 82,550 media taken, all given
back, 7011.6 -> 6349.8 MB of streams but the setups (0.906: the records 6286.4 MB); 16.3 MB/s
encoding and 15.5 MB/s decoding a thread. Through `aqr -c -T 16` (against `1c50674d`):

| | before | after |
|---|---|---|
| Unpacking (66 `.wem`, 3 banks, 15,492 media, 278.95 MB) | 270.28 MB, 1.9 s | 253.28 MB (−6.3%), 12.6 s |
| Chants of Sennaar (2 banks, 308.38 MB) | 307.01 MB, 1.9 s | 285.34 MB (−7.1%), 4.0 s |

Unpacking's media are small (18 KB on average): a medium's decode (for the archive's check)
parses its setup and makes its tables again each time, and that, not the encode, is most of the
time.

The packets that do not end exactly end early with zero bits only, so they do not tell against
the setups found: a stream's last packet often ends 21 to 28 bits early, and in some of No Man's
Sky's media runs of packets end far early (silence, padded); the record keeps their tails. Two
ids (423, 438) are the same codebook, as libvorbis's tables have it twice, which suggests that
the library is libvorbis's books in the order of its tables.

Left: the ids of other Wwise versions, where the library differs (`vorb` chunks of other sizes,
packets with granules); aoTuV's setups (Satisfactory's low rates); the decode's cost a medium.
