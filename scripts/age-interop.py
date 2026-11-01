"""Checks aqr's encrypted archives against age (pyrage, the rage implementation): an archive
sealed by aqr opened by age, and a payload sealed by age opened as aqr's envelope.

usage: .venv/bin/python scripts/age-interop.py SEALED PLAIN PASSPHRASE
       .venv/bin/python scripts/age-interop.py --seal PLAIN PASSPHRASE OUT

The first form takes an encrypted archive and its payload decrypted (as aqr wrote them) and
checks that age decrypts the archive, rewritten as an age file, to the payload. The second
seals PLAIN with age and writes it as an encrypted archive (not mirrored) for aqr to open.
"""

import base64
import sys

import pyrage

MAGIC = bytes.fromhex("f9caf2a6d02a0100")
MIRRORED = bytes.fromhex("f9caf2a6d02a0200")
FIELDS = 97
MIRROR = 117
SEALED = 65536 + 16


def b64(d):
    return base64.b64encode(d).decode().rstrip("=")


def unb64(s):
    return base64.b64decode(s + "=" * (-len(s) % 4))


def mirror_before(m):
    n = m + 1
    return n in (2, 4, 8, 16, 32) or n % 64 == 0


def to_age(c):
    mirrored = c.startswith(MIRRORED)
    assert mirrored or c.startswith(MAGIC)
    f = c[8 : 8 + FIELDS]
    wf, salt, body, mac, nonce = f[0], f[1:17], f[17:49], f[49:81], f[81:97]
    at = 8 + FIELDS + (8 if mirrored else 0)
    chunks = []
    while at < len(c):
        if mirrored and mirror_before(len(chunks)):
            at += MIRROR
        chunks.append(c[at : at + SEALED])
        at += SEALED
    head = f"age-encryption.org/v1\n-> scrypt {b64(salt)} {wf}\n{b64(body)}\n--- {b64(mac)}\n"
    return head.encode() + nonce + b"".join(chunks)


def from_age(a):
    head, rest = a.split(b"\n--- ", 1)
    mac, payload = rest.split(b"\n", 1)
    lines = head.decode().split("\n")
    assert lines[0] == "age-encryption.org/v1" and len(lines) == 3, lines
    _, kind, salt, wf = lines[1].split(" ")
    assert kind == "scrypt"
    fields = bytes([int(wf)]) + unb64(salt) + unb64(lines[2]) + unb64(mac.decode()) + payload[:16]
    assert len(fields) == FIELDS
    return MAGIC + fields + payload[16:]


def main():
    if sys.argv[1] == "--seal":
        plain, passphrase, out = sys.argv[2:5]
        a = pyrage.passphrase.encrypt(open(plain, "rb").read(), passphrase)
        open(out, "wb").write(from_age(a))
        return
    sealed, plain, passphrase = sys.argv[1:4]
    got = pyrage.passphrase.decrypt(to_age(open(sealed, "rb").read()), passphrase)
    want = open(plain, "rb").read()
    assert got == want, "age decrypts the archive to other bytes"
    print(f"{sealed}: age decrypts it to its payload ({len(got)} bytes)")


main()
