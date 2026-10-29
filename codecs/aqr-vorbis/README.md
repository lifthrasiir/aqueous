# aqr-vorbis

Vorbis I audio packets as the values they code, and back bit for bit. So far the syntax layer:

- `setup` parses a setup header for a stream's channels and block sizes: codebooks (with the
  codewords of Vorbis I §3.2.1 and their lookup tables), floors of type 1, residues of types 0
  to 2, mappings and modes.
- `packet::walk` runs the decode of an audio packet (§4.3.1, floors §7.2.3, residues §8.6.2)
  against a source of values. Reading a packet, it gives its values (fields and codebook
  entries) and its tail: the padding of its last byte, or the bits of a field cut short where
  the packet ends inside it. Given the values, it writes the packet back.

Written from the Vorbis I specification alone. Floors of type 0 are not handled (libvorbis has
not written them since 1.0).

The values are to be modelled and coded in place of the packets' Huffman codes, and the
packets rebuilt by a module; FSB5 banks (aqr-fsb) are the first container.
