"""Waveform's .fc: the members from 0 on, then records of a hash, an offset and a size (u32),
then their count (u32) at the end."""
import glob, os, struct, sys
for p in sorted(glob.glob(os.path.join(sys.argv[1], '*.fc'))):
    d = open(p, 'rb').read()
    n = struct.unpack('<I', d[-4:])[0]
    t = len(d) - 4 - 12 * n
    for i in range(n):
        _, o, s = struct.unpack('<III', d[t + 12 * i:t + 12 * i + 12])
        if s:
            print(f'{os.path.basename(p)}\t{o}\t{s}')
