# aqr-pdb

PDB files (the MSF 7.00 container of Microsoft's debugging information, as `link.exe` and
`lld-link` write them) rebuilt bit for bit from streams the archive compresses better: the
container (`pdb-msf`), the modules' symbols and C13 lines (`pdb-symbols`) and the type records of
TPI and IPI with their hash streams (`pdb-types`). `FORMAT.md` specifies the modules, and how they
work.

aqr-compress takes PDB files whose modules' symbols are 512 KiB or more: below that, the modules'
blocks (12 KB) cost more than they save. The other streams are scanned as content (record
inference finds the section contributions, hashes and the like).

Written from public descriptions of the MSF container and of CodeView's records (LLVM's
documentation of the PDB format); no code of LLVM, Microsoft's `microsoft-pdb` or other PDB
readers was read.

```sh
cargo run --release -p aqr-pdb --example aqr-pdb-check -- FILE_OR_DIR...   # containers and symbols rebuilt, inputs under zstd
```

## Results

On an M4, 2026-11-02 (`corpus/`, not in git): `uniform.pdb`, a Rust program's release build by
`lld-link` (54.3 MB); Rust's `std` for x86_64-pc-windows-msvc (5.9 MB); and 12 PDBs of Windows
11's DLLs from Microsoft's symbol server (0.3 to 83.4 MB), of which `combase.pdb` holds type
records. Through `aqr` (level mid), each file alone, the TPI and IPI streams coded by field
(`pdb-types`, 2026-11-10):

| | before | with aqr-pdb | xz -9 |
|---|---|---|---|
| `uniform.pdb` | 14.95 MB | 6.38 MB (-57.3%) | 9.73 MB |
| `std` | 1.116 MB | 0.729 MB (-34.7%) | 0.817 MB |
| `combase.pdb` | 12.54 MB | 9.42 MB (-24.9%) | 9.63 MB |
| `windows.ui.fileexplorer.pdb` | 842 KB | 816 KB (-3.1%) | 649 KB |
| `DwmSceneI.pdb` | 491 KB | 459 KB (-6.4%) | 424 KB |

The 9 other PDBs of Windows (public symbols) and two of .NET are kept as before.

| (before -> with, 10 threads) | compressing | testing |
|---|---|---|
| `uniform.pdb` | 101 -> 56 MB/s | 1347 -> 544 MB/s |
| `combase.pdb` | 100 -> 113 MB/s | 1813 -> 1305 MB/s |
| `std` | 45 MB/s | 898 MB/s |

Peak memory compressing `uniform.pdb` 331 -> 473 MB, testing 136 MB. The modules' blocks are
4.5 KB (`pdb-msf`) and 7.0 KB (`pdb-symbols`).

## Tests

```sh
cargo rt -p aqr-pdb
```

- `tests/pdb.rs`: the three modules' round trips and the records kept as they are.

## Left

- **Type records.** Kinds with no layout (`LF_VFTABLE`, labels, 16-bit kinds, the old forms) are
  kept whole; the 8 bytes some compilers write after a function id's name (310 KB in
  `combase.pdb`, a hash) are kept; names (1.1 MB of `combase.pdb`'s 4.5 MB of types) are text as
  they are; type indices (0.9 MB) are only transformed; hash-stream adjusters (none seen) keep a
  stream's hashes.
- **Global symbols and the like.** The global symbols' records (and the publics' and globals'
  hashes, which follow from them), the file checksums of C13 lines, the DBI stream's section
  contributions; FPO and OMAP.
- **Windows' public PDBs.** xz -9 still beats the archive by 13 to 20% on them.
- **Managed code.** The records of .NET's PDBs of the MSF container.
- **Record lengths.** Where they follow from the fields (inline sites, ends).
