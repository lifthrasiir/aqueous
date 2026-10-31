# aqr-ima

IMA ADPCM blocks as a record of their headers and nibbles, and back bit for bit: Xbox ADPCM (of
FSB5 banks, through aqr-fsb) and Microsoft's IMA ADPCM (of WAV files, through aqr-raw) alike, by their channels and
bytes per channel. The module `ima-recons` rebuilds whole blocks; `FORMAT.md` specifies it.

The record is a binary arithmetic code of adaptive bits (shifts growing with what each has seen)
in contexts of the IMA decoder's state:
- a channel equal to the one before in a block (FMOD's stereo is often two equal channels) is
  one decision;
- a header against the state its channel ended the block before at: the index as that one's
  plus one (FMOD's), the predictor as a difference of a few steps, the reserved byte as 0;
- each nibble by a tree of 4 bits, in a context of the step index, the nibble before, and the
  other channel's nibble at the same place.

Any bytes of whole blocks come back. On The Crew's FSB5 banks the record is 57.8% of the blocks
(xz -9e: about 67%); one thread encodes about 33 MB/s and decodes about 25 MB/s. IMA ADPCM WAV files of ffmpeg and
sox come to 0.58–0.61 in stereo, 0.78 in mono (aqr-raw's README).
