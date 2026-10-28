#!/usr/bin/env python3
"""Two files of known layouts for rec_probe.py.

- struct.bin: a 316-byte header, 50000 records of 20 bytes (u32 id counting up, u64 time in
  ms rising by 900–1100, f32 random walk, u16 kind of four values, u16 zero) and a footer of
  68 bytes. Oracle: --layout 0x13c:20:50000.
- pcap.bin: a pcap file of 20000 packets, 60–1514 bytes each, whose payloads are taken in
  turn from SOURCE (a file of real bytes).

Usage (from the root): .venv/bin/python scripts/rec_synth.py SOURCE OUTDIR
"""

import struct
import sys
from pathlib import Path

import numpy as np


def main():
    src, out = Path(sys.argv[1]).read_bytes(), Path(sys.argv[2])
    rng = np.random.default_rng(1)

    n = 50000
    hdr = b'SYNT' + struct.pack('<III', 1, n, 20) + bytes(rng.integers(0, 256, 300, dtype=np.uint8))
    rec = np.zeros(n, dtype=[('id', '<u4'), ('ts', '<u8'), ('v', '<f4'), ('kind', '<u2'), ('pad', '<u2')])
    rec['id'] = np.arange(n) + 1000
    rec['ts'] = 1700000000000 + np.cumsum(rng.integers(900, 1100, n))
    rec['v'] = np.cumsum(rng.normal(0, 1, n)).astype(np.float32)
    rec['kind'] = rng.choice([1, 2, 7, 300], n, p=[.5, .3, .15, .05])
    (out / 'struct.bin').write_bytes(hdr + rec.tobytes() + b'END!' + bytes(64))

    pcap = bytearray(struct.pack('<IHHiIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1))
    t, at = 1700000000.0, 0
    for _ in range(20000):
        ln = int(rng.choice([60, 64, 1514, int(rng.integers(60, 1514))]))
        t += rng.exponential(0.002)
        pcap += struct.pack('<IIII', int(t), int((t % 1) * 1e6), ln, ln) + src[at:at + ln]
        at = (at + ln) % (len(src) - 2000)
    (out / 'pcap.bin').write_bytes(pcap)


if __name__ == '__main__':
    main()
