# aqr-vorbis

Vorbis I audio packets as the values they code, the values coded by rANS with static tables in
place of their Huffman codes, and the packets written back bit for bit. The module
`vorbis-recons` rebuilds a stream's packets (each after its 16-bit length, as FSB5 frames them)
from the stream's setup header and the record of their values; `FORMAT.md` specifies it.

- `setup` parses a setup header for a stream's channels and block sizes: codebooks (with the
  codewords of Vorbis I §3.2.1, their decoding trees, a table for their first 10 bits, and their
  lookup tables), floors of type 1, residues of types 0 to 2, mappings and modes.
- `packet::walk` runs the decode of an audio packet (§4.3.1, floors §7.2.3, residues §8.6.2)
  against a source of values: reading a packet gives its values and tail (`packet::parse`),
  values given write it (`packet::write`), and the record's coder takes or gives them (`codec`).
- `model`: each value is a symbol of a key, chosen by its role, its book and a context: for a
  residue VQ entry, the magnitude of the entry before it; for floor values and classwords, the
  same field's value in the previous packet of its block size. Entries are coded by their rank
  in the code (escapes past 255, as tables hold 256 symbols). Each key's table is the book's
  code (free) or one of its own the record gives, where it pays.
- `codec`: the record: the packets, the odd ones (cut short, or of padding not zero), the keys'
  tables, then one rANS stream (aqr-common's `rans64`).

Written from the Vorbis I specification alone. Floors of type 0 are not handled (libvorbis has
not written them since 1.0).

On The Crew's FSB5 banks (through aqr-fsb), the record is 90.7% of the framed packets of large
streams, 93.2% of small ones; one thread encodes about 23 MB/s and decodes about 21 MB/s.
