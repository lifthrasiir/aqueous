#!/usr/bin/env python3
"""Writes tests/data/*.7z: deterministic files archived by 7-Zip (7zz) in LZMA2 and LZMA,
solid and not, with the header encoded and plain, stored, in LZMA2 blocks of 64 KiB (dictionary
resets). 7-Zip puts the PE file (made up) in a folder of BCJ and LZMA or LZMA2, left as it is.

    python3 -I scripts/gen-fixtures.py   # needs 7zz on the PATH
"""
import os, random, struct, subprocess, tempfile

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'tests', 'data')

def files():
    r = random.Random(1)
    words = [''.join(r.choice('etaoinshrdlu') for _ in range(r.randint(1, 9))) for _ in range(300)]
    text = ' '.join(r.choice(words) for _ in range(6000)).encode()[:32768]
    funcs = [bytes(r.randrange(256) for _ in range(r.randint(8, 120))) for _ in range(200)]
    binary = bytearray()
    while len(binary) < 32768:
        binary += r.choice(funcs)
    letters = bytes(r.choice(b'etaoinshrdlucmfwyp') for _ in range(50000))
    noise = bytes(r.randrange(256) for _ in range(20000))
    return {
        'bin/prog.exe': pe(r),
        'docs/words.txt': text,
        'docs/copy of words.txt': text,
        'bin/code.bin': bytes(binary[:32768]),
        'bin/letters.dat': letters,
        'bin/noise.dat': noise,
        'empty.txt': b'',
        'tiny.txt': b'x',
    }

def pe(r):
    """A PE file of x86 code (calls among random bytes), which 7-Zip filters by BCJ."""
    dos = bytearray(64)
    dos[0:2] = b'MZ'
    struct.pack_into('<I', dos, 60, 64)
    coff = struct.pack('<4sHHIIIHH', b'PE\0\0', 0x14c, 1, 0, 0, 0, 224, 0x102)
    opt = bytearray(224)
    struct.pack_into('<H', opt, 0, 0x10b)
    sec = struct.pack('<8sIIIIIIHHI', b'.text', 0x2000, 0x1000, 0x2000, 0x200, 0, 0, 0, 0, 0x60000020)
    code = bytearray()
    while len(code) < 0x2000:
        code += b'\xe8' + struct.pack('<i', r.randrange(-5000, 5000)) + bytes(r.randrange(256) for _ in range(r.randint(1, 12)))
    head = bytes(dos + coff + opt + sec)
    return head + bytes(0x200 - len(head)) + bytes(code[:0x2000])

VARIANTS = {
    'lzma2': ['-m0=LZMA2'],
    'lzma2-plain': ['-m0=LZMA2', '-mhc=off'],
    'lzma': ['-m0=LZMA'],
    'lzma-files': ['-m0=LZMA', '-ms=off'],
    'copy': ['-mx0'],
    'blocks': ['-m0=LZMA2:d=256k:c=64k', '-mhc=off'],
}

def main():
    os.makedirs(OUT, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, 'in')
        for name, data in files().items():
            p = os.path.join(src, name)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, 'wb') as f:
                f.write(data)
        os.makedirs(os.path.join(src, 'empty dir'))
        for root, dirs, fs in os.walk(src):
            for n in dirs + fs:
                os.utime(os.path.join(root, n), (1_600_000_000, 1_600_000_000))
        for v, args in VARIANTS.items():
            out = os.path.join(OUT, f'{v}.7z')
            if os.path.exists(out):
                os.remove(out)
            subprocess.run(['7zz', 'a', '-bd', '-mtc=off', '-mta=off', '-mmt=1'] + args + [out, '.'],
                           cwd=src, check=True, capture_output=True)

if __name__ == '__main__':
    main()
