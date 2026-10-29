# aqr-gnu-gettext

GNU gettext message catalogs (`.mo` files) rebuilt bit for bit from the file without its
tables. Where the strings lie one after another, as GNU msgfmt writes them, their lengths and
offsets follow from the strings (and the NULs within plural forms), and the hash table from
the originals: the module `gnu-gettext-recons` derives the three tables, and the rest of the
file (the header and the strings) is kept as it is for the archive to compress. Other writers
of the format (Python's `msgfmt.py`, Babel) write no hash table, which is taken too. Solaris'
own `.mo` format is another one, not taken.

`FORMAT.md` specifies the module.

## Results

On the 1,171 distinct `.mo` files of the 10 GB corpus (79.66 MB; one of them not a catalog),
archived together by `aqr -c` at level mid on an M4, against 406686a, where aqr-infer-recs
transposed their tables:

| | archive | compression | instructions |
|---|---|---|---|
| before | 13,702,395 B | 1.82 s | 183.9 G |
| this crate | 10,983,629 B (−19.8%) | 1.17 s | 76.5 G |

Extraction takes 1.37 G instructions (1.25 G before).
