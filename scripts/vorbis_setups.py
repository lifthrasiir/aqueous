#!/usr/bin/env python3
"""Enumerates the Vorbis setup headers a libvorbis build's VBR mode makes.

FMOD's FSB5 stores a Vorbis stream without its setup header, only the header's zlib CRC-32.
Its setups come from vorbis_encode_setup_vbr(channels, rate, quality), with mono setups for more
than two channels, of a libvorbis 1.3 that FMOD changed. The change was inferred by matching
outputs against the 161 setups known from FMOD (every one is then reproduced); in terms of
libvorbis 1.3.7's templates:

- Blocksizes are always 256 and 2048. A template with one blocksize (8 and 11 kHz) becomes 256
  only; every other one, including 32 to 48 kHz below quality 0, becomes 256/2048.
- Floors follow: short-block floors 256x4 and 256x4low become 128x4, 256x7 becomes 128x7;
  long-block floors 2048x27 and 512x17 become 1024x27.
- The 16/22 kHz template's lowest step gets a long-block residue too: the step's class layout
  (mid_un for mono, mid for stereo), with the next step's short-block group book and its stage
  books.
- Everything else, including the lowpass that sets the residues' ends, is unchanged.

Setups of unmodified libvorbis (1.1.0 to 1.3.7 agree) match 107 of the 161; aoTuV and 1.0.1
fewer. Neither the modified tree nor the setups belong in this repository.

This runs a harness built from vorbis_setups_harness.c against a libvorbis over sample rates,
channel counts 1 and 2, and qualities, and keeps each distinct setup with the quality intervals
that give it.

The harness scans quality from -0.1 to 1.0 in `--q-step` steps and reports where the setup
changes. Results are cached per (rate, channels) in OUT/runs.tsv (rate, channels, quality where
an interval starts, CRC-32 or "-" when libvorbis refuses); setups go to OUT/setups/<crc>.bin.
`--table` checks a C table of known setups (vgmstream's vorbis_codebooks_fsb.h layout:
`vcb_<crc>[] = {0x.., ...}`) against what was found, by CRC only.

Usage: python3 scripts/vorbis_setups.py HARNESS OUT [--rates R,LO-HI:STEP,standard]
       [--q-step S] [-j N] [--table FILE]
"""

import argparse
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

# The rates of FSB5's frequency field, then 96 kHz.
STANDARD_RATES = [8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000]


def parse_rates(spec):
    rates = []
    for part in spec.split(','):
        if part == 'standard':
            rates += STANDARD_RATES
        elif '-' in part:
            span, _, step = part.partition(':')
            lo, hi = map(int, span.split('-'))
            rates += range(lo, hi + 1, int(step or 1))
        else:
            rates.append(int(part))
    return sorted(set(rates))


def scan(harness, out, rate, channels, step):
    """The harness's change points for one (rate, channels): [(quality, crc or '-')]."""
    proc = subprocess.run([str(harness), str(out / 'setups'), str(channels), str(rate), '-0.1', '1.0', str(step)],
                          capture_output=True, text=True, check=True)
    points = []
    for line in proc.stdout.splitlines():
        r, c, q, crc = line.split('\t')
        assert (int(r), int(c)) == (rate, channels)
        crc = '-' if crc.startswith('-') else crc
        if not points or points[-1][1] != crc:
            points.append((float(q), crc))
    return points


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('harness', type=Path)
    ap.add_argument('out', type=Path)
    ap.add_argument('--rates', default='standard', help='"standard", R or LO-HI[:STEP], comma-separated')
    ap.add_argument('--q-step', type=float, default=0.001)
    ap.add_argument('-j', type=int, default=16)
    ap.add_argument('--table', type=Path)
    args = ap.parse_args()

    (args.out / 'setups').mkdir(parents=True, exist_ok=True)
    runs_path = args.out / 'runs.tsv'
    done = set()
    if runs_path.exists():
        for line in runs_path.read_text().splitlines():
            r, c, _, _ = line.split('\t')
            done.add((int(r), int(c)))

    todo = [(r, c) for r in parse_rates(args.rates) for c in (1, 2) if (r, c) not in done]
    print(f'{len(todo)} (rate, channels) to scan, {len(done)} cached')
    with open(runs_path, 'a') as log, ThreadPoolExecutor(args.j) as pool:
        for (r, c), points in zip(todo, pool.map(lambda k: scan(args.harness, args.out, *k, args.q_step), todo)):
            for q, crc in points:
                log.write(f'{r}\t{c}\t{q:.6f}\t{crc}\n')
            log.flush()

    found = {p.stem for p in (args.out / 'setups').glob('*.bin')}
    print(f'{len(found)} distinct setups')
    if args.table:
        known = set(re.findall(r'vcb_([0-9a-f]{8})\[\]', args.table.read_text()))
        print(f'table: {len(known & found)} of {len(known)} found; missing: {" ".join(sorted(known - found))}')


if __name__ == '__main__':
    main()
