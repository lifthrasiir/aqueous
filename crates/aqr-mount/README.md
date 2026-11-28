# aqr-mount

What the mounting programs share: a read-only `Tree` of nodes, which each program serves
through its system's interface. Nodes are named by a `NodeId` (the root is 1, FUSE's root
inode) and looked up one name at a time in a directory, in any case (Windows) or not; a tree
gives attributes, a directory's entries and reads at an offset. `lookup_path` walks a path
whole, as WinFsp gives them.

| System | Program | Released |
|---|---|---|
| Windows | `aqr-mount-win` (`aqr-mount.exe`, WinFsp) | on its own, beside the installer |
| macOS | to be in `aqr-mac` | in the app |
| Linux | `aqr-mount-fuse` (FUSE through fusermount3) | to be decided |

## Left

- A tree of an archive: aqr's listing for the nodes, its extraction for reads at an offset
  (solid streams need points to resume from, or a cache).
- What a name colliding in case does on Windows (the first in the tree's order is taken now).
- Symbolic links and the other metadata archives carry (modes, owners, times other than the
  modification's).
