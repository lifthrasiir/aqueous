# aqr-pdb

PDB files (the MSF 7.00 container of Microsoft's debugging information, as `link.exe` and
`lld-link` write them) rebuilt bit for bit from streams the archive compresses better
(`FORMAT.md`):

- the container (`pdb-msf`): its streams' data one after another in the order of their first
  blocks (as the file holds them), the layout (the superblock's fields, the streams' sizes, the
  blocks as deltas from the block after the previous) and the rest of the blocks' bytes in
  their order (the free page maps, free blocks of stale data, what follows a stream's data in
  its last block);
- the modules' streams (`pdb-symbols`), all modules' in one application: each symbol record's
  fields by its kind's layout to streams of their own; the offsets of a scope's parent and end
  (procedures, blocks, inline sites) derived from how the scopes nest; item ids and type
  indices through caches of the 64 most recent; code offsets as deltas from their field's
  previous; inline sites' binary annotations by their operations, the operands of code
  offsets, code lengths, lines and others apart. Of the C13 lines, the lines' offsets and
  numbers as deltas, and of inlinee lines each inlinee's file and line from its last entry
  (the same in 99.9% of uniform.pdb's 384 K entries); the rest (checksums, global references)
  as it is, scanned as content.

aqr-compress takes PDB files whose modules' symbols are 512 KiB or more: below that, the
modules' blocks (12 KB) cost more than they save. The other streams are scanned as content
(record inference finds the section contributions, hashes and the like): the modules' streams
being whole outputs, they are a few runs, each inferred once (when the modules' symbols alone
were outputs, uniform.pdb's were 57 pieces, 1.28 s of inference against 0.29 s at once).

Written from public descriptions of the MSF container and of CodeView's records (LLVM's
documentation of the PDB format); no code of LLVM, Microsoft's `microsoft-pdb` or other PDB
readers was read.

```sh
cargo run --release -p aqr-pdb --example aqr-pdb-check -- FILE_OR_DIR...   # containers and symbols rebuilt, inputs under zstd
```

## How it does

Measured on 2026-11-02 on an M4 (`corpus/`, not in git): `uniform.pdb`, a Rust program's
release build by `lld-link` (54.3 MB, 36.7 MB of it modules' symbols, 75.8% of those inline
sites); Rust's `std` for x86_64-pc-windows-msvc (5.9 MB); and 12 PDBs of Windows 11's DLLs
from Microsoft's symbol server (0.3 to 83.4 MB), of which `combase.pdb` holds type records.

The modules' streams, through `aqr-pdb-check` (zstd -3):

| | as they are | zstd -3 | the streams, zstd -3 | |
|---|---|---|---|---|
| `uniform.pdb` | 44.17 MB | 13.95 MB | 5.78 MB | −58.6% |
| `std` | 3.36 MB | 0.70 MB | 0.41 MB | −41.2% |
| `combase.pdb` | 17.85 MB | 4.62 MB | 3.31 MB | −28.5% |

Of `uniform.pdb`'s, the annotations are left at 2.99 MB (from 3.84 MB as they were), most of
them code offsets (1.36 MB); the C13 lines at 1.07 MB (from 1.58 MB).

Through `aqr` (level mid), each file alone, before and after:

| | before | after | | xz -9 |
|---|---|---|---|---|
| `uniform.pdb` | 14.95 MB | 6.57 MB | −56.0% | 9.73 MB |
| `std` | 1.116 MB | 0.850 MB | −23.8% | 0.817 MB |
| `combase.pdb` | 12.54 MB | 11.71 MB | −6.6% | 9.63 MB |
| `windows.ui.fileexplorer.pdb` | 842 KB | 816 KB | −3.1% | 649 KB |
| `DwmSceneI.pdb` | 491 KB | 459 KB | −6.4% | 424 KB |

The 9 other PDBs of Windows (public symbols: mostly the global symbols' records, section
contributions and the publics' hash) and two of .NET are kept as before.

| | compressing, G instructions | MB/s (10 threads) | testing, G instructions | MB/s |
|---|---|---|---|---|
| `uniform.pdb` | 13.2 -> 28.2 | 101 -> 56 | 1.1 -> 2.4 | 1347 -> 544 |
| `combase.pdb` | 24.0 -> 19.4 | 100 -> 113 | 1.2 -> 1.6 | 1813 -> 1305 |
| `std` | 2.1 -> 3.7 | 45 | 0.67 -> 0.79 | 898 |

Peak RSS compressing `uniform.pdb` 331 -> 453 MB, testing 136 MB. The modules' Wasm blocks are
4.5 KB (`pdb-msf`) and 7.0 KB (`pdb-symbols`).

## Status

Left:

- The type records (TPI and IPI: `combase.pdb`'s 38.4 MB of types are 3.5 of its 14.1 MB under
  zstd -3), the global symbols' records (and their publics' and globals' hashes, which follow
  from them), the file checksums of C13 lines, the DBI stream's section contributions; FPO
  and OMAP.
- Windows' public PDBs: xz -9 still beats the archive by 13 to 20% on them.
- The records of managed code (.NET's PDBs of the MSF container).
- A record's length where it follows from its fields (inline sites, ends).
