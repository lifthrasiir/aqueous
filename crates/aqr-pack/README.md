# aqr-pack

Archives of other formats than Aqueous written from the tree that `aqr -c` gathers, the
counterpart of aqr-unpack: `aqr -c --format zip` (or `-o NAME.zip`) makes a ZIP archive
instead of an Aqueous one. A single input goes in by its name (a directory with what it holds
under it, as 7-Zip and `zip -r` have it).

```rust
let w = aqr_pack::zip::write(&tree, &contents, &options, &mut out, &|progress| ...)?;
// w.read, w.len: bytes of the files and of the archive; w.notes: what ZIP did not keep
```

## ZIP

- Each file is deflated as zlib 1.3.1 does it (aqr-flate's emulator, `zlib::compress`), at
  zlib's level (`-l 0` to `9` there, 6 by default; 0 stores everything), or stored where that
  is no smaller; so an archive made here is rebuilt whole when Aqueous recompresses it. The
  files of a batch (`--batch`, 512 MiB) are deflated on the pool's threads, then written in
  order.
- Names are UTF-8 (flagged where not ASCII); directories are entries; symbolic links are
  stored entries of their targets with Unix's `S_IFLNK`, as Info-ZIP writes them; hard links
  are files each. Times: MS-DOS's (local time), and the extended timestamp (0x5455) and NTFS
  (0x000a, 100 ns) fields with the modification, access and creation times. The Unix mode
  where it is known (made by Unix), Windows' attributes in the low byte. Zip64 where needed.
- Left out, told: FIFOs and devices, extended attributes and forks (on Windows, alternate data
  streams, Dropbox's among them).

On a Ryzen 7 9700X, `third_party` (1,399 files, 40.8 MB): 0.30 s on 16 threads, 0.92 s on
one (Python's zlib at level 6 takes 0.67 s for the files alone), 20.04 MB; recompressed by
`aqr -c` in a directory, 9.88 MB (the files archived themselves: 9.69 MB).

## Status

Left: tar and 7z, ZIP's AES encryption (`-P`), a single large file deflated in parallel
(pigz's way), Info-ZIP's owner field (0x7875).
