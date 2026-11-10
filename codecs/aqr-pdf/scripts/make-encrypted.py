#!/usr/bin/env python3
"""Small encrypted PDF files for tests/data (the Standard security handler, empty user
password), written by ISO 32000's algorithms: RC4 40 and 128 bits (revisions 2 and 3), a V2
crypt filter (revision 4), AESV2 (revision 4, also with the metadata left plain) and AESV3
(revisions 5 and 6). Everything is deterministic (IVs and salts from a counter-mode hash).

    .venv/bin/python codecs/aqr-pdf/scripts/make-encrypted.py codecs/aqr-pdf/tests/data

Needs pycryptodomex (Cryptodome). poppler's pdftotext opens all of them (the check that the
files follow the specification, independently of aqr-pdf).
"""
import hashlib
import struct
import sys
import zlib

from Cryptodome.Cipher import AES, ARC4

PAD = bytes.fromhex("28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A")

_ctr = 0


def rnd(n):
    """Deterministic 'random' bytes."""
    global _ctr
    out = b""
    while len(out) < n:
        _ctr += 1
        out += hashlib.sha256(b"aqr-pdf fixtures %d" % _ctr).digest()
    return out[:n]


def text(seed, n):
    words = ["Hello", "PDF", "world", "recompression", "stream", "object", "page"]
    x = seed
    s = "BT /F1 12 Tf\n"
    for i in range(n):
        x = (x * 1103515245 + 12345) & 0xFFFFFFFF
        s += "%d %d Td (%s) Tj\n" % ((x >> 16) % 600, i % 800, words[(x >> 8) % len(words)])
    return (s + "ET\n").encode()


def pkcs5(d):
    p = 16 - len(d) % 16
    return d + bytes([p]) * p


def standard_key(o, p, id0, r, n, metadata):
    h = hashlib.md5(PAD + o + struct.pack("<I", p & 0xFFFFFFFF) + id0 + (b"\xff" * 4 if r >= 4 and not metadata else b"")).digest()
    if r >= 3:
        for _ in range(50):
            h = hashlib.md5(h[:n]).digest()
    return h[:n]


def u_entry(key, id0, r):
    if r == 2:
        return ARC4.new(key).encrypt(PAD)
    x = ARC4.new(key).encrypt(hashlib.md5(PAD + id0).digest())
    for i in range(1, 20):
        x = ARC4.new(bytes(b ^ i for b in key)).encrypt(x)
    return x + b"\0" * 16


def hash_2b(pw, salt, udata, r):
    k = hashlib.sha256(pw + salt + udata).digest()
    if r == 5:
        return k
    i = 0
    while True:
        k1 = (pw + k + udata) * 64
        e = AES.new(k[:16], AES.MODE_CBC, k[16:32]).encrypt(k1)
        m = int.from_bytes(e[:16], "big") % 3
        k = [hashlib.sha256, hashlib.sha384, hashlib.sha512][m](e).digest()
        i += 1
        if i >= 64 and e[-1] <= i - 32:
            break
    return k[:32]


def ambiguous(salt):
    """Whether the number of rounds of hash_2b for the empty password differs between the two
    readings of "the round number" (rounds done, or counted from 0); the fixtures use salts for
    which it does, so that a reader that gets it wrong fails."""
    k = hashlib.sha256(salt).digest()
    i = 0
    first = {}
    while len(first) < 2:
        e = AES.new(k[:16], AES.MODE_CBC, k[16:32]).encrypt(k * 64)
        k = [hashlib.sha256, hashlib.sha384, hashlib.sha512][int.from_bytes(e[:16], "big") % 3](e).digest()
        i += 1
        if i >= 64 and e[-1] <= i - 32:
            first.setdefault("done", i)
        if i >= 64 and e[-1] <= i - 33:
            first.setdefault("index", i)
        if i > 400:
            return False
    return first["done"] != first["index"]


def salt():
    while True:
        s = rnd(8)
        if ambiguous(s):
            return s


def hexs(b):
    return "<" + b.hex() + ">"


def make(name, variant, metadata=True):
    """variant: rc4-40, rc4-128, v4-rc4, aesv2, aesv3-5 or aesv3-6."""
    id0 = rnd(16)
    p = -3904
    fkey = None
    if variant in ("aesv3-5", "aesv3-6"):
        r = int(variant[-1])
        fkey = rnd(32)
        vs, ks = (salt(), salt()) if r == 6 else (rnd(8), rnd(8))
        u = hash_2b(b"", vs, b"", r) + vs + ks
        ue = AES.new(hash_2b(b"", ks, b"", r), AES.MODE_CBC, b"\0" * 16).encrypt(fkey)
        enc = (
            "<< /Filter /Standard /V 5 /R %d /Length 256 /P %d /EncryptMetadata %s "
            "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF "
            "/O %s /U %s /OE %s /UE %s /Perms %s >>"
            % (r, p, "true" if metadata else "false", hexs(rnd(48)), hexs(u), hexs(rnd(32)), hexs(ue), hexs(rnd(16)))
        )
        method = "aes256"
    else:
        r, v, n = {"rc4-40": (2, 1, 5), "rc4-128": (3, 2, 16), "v4-rc4": (4, 4, 16), "aesv2": (4, 4, 16)}[variant]
        o = rnd(32)
        fkey = standard_key(o, p, id0, r, n, metadata)
        u = u_entry(fkey, id0, r)
        enc = "<< /Filter /Standard /V %d /R %d /Length %d /P %d /O %s /U %s" % (v, r, n * 8, p, hexs(o), hexs(u))
        if v == 4:
            cfm = "AESV2" if variant == "aesv2" else "V2"
            enc += " /CF << /StdCF << /AuthEvent /DocOpen /CFM /%s /Length 16 >> >> /StmF /StdCF /StrF /StdCF" % cfm
            enc += " /EncryptMetadata %s" % ("true" if metadata else "false")
        enc += " >>"
        method = "aes128" if variant == "aesv2" else "rc4"

    def seal(num, data):
        if method == "aes256":
            key = fkey
        else:
            key = hashlib.md5(fkey + struct.pack("<I", num)[:3] + b"\0\0" + (b"sAlT" if method == "aes128" else b"")).digest()[: min(len(fkey) + 5, 16)]
        if method == "rc4":
            return ARC4.new(key).encrypt(data)
        iv = rnd(16)
        return iv + AES.new(key, AES.MODE_CBC, iv).encrypt(pkcs5(data))

    xml = b'<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="">' + b"<dc:title>fixture</dc:title>" * 6 + b"</rdf:Description></rdf:RDF></x:xmpmeta>"
    # (number, dictionary, data, encrypted?)
    objs = [
        (1, "<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>", None, True),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", None, True),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 9 0 R] /Resources << /Font << /F1 5 0 R >> >> >>", None, True),
        (4, "/Filter /FlateDecode", zlib.compress(text(1, 120), 6), True),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>", None, True),
        (6, "", b"plain stream data. " * 40, True),
        (7, "/Type /Metadata /Subtype /XML", xml, metadata),
        (9, "/Filter /FlateDecode", zlib.compress(text(2, 300), 9), True),
        (10, "/Filter /FlateDecode", zlib.compress(text(3, 40), 1) + b"\n", True),
    ]
    f = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"
    offs = {}
    for num, d, data, e in objs:
        offs[num] = len(f)
        f += b"%d 0 obj\n" % num
        if data is None:
            f += d.encode() + b"\n"
        else:
            body = seal(num, data) if e else data
            f += ("<< %s /Length %d >>\nstream\n" % (d, len(body))).encode() + body + b"\nendstream\n"
        f += b"endobj\n"
    offs[8] = len(f)
    f += b"8 0 obj\n" + enc.encode() + b"\nendobj\n"
    x = len(f)
    f += b"xref\n0 11\n0000000000 65535 f \n"
    for num in range(1, 11):
        f += ("%010d 00000 n \n" % offs[num]).encode()
    f += ("trailer\n<< /Size 11 /Root 1 0 R /Encrypt 8 0 R /ID [%s %s] >>\nstartxref\n%d\n%%%%EOF\n" % (hexs(id0), hexs(id0), x)).encode()
    open(name, "wb").write(f)


if __name__ == "__main__":
    d = sys.argv[1]
    make(d + "/enc-rc4-40.pdf", "rc4-40")
    make(d + "/enc-rc4-128.pdf", "rc4-128")
    make(d + "/enc-v4-rc4.pdf", "v4-rc4")
    make(d + "/enc-aesv2.pdf", "aesv2")
    make(d + "/enc-aesv2-plain-metadata.pdf", "aesv2", metadata=False)
    make(d + "/enc-aesv3-r5.pdf", "aesv3-5")
    make(d + "/enc-aesv3-r6.pdf", "aesv3-6")
