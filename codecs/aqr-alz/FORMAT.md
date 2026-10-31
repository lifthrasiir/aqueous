# aqr-alz formula

aqr-alz has no module of its own. The formula of an ALZ archive concatenates, in the order of
the archive, the bytes it keeps as they are and the bodies of the entries it rebuilds:

- a stored body is its contents;
- a DEFLATE body is an output of one application of aqr-flate's `flate-set` for all of them
  ([its FORMAT.md](../aqr-flate/FORMAT.md), with no derived values), split into the bodies;
- a bzip2 body is an output of one application of aqr-bz2's `bz2-alz-recons` for all of them
  ([its FORMAT.md](../aqr-bz2/FORMAT.md#8-the-bz2-alz-recons-module)), split into the bodies.

External reference: OctopusET's
[ALZ Archive Format Spec](https://gist.github.com/OctopusET/6a88b6bb3f4eed682c031e704012cfc0)
(CC0 1.0), cited below as *the spec*; this document only says what the formula takes of it.

## The archive

The archive is read from its start: the archive header (`ALZ\x01`, a version and an id, 8 bytes,
spec §3), then records one after another, each known by its signature (spec §2):

- a local file header (`BLZ\x01`, spec §4) and its body: the entry's name, then the body, which
  is the encryption header (12 bytes, where the descriptor's bit 0 is set) and the compressed
  data of the compressed size. A descriptor whose size width (high nibble) is none of 0, 1, 2, 4,
  8 is not a local file header;
- a comment section (`ELZ\x01`, spec §8): its entries are read one after another up to a central
  directory's signature;
- the central directory structure (`CLZ\x01`, spec §10), 16 bytes;
- the end of central directory (`CLZ\x02`, spec §11) or a split marker (`CLZ\x03`), 4 bytes, end
  the records.

Anything else ends them where it starts. A record running past the data ends them at the end of
the data: an archive cut short, or a volume of a split archive whose last entry goes on in the
next volume (spec §12). The *extent* of the archive, where it is found in other data, is where
its records end. The tail the spec reads first (spec §9) is, in every archive seen, the central
directory's 12 bytes after its signature and the end's signature; the formula does not read it.

## The bodies rebuilt

An entry's body is rebuilt where it is not encrypted, not empty, and:

- its method is 0 (stored) and its compressed size is its uncompressed size; or
- its method is 2 (DEFLATE, RFC 1951) and aqr-flate splits it into its data and a record shorter
  than the body, with no more data than the uncompressed size; or
- its method is 1 (ALZip's bzip2, spec §5.1) and `bz2-alz-recons` gives the whole body back
  from its data, with no more data than the uncompressed size.

The DEFLATE and bzip2 bodies decode within a budget (`Options::limit`, which aqr-compress gives
as what is left of its own), by the uncompressed sizes of their headers, taken in order; bodies
past it are kept. The CRC-32 of the headers is not checked: what is rebuilt is the body, bit for
bit, whatever its data.

## The data

Each body's contents (the stored body, or the data of a DEFLATE or bzip2 body) are sliced into
a stream by the kind its entry's name tells, one for each method: those of at least 1 KiB a slice
of their own. Everything else (the headers, the names, bodies kept, the records after the
entries) is one stream of class `alz`, as it is.
