# The formula of an image

aqr-iso has no formula of its own: it reads the files of ISO 9660 and UDF images for aqr-disk,
whose formula ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)) takes them apart into the
contents of their files and the rest (the system area, descriptors, path tables, directories,
file entries, the padding of sectors, what no file points at).

The ranks of its files, by which aqr-disk names an extent several trees give, are UDF's (0),
Joliet's (1), then ISO 9660's own (2).

## Reading

- **UDF** (ECMA-167 as OSTA UDF profiles it): the anchor at sector 256 (or the last sector), the
  main volume descriptor sequence (partitions, the logical volume of 2048-byte blocks), the file
  set descriptor, then the directories: file entries and extended file entries, their data in
  short or long allocation descriptors (chained by allocation extent descriptors) or inline.
- **ISO 9660**: the trees of the primary volume descriptor and of the supplementary ones (Joliet's
  UCS-2 names), files of several extents as one; 8.3 names with versions dropped.

The names give the contents their kinds (aqr-compress's `kind_by_name`). aqr-disk leaves an extent
overlapping one before it in the rest, and so everything the trees do not point at. Each directory
is read once, so loops do not hang it, and what is past the end of an image cut short is not read.
aqr-compress takes an image as an archive that only bundles files (as aqr-bundle's): its formula is
not compared against the image raw, and its files are made in parallel, each a stream of its own
sharing the image's bytes where it is mapped.

