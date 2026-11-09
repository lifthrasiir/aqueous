# The formula of an image

aqr-iso has no formula of its own: it reads the files of ISO 9660 and UDF images for aqr-disk,
whose formula ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)) takes them apart into the
contents of their files and the rest (the system area, descriptors, path tables, directories,
file entries, the padding of sectors, what no file points at).

The ranks of its files, by which aqr-disk names an extent several trees give, are UDF's (0),
Joliet's (1), then ISO 9660's own (2).
