# aqr-ogg

Ogg files rebuilt bit for bit from their logical streams' packets and the layout of their
pages. The module `ogg-recons` derives each page's lacing values from its stream's packets and
its CRC from the page; the layout holds only each page's stream, lacing values' count and the
flags and numbers that differ from a muxer's prediction. A Vorbis stream's audio packets go
to aqr-vorbis's `vorbis-recons`. [FORMAT.md](FORMAT.md) specifies the module and
[its design notes](FORMAT.md#design-notes) say how it works.

```sh
cargo run -r -p aqr-ogg --example aqr-ogg-check -- FILE...   # encoded, decoded; streams by class
```

## What is taken

Ogg files of Vorbis only. Of other streams (Opus, FLAC, Theora) the layout and the packets
framed are no smaller than the pages once compressed, so they are kept as they are. Pages are
taken from the start of a file up to the first that is not whole or of a wrong CRC; what
follows is kept as it is.

## Results

12 Ogg Vorbis files (libvorbisenc with libogg, and ffmpeg's muxer; 49.14 MB): 0.932 through
`aqr` (`zstd -3`: 0.999). The layout is about 5 bytes a page before compression.

## Tests

- `tests/ogg.rs` (`tests/data`): files rebuilt from their formulas: libvorbisenc and libogg,
  ffmpeg's muxer (Vorbis and Opus), FLAC's, chained and grouped streams, data after the pages.
- `tests/edge.rs`: Vorbis streams over the size of a piece and with packets of a chunk and
  more, Opus packets of every TOC code, a decoder given lengths and counts the data cannot hold.

## Left

- **FLAC in Ogg.** Its packets are FLAC's metadata blocks and frames, which aqr-flac would take
  once joined into a FLAC stream.
- **Opus.** No recompressor; the pages are kept as they are.
