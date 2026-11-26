# aqr-egg formula

aqr-egg has no module of its own. The formula of an EGG archive concatenates, in the order of
the archive, the bytes it keeps as they are and the blocks it rebuilds:

- a stored block is its contents;
- a DEFLATE block is an output of one application of aqr-flate's `flate-set` for all of them
  ([its FORMAT.md](../aqr-flate/FORMAT.md), with no derived values), split into the blocks;
- a bzip2 block is an output of one application of aqr-bz2's `bz2-recons` for all of them
  ([its FORMAT.md](../aqr-bz2/FORMAT.md)), split into the blocks;
- the LZMA stream of an LZMA block (after its 9 bytes of header, which are kept) is an output of
  one application of aqr-lzma's `lzma-recons` for all of them ([its FORMAT.md](../aqr-lzma/FORMAT.md),
  the index cutting the data and the records into the streams), split into the blocks;
- an AZO block is an output of one application of aqr-azo's `azo-recons` for all of them
  ([its FORMAT.md](../aqr-azo/FORMAT.md)), split into the blocks.

External reference: OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0 1.0), cited below as *the spec*; this document only says what the formula takes of it, and
where the archives seen differ from it.

## The archive

The archive is read from its start: the archive header (`EGGA`, a version, an id and a reserved
word, 14 bytes, spec §5), the prefix's records (split, solid, skip and global encryption records,
spec §6) up to an end marker, then records one after another, each known by its signature
(spec §3):

- a file header (spec §7), its records up to an end marker (spec §8), then its blocks: each a
  block header (spec §9), an end marker and its compressed data of the size the header tells. A
  block header without its end marker is not a file's: the records end at the file;
- a comment or a dummy record (spec §8.2, §8.6), between the files;
- the final end marker ends the records.

The records of a file and the prefix are records of an extra field (spec §4), any of them read by
its size, as the spec's §16.5 reads unknown ones. A file is encrypted where it has an encryption
record (spec §8.5). Its name is that of its first filename record, which differs from the spec's
§8.1 in the archives seen: bit 3 of its flags (not bit 4) tells of a locale, a code page of 2
bytes (949, 932) between the size and the name, which the size does not count.

Anything else ends the records where it starts: the data of a continuation volume of a split
archive, which goes on from the previous volume after the prefix's end marker (spec §13.2), ends
them there. A record running past the data ends them at the end of the data: an archive cut short,
or a volume whose last block goes on in the next one. The *extent* of the archive, where it is
found in other data, is where its records end.

In a solid archive (spec §14), the archives seen have one block, after the last file, holding
the data of all of them one after another; the files before it have none.

## The blocks rebuilt

A block is rebuilt where its file is not encrypted, it is not empty, and:

- its method is 0 (stored) and its compressed size is its uncompressed size; or
- its method is 1 (DEFLATE, RFC 1951) and aqr-flate splits it into its data and a record shorter
  than the block, with as much data as its uncompressed size; or
- its method is 2 (bzip2, spec §11.3) and `bz2-recons` gives the whole block back from its data,
  as much as its uncompressed size; or
- its method is 4 (LZMA, spec §11.1): its 9 bytes of header are 4 bytes (`04 41 05 00` in every
  archive seen, ZIP's header of LZMA: the LZMA SDK's version 4.65 and the size of the
  properties) and LZMA's properties (the byte of lc, lp and pb, and the dictionary's size), and
  the rest is one LZMA stream of as much data as the uncompressed size (in the archives seen with
  no end marker; one with an end marker is taken too), which `lzma-recons` gives back; or
- its method is 3 (AZO, spec Appendix A): the block is an AZO stream, which aqr-azo splits into
  its data (as its blocks decode, with any x86 filter undone) and record, of as much data as the
  uncompressed size.

The DEFLATE, bzip2, LZMA and AZO blocks decode within a budget
(`Options::limit`, which aqr-compress gives as what is left of its own), by the uncompressed
sizes of their headers, taken in order; blocks past it are kept. The CRC-32 of the headers is not
checked: what is rebuilt is the block, bit for bit, whatever its data.

## The data

Each block's contents (the stored block, or the data of a DEFLATE, bzip2, LZMA or AZO block) are
sliced into a stream by the kind its file's name tells, one for each method: those of at least
1 KiB a slice of their own. The block of a solid archive is cut into the contents of the files it
holds, by their sizes, where those add up to its uncompressed size (else it is all of the kind of
the file it follows). Everything else (the headers, the names, blocks kept, the records after the
files) is one stream of class `egg`, as it is.

## Design notes

- No module of its own (as aqr-alz): the blocks follow their headers, each of the size its
  header tells, so the formula is the bytes kept and the blocks rebuilt in turn.
- ALZip's DEFLATE is zlib's: aqr-flate's adaptive profile (zlib emulation, aqr-compress's levels
  7 to 9) leaves a record of 0.08% of the blocks, the default profile 0.79%. Its bzip2 is
  libbzip2's, a stream a block. Its LZMA is the LZMA SDK's (lc 3, lp 0, pb 2, a dictionary of
  16 MiB, no end marker), whose parse aqr-lzma records in 1.2% of the streams. Its AZO is a
  stream a block, whose parse aqr-azo records in 1.0% of the streams; the data of an AZO block
  with the x86 filter is the file's (aqr-azo's FORMAT.md).
- A solid archive is one block after its last file holding all the files' data, cut into their
  contents by their sizes.
- The files' CRC-32 and sizes (`egg::entry_sums`, the records read one after another, the
  blocks' data skipped; of files of one block, none of a solid archive's) tell aqr-compress's
  probe what the files archived along hold, as for ZIP and 7z.
