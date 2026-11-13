"""Sakuna's .arc (TGP0) of flags 0, stored: records of a name (0x60 bytes), an offset (of the data
after the table), a size, a flag and 0; the members to the end of the file."""
import glob, os, struct, sys
for p in sorted(glob.glob(os.path.join(sys.argv[1], '*.arc'))):
    d = open(p, 'rb').read(1 << 22)
    _, flags, n, _ = struct.unpack('<HHII', d[4:16])
    if flags != 0:
        continue
    base = 16 + 0x70 * n
    for i in range(n):
        o, s, _, _ = struct.unpack('<IIII', d[16 + 0x70 * i + 0x60:16 + 0x70 * (i + 1)])
        if s:
            print(f'{os.path.basename(p)}\t{base + o}\t{s}')
