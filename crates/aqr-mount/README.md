# aqr-mount

What the mounting programs share: a read-only `Tree` of nodes, which each program serves
through its system's interface. Nodes are named by a `NodeId` (the root is 1, FUSE's root
inode) and looked up one name at a time in a directory, in any case (Windows) or not; a tree
gives attributes, a directory's entries and reads at an offset. `lookup_path` walks a path
whole, as WinFsp gives them.

`ServedTree` is the tree of an archive, as `aqr --serve` serves it (aqr-ipc's client): what
aqr reads (Aqueous archives, the other formats of a tree, a compressed single file) is mounted
as aqr-win's shell extension browses it, and what decodes runs in aqr's process, not the
mounting program's. Directories are listed as they are first looked in, their entries given
node IDs then; reads go in pieces of the protocol's chunk (1 MiB), the first of a file
evaluating its leaf, which `aqr --serve` keeps for the reads after. `OneFile`, one fixed file,
is for the tests. The tests of `ServedTree` run the `aqr` built beside them (`cargo rt -p aqr
-p aqr-mount`).

| System | Program | Released |
|---|---|---|
| Windows | `aqr-mount-win` (`aqr-mount.exe`, WinFsp) | on its own, beside the installer |
| macOS | to be in `aqr-mac` (deferred: macFUSE/FUSE-T, a local NFS server or FSKit) | in the app |
| Linux | `aqr-mount-fuse` (FUSE through fusermount3) | as `aqr-mount`, in the release archive |

## Left

- Encrypted archives (a passphrase asked for, given to `open`).
- `aqr --serve` gone (killed, crashed): every request fails now; opened again, as aqr-win does.
- Links (left out now) and the entries of other kinds.
- Reads of a big leaf: the first waits for all of the leaf to be evaluated (`aqr --serve`'s
  cache, spilled to a file over its bound); WinFsp's IRP times out after its default.
- What a name colliding in case does on Windows (the first in the tree's order is taken now).
- Symbolic links and the other metadata archives carry (modes, owners, times other than the
  modification's).
