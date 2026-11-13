# aqr-vorbis

Vorbis I audio packets as the values they code, the values coded by rANS with static tables in
place of their Huffman codes, and the packets written back bit for bit. The module
`vorbis-recons` rebuilds a stream's packets (each after its 16-bit length, as FSB5 frames them)
from the stream's setup header and the record of their values; `FORMAT.md` specifies it.
aqr-fsb gives it the samples of FSB5 banks, aqr-ogg the audio packets of Ogg's Vorbis streams.

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
- `plan`: the module's memory on WebAssembly (it is on the stack allocator, `aqr_api::stack`):
  exactly what the decoder reserves, in the order it does, from the setup header and the
  record's header. Nothing grows or is made per packet: the output first, then the setup (four
  vectors a codebook, its tree last; floors, residues and mappings in fixed room), the keys
  (the entries by rank in one vector), the header's lists and the tables (the default ones from
  the books' codes, the own ones from the levels, all in one vector of frequencies, with no
  hashing), the decoder, the contexts (slots only for the floors' values and the residues'
  partitions the setup has) and the walk's room; freed top first (`Codebooks`, and fields
  declared in the reverse of the order they are made in).

- `setups`: the setup database (`data/vorbis-setups.bin`, FORMAT.md §8), for streams kept
  without their setups: libvorbis's setups as parts (codebooks, floors, residues with their
  `end`s cleared, mappings, modes) and skeletons listing them; it assembles a setup from a
  skeleton and `end`s, and finds the setup of a CRC-32 (FSB5's): by its row, or by a search over
  all skeletons that uses CRC-32's affinity (meet in the middle over the residues' ends).
  Wwise's media (aqr-wwise) name each codebook by an id instead, found against the skeletons.

Written from the Vorbis I specification alone. Floors of type 0 are not handled (libvorbis has
not written them since 1.0).

On The Crew's FSB5 banks (through aqr-fsb), the record is 90.7% of the framed packets of large
streams, 93.2% of small ones; one thread encodes about 23 MB/s and decodes about 21 MB/s.
