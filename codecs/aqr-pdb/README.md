# aqr-pdb

PDB files (the MSF 7.00 container of Microsoft's debugging information, as `link.exe` and
`lld-link` write them) rebuilt bit for bit from streams the archive compresses better
(`FORMAT.md`):

- the container (`pdb-msf`): its streams' data one after another in the order of their first
  blocks (as the file holds them), the layout (the superblock's fields, the streams' sizes, the
  blocks as deltas from the block after the previous) and the rest of the blocks' bytes in
  their order (the free page maps, free blocks of stale data, what follows a stream's data in
  its last block);
- the modules' symbol records (`pdb-symbols`), all modules' in one application: each record's
  fields by its kind's layout to streams of their own; the offsets of a scope's parent and end
  (procedures, blocks, inline sites) derived from how the scopes nest; item ids and type
  indices through caches of the 64 most recent; code offsets as deltas from their field's
  previous; inline sites' binary annotations by their operations, the operands of code
  offsets, code lengths, lines and others apart.

aqr-compress takes PDB files whose modules' symbols are 512 KiB or more: below that, the
modules' blocks (15 KB) cost more than they save. The rest of the streams is scanned as
content (record inference finds the section contributions, hashes and the like).

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

The modules' symbols, through `aqr-pdb-check` (zstd -3):

| | as they are | zstd -3 | the streams, zstd -3 | |
|---|---|---|---|---|
| `uniform.pdb` | 36.74 MB | 12.20 MB | 4.77 MB | −60.9% |
| `std` | 1.60 MB | 0.51 MB | 0.28 MB | −46.3% |
| `combase.pdb` | 12.16 MB | 3.11 MB | 2.40 MB | −22.9% |

Of `uniform.pdb`'s, the annotations are left at 2.99 MB (from 3.84 MB as they were), most of
them code offsets (1.36 MB).

Through `aqr` (level mid), each file alone, before and after:

| | before | after | | xz -9 |
|---|---|---|---|---|
| `uniform.pdb` | 14.95 MB | 7.03 MB | −53.0% | 9.73 MB |
| `std` | 1.116 MB | 0.854 MB | −23.5% | 0.817 MB |
| `combase.pdb` | 12.54 MB | 11.98 MB | −4.4% | 9.63 MB |
| `windows.ui.fileexplorer.pdb` | 842 KB | 828 KB | −1.7% | 649 KB |
| `DwmSceneI.pdb` | 491 KB | 477 KB | −2.7% | 424 KB |

The 9 other PDBs of Windows (public symbols: mostly the global symbols' records, section
contributions and the publics' hash) and two of .NET are kept as before. Compressing
`uniform.pdb` takes 64.2 G instructions (13.2 G before; record inference over the streams but
the symbols takes most), peak RSS 473 MB (331 MB); testing 2.3 G (1.1 G). `combase.pdb`:
38.4 G (24.0 G), testing 1.6 G (1.2 G). The modules' Wasm blocks are 7.4 KB (`pdb-msf`) and
7.8 KB (`pdb-symbols`).

## Status

Left:

- The type records (TPI and IPI: `combase.pdb`'s 38.4 MB of types are 3.5 of its 14.1 MB under
  zstd -3), the global symbols' records (and their publics' and globals' hashes, which follow
  from them), the modules' C13 lines, the DBI stream's section contributions; FPO and OMAP.
- Windows' public PDBs: xz -9 still beats the archive by 13 to 20% on them.
- The records of managed code (.NET's PDBs of the MSF container).
- A record's length where it follows from its fields (inline sites, ends).
