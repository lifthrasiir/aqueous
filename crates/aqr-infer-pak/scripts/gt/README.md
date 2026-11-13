# Ground truths of paks

The inference measured against the ideal: a sample's directories parsed by hand (a parser of
its format here, printing `relative/path<TAB>offset<TAB>size` a member a line), then the sample
archived by aqr three ways (`eval.py`): with no paks (`AQR_PAKS=0`), inferred (the plan
written by `AQR_PAK_DUMP`), and given the ground truth (`AQR_PAK_LAYOUT`), the sizes and times
of each and how many members the inference got right. `scancov.py` tells how many of the
members the scan finds by itself (`AQR_SCAN_DUMP`).

The switches are in aqr builds not of the dist profile (`aqr_dist`, aqr-compress's `build.rs`).
Copy a sample into a directory of its own: aqr plans paks only for archives of several files.

```sh
python -I baba.py "…/Baba Is You/Assets.dat" > baba.txt
python -I eval.py AQR baba baba.txt DIR
python -I scancov.py AQR baba.txt DIR
```

| sample | parser | members |
|---|---|---|
| Baba Is You, `Assets.dat` | `baba.py FILE` | (offset, size) after a u32 table length |
| Waveform, `*.fc` | `waveform.py DIR` | records of a hash, offset, size before a count at the end |
| Sakuna: Of Rice and Ruin, `*.arc` of flags 0 | `sakuna.py DIR` | a name of 0x60 bytes, an offset of the data after the table, a size |
| The Tiny Bang Story, `data.pfp` | `tinybang.py FILE` | a count, then a name (u8 length), an offset, a size |

On these (November 2026), the ground truth taken apart was no smaller than no paks at all
(±0.1%): their members are PNG, JPEG, Ogg and WAV files the scan finds itself, or game data
stored, and Sakuna's other 384 `.arc` files are of an unknown codec. The scan found 31.5% and
39% of Waveform's and The Tiny Bang Story's members exactly, none of Baba Is You's or Sakuna's.
