# aqr-vorbis

Vorbis I audio packets as the values they code, coded under a context-mixing model in place of
their Huffman codes, and the packets written back bit for bit. The module `vorbis-recons`
rebuilds a stream's packets (each after its 16-bit length, as FSB5 frames them) from the stream's
setup header and the record of their values; `FORMAT.md` specifies it.

- `setup` parses a setup header for a stream's channels and block sizes: codebooks (with the
  codewords of Vorbis I §3.2.1, their decoding trees and lookup tables), floors of type 1,
  residues of types 0 to 2, mappings and modes.
- `packet::walk` runs the decode of an audio packet (§4.3.1, floors §7.2.3, residues §8.6.2)
  against a source of values: reading a packet gives its values and tail (`packet::parse`),
  values given write it (`packet::write`), and the model coding or decoding them makes the
  record (`codec`).
- `model`: a residue VQ entry of a lookup-1 book is coded digit by digit, each digit as
  decisions whose prior is the book's own code factorised over the digits, mixed with counters
  and APMs in contexts of the coefficients around (before it in frequency, in the previous
  packet, in the previous pass, in the other channel). Other entries are coded down their
  books' trees, fields as binary trees of their values. `cm` holds the integer primitives.

Written from the Vorbis I specification alone. Floors of type 0 are not handled (libvorbis has
not written them since 1.0).

On The Crew's FSB5 banks (through aqr-fsb), the record is 89.9-91.5% of the framed packets;
against the Huffman codes, VQ entries 95.7%, floor values 63-90%, classwords 84.7%. The model
codes about 2 MB/s on a thread.

```sh
cargo run -r -p aqr-fsb --example cmcost -- FILE.fsb...   # the model's cost by field
```
