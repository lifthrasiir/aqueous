# aqr-vorbis

Vorbis I audio packets as the values they code, the values coded by rANS with static tables in
place of their Huffman codes, and the packets written back bit for bit. The module
`vorbis-recons` rebuilds a stream's packets (each after its 16-bit length, as FSB5 frames them)
from the stream's setup header and the record of their values. [FORMAT.md](FORMAT.md) specifies
it and [its design notes](FORMAT.md#design-notes) say how it works. aqr-fsb gives it the samples
of FSB5 banks, aqr-ogg the audio packets of Ogg's Vorbis streams, aqr-wwise those of Wwise's
media.

Written from the Vorbis I specification alone.

## What is taken

Vorbis I streams: codebooks, floors of type 1, residues of types 0 to 2, any mappings and
modes. Packets cut short, or with padding that is not zero, are kept as odd ones. Streams whose
setup header is not kept (FSB5's CRC-32, Wwise's codebook ids) get it from the setup database
(`data/vorbis-setups.bin`, FORMAT.md §8: libvorbis's setups as parts and skeletons). Not taken:
floors of type 0 (libvorbis has not written them since 1.0).

## Results

On The Crew's FSB5 banks (through aqr-fsb): the record is 90.7% of the framed packets of large
streams, 93.2% of small ones; one thread encodes about 23 MB/s and decodes about 21 MB/s (one thread, Windows).

## Tests

- `tests/packets.rs`: a setup made here (ordered and sparse codebook lengths, lookups of both
  types, floor classes with and without subclasses, residues of the three types, coupling, two
  submaps) and packets of random values, parsed and written back bit for bit, whole, cut short,
  with padding of any bits.
- `tests/codec.rs`: streams of packets through the record and back (cut short, padding, bytes
  past a packet's end), where the stream ends, records that are not one, stripped streams.
- `tests/corrupt.rs`: setup headers that are none are refused within bounded time and memory.
- `tests/edge.rs`: every damage of a setup header is refused, or parses and its packets are
  walked within bounds and written back as they were.

## Left

- **Fixed cost of a stream.** About 0.24 ms in the encoder (its keys, default tables and rANS
  tables), which matters for banks of thousands of small media.
- **Setups of encoders other than libvorbis's.** The database holds libvorbis's (stock 1.3.7);
  aoTuV's are missing (aqr-wwise: Satisfactory's low-rate media, 9 of 82,570, are not found).
