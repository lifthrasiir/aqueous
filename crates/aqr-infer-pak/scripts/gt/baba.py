"""Baba Is You's Assets.dat: u32 table length, then (offset, size) u32 pairs; members after the
table, padded to 16. Entries of offset 0 are none."""
import struct, sys
p = sys.argv[1]
d = open(p, 'rb').read(1 << 20)
n = struct.unpack('<I', d[:4])[0] // 8
for i in range(n):
    o, s = struct.unpack('<II', d[8 * i:8 * i + 8])
    if o >= 8 * n and s > 0:
        print(f'Assets.dat\t{o}\t{s}')
