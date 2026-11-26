# aqr-wwise

Audiokinetic Wwise's Vorbis media (`.wem`, and the media of banks, aqr-bundle's `wwise-bnk`).
A medium is a RIFF WAVE of format `FFFF` whose Vorbis stream has no identification or comment
header and a stripped setup header: each codebook is a 10-bit id into a library that Wwise's
runtime carries. The medium is split into its head, its packets (rebuilt by aqr-vorbis's
`vorbis-recons` from the record of their values, with the setup the stripped one stands for) and
its tail. [FORMAT.md](FORMAT.md) tells the media, the setup rebuilt and the split, and
[its design notes](FORMAT.md#design-notes) how the ids were found. aqr-compress takes media by
their header or the extension `.wem` (format `wem`), the banks' media among them.

```sh
cargo build --release --example aqr-wwise   # target/release/examples/aqr-wwise
aqr-wwise ids [--db FILE] FILE...|@LIST   # setups grouped, the ids' codebooks found
aqr-wwise recode FILE...|@LIST            # each medium through its formula and back; size, speed
```

## What is taken

Vorbis media with `vorb` fields in `fmt ` of size 0x2A whose setup has the 286 known ids, or is
found among aqr-vorbis's database skeletons. Not taken: other `vorb` sizes, and media whose
setup is none of those.

## Results

Seven games' media (82,570 Vorbis media, 26 setups), `recode`, 16 threads, Windows, Ryzen 7
9700X: 82,550 taken, all given back, 7011.6 -> 6349.8 MB of streams but the setups (0.906);
16.3 MB/s encoding and 15.5 MB/s decoding a thread. Through `aqr -c -T 16` (against
`1c50674d`):

| Input | Before | After |
|---|---|---|
| Unpacking (66 `.wem`, 3 banks, 15,492 media, 278.95 MB) | 270.28 MB, 1.9 s | 252.35 MB (-6.6%), 4.5 s |
| Chants of Sennaar (2 banks, 308.38 MB) | 307.01 MB, 1.9 s | 284.41 MB (-7.4%), 3.2 s |

`aqr -K -T 16` of Unpacking's archive: 3.9 s.

## Tests

- `tests/formula.rs`: media made here from setups of aqr-vorbis's database: their stripped
  setups read and made again, their formulas (the setup found by the table of ids or by a
  skeleton) giving them back through aqr-vorbis's module.
- `tests/riff.rs`: the walk of a medium's RIFF chunks (the pad byte after a chunk of odd size).

## Left

- **Other Wwise versions.** The ids of versions where the library differs (`vorb` chunks of
  other sizes, packets with granules).
- **aoTuV's setups.** Satisfactory's low-rate media (9 of 82,570) are kept as they are.
- **Fixed cost of a stream** in aqr-vorbis's encoder (about 0.24 ms: its keys, default tables and
  rANS tables).
