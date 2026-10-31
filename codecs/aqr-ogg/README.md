# aqr-ogg

Ogg files rebuilt bit for bit from their logical streams' packets and the layout of their
pages. The module `ogg-recons` derives each page's lacing values from its stream's packets and
its CRC from the page, and the layout holds only each page's stream and lacing values' count,
and its header type, sequence number and granule position as they differ from a prediction:
the flags and numbers as a muxer writes them, the granule positions from the samples of the
packets ending on the page (of Vorbis' block sizes, of Opus' TOC bytes). `FORMAT.md` specifies
it.

- `page`: a page, read with its CRC checked, and written.
- `layout`: the packets framed (in chunks of 65535 bytes at most, as aqr-vorbis frames its
  packets), the layout record, and the walk over the pages that the encoder and the decoder
  both make.
- `formula`: the file as its layout and its streams' packets, a Vorbis stream's audio packets
  by aqr-vorbis's `vorbis-recons` in pieces of 2 MiB (on threads), its setup header one stream
  that the pieces and the packets share.

```sh
cargo run -r -p aqr-ogg --example aqr-ogg-check -- FILE...   # encoded, decoded; streams by class
```

Ogg files of Vorbis only are taken: of other streams (Opus, FLAC, Theora), the layout and the
packets framed are no smaller than the pages once compressed. Pages are taken from the start of
a file up to the first that is not whole or of a wrong CRC; what follows is kept as it is.

On 12 Ogg Vorbis files (libvorbisenc with libogg, and ffmpeg's muxer; 49.14 MB): 0.932 through
`aqr` (zstd -3 0.999); the layout is about 5 bytes a page before compression.

## Status

Left:

- FLAC in Ogg: its packets are FLAC's metadata blocks and frames, which aqr-flac would take
  once joined into a FLAC stream.
- Opus: no recompressor; the pages are kept as they are.
