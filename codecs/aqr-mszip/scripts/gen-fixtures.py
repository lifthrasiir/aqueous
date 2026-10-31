"""Writes tests/data: CAB files of MSZIP and stored folders, made here (Python's zlib for the
MSZIP blocks, each a stream of its own with the blocks before as its dictionary). Checksums are
left zero, as MS-CAB allows.

    python3 -I codecs/aqr-mszip/scripts/gen-fixtures.py
"""

import os
import random
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests", "data")


def text(n, seed):
    r = random.Random(seed)
    words = [bytes(r.choice(b"abcdefghijklmnopqrstuvwxyz") for _ in range(r.randint(2, 9))) for _ in range(300)]
    out = bytearray()
    while len(out) < n:
        out += r.choice(words) + (b"\n" if r.random() < 0.1 else b" ")
    return bytes(out[:n])


def noise(n, seed):
    return random.Random(seed).randbytes(n)


def mszip(data, level=6, strategy=zlib.Z_DEFAULT_STRATEGY):
    blocks = []
    for i in range(0, max(len(data), 1), 32768):
        chunk = data[i : i + 32768]
        hist = data[max(0, i - 32768) : i]
        c = zlib.compressobj(level, zlib.DEFLATED, -15, 9, strategy, zdict=hist) if hist else zlib.compressobj(
            level, zlib.DEFLATED, -15, 9, strategy
        )
        blocks.append((b"CK" + c.compress(chunk) + c.flush(), len(chunk)))
    return blocks


def stored(data):
    return [(data[i : i + 32768], len(data[i : i + 32768])) for i in range(0, len(data), 32768)]


def cab(folders, res=None, header_res=b""):
    """`folders`: (type, [(name, contents)], make blocks). `res`: (folder, data) reserve sizes."""
    fres, dres = res or (0, 0)
    flags = 4 if res is not None else 0
    files = []
    for k, (_, fs, _) in enumerate(folders):
        off = 0
        for name, contents in fs:
            files.append((name, k, off, len(contents)))
            off += len(contents)
    head = 36 + (4 + len(header_res) if flags & 4 else 0)
    folders_at = head
    files_at = folders_at + len(folders) * (8 + fres)
    files_len = sum(16 + len(n) + 1 for n, _, _, _ in files)
    at = files_at + files_len
    folder_recs, data = b"", b""
    for typ, fs, make in folders:
        blocks = make(b"".join(c for _, c in fs))
        folder_recs += struct.pack("<IHH", at + len(data), len(blocks), typ) + b"\0" * fres
        for payload, n in blocks:
            data += struct.pack("<IHH", 0, len(payload), n) + b"\xaa" * dres + payload
    total = at + len(data)
    hdr = b"MSCF" + struct.pack("<IIIIIBBHHHHH", 0, total, 0, files_at, 0, 3, 1, len(folders), len(files), flags, 0x1234, 0)
    if flags & 4:
        hdr += struct.pack("<HBB", len(header_res), fres, dres) + header_res
    file_recs = b"".join(struct.pack("<IIHHHH", size, off, k, 0x5021, 0x6000, 0x20) + n + b"\0" for n, k, off, size in files)
    out = hdr + folder_recs + file_recs + data
    assert len(out) == total
    return out


def main():
    os.makedirs(OUT, exist_ok=True)
    t1, t2, t3 = text(100_000, 1), text(5_000, 2), text(70_000, 3)
    n1 = noise(40_000, 4)
    with open(os.path.join(OUT, "..", "..", "..", "aqr-jpeg", "tests", "data", "gradient.jpg"), "rb") as f:
        jpeg = f.read()
    fixtures = {
        # one MSZIP folder of several files, blocks referring to the ones before
        "mszip": cab([(1, [(b"a.txt", t1), (b"sub\\b.txt", t2), (b"c.bin", n1)], mszip)]),
        # MSZIP, stored and MSZIP folders; reserved fields in the header, folders and blocks
        "mixed": cab(
            [
                (1, [(b"one.txt", t3)], mszip),
                (0, [(b"two.bin", n1[:30000]), (b"three.txt", t2)], stored),
                (1, [(b"four.txt", t2 + t1[:20000])], lambda d: mszip(d, 9)),
            ],
            res=(3, 4),
            header_res=b"\x01\x02\x03\x04\x05\x06",
        ),
        # fixed codes, and Huffman only
        "fixed": cab(
            [
                (1, [(b"f.txt", t3)], lambda d: mszip(d, 6, zlib.Z_FIXED)),
                (1, [(b"h.txt", t2)], lambda d: mszip(d, 6, zlib.Z_HUFFMAN_ONLY)),
            ]
        ),
        # a small file alone, in one block
        "small": cab([(1, [(b"s.txt", t2[:700])], mszip)]),
        # a JPEG file (aqr-jpeg's fixture) and text, for aqr-compress
        "jpeg": cab([(1, [(b"t.txt", t2), (b"img\\g.jpg", jpeg)], mszip)]),
    }
    for name, d in fixtures.items():
        with open(os.path.join(OUT, name + ".cab"), "wb") as f:
            f.write(d)


main()
