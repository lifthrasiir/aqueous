#!/usr/bin/env python3
"""Writes an archive shaped like 42.zip, without its password: 16 "lib" archives, each of 16
"book" archives, then chapters, docs and pages, each page holding "0.dll", 4294967295 zero
bytes. The levels are deflated as PKZIP made them (maximum), and every archive of a level is
the same one.

usage: python3 aqr-zip/scripts/make-42.py OUT.zip   (seconds, for the zeros)
"""
import io, struct, sys, time, zipfile, zlib

DATE = (2000, 3, 28, 21, 40, 54)


def store(entries):
    """An archive of (name, data) entries, deflated at level 9."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for name, data in entries:
            info = zipfile.ZipInfo(name, DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)
    return buf.getvalue()


def page():
    """The page archive, by hand: Python's zipfile would write zip64 fields for 0.dll, whose size,
    0xFFFFFFFF, is the value that otherwise stands for them, but 42.zip has none."""
    n, chunk = 0xFFFFFFFF, bytes(1 << 24)
    c, crc, body = zlib.compressobj(9, zlib.DEFLATED, -15), 0, []
    while n:
        k = min(n, len(chunk))
        body.append(c.compress(chunk[:k]))
        crc = zlib.crc32(chunk[:k], crc)
        n -= k
    body.append(c.flush())
    body = b"".join(body)
    name = b"0.dll"
    dos = ((DATE[3] << 11) | (DATE[4] << 5) | DATE[5] // 2, ((DATE[0] - 1980) << 9) | (DATE[1] << 5) | DATE[2])
    common = struct.pack("<HHHHHIIIHH", 20, 2, 8, *dos, crc, len(body), 0xFFFFFFFF, len(name), 0)
    local = b"PK\x03\x04" + common + name
    central = b"PK\x01\x02" + struct.pack("<H", 20) + common + struct.pack("<HHHII", 0, 0, 0, 0, 0) + name
    end = b"PK\x05\x06" + struct.pack("<HHHHIIH", 0, 0, 1, 1, len(central), len(local) + len(body), 0)
    return local + body + central + end


t = time.time()
level = page()
for name in ["page", "doc", "chapter", "book", "lib"]:
    level = store([(f"{name} {i:x}.zip", level) for i in range(16)])
open(sys.argv[1], "wb").write(level)
print(f"{len(level)} bytes in {time.time() - t:.0f} s")
