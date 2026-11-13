"""The Tiny Bang Story's data.pfp (PFPK): u32 count, then records of a name (u8 length), an offset
and a size (u32)."""
import struct, sys
p = sys.argv[1]
d = open(p, 'rb').read(1 << 22)
n = struct.unpack('<I', d[4:8])[0]
i = 8
for _ in range(n):
    L = d[i]
    o, s = struct.unpack('<II', d[i + 1 + L:i + 9 + L])
    i += 9 + L
    if s:
        print(f'data.pfp\t{o}\t{s}')
