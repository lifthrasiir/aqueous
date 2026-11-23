"""What an encrypted archive takes of age v1 (age-encryption.org/v1): the scrypt recipient stanza,
the header MAC and the STREAM payload, with ChaCha20-Poly1305 (RFC 8439) in Python alone. Slow,
but the cases are small.
"""

import base64
import hashlib
import hmac
import struct

CHUNK = 64 * 1024


# --- ChaCha20-Poly1305 (RFC 8439)


def _rotl(v, c):
    return ((v << c) & 0xFFFFFFFF) | (v >> (32 - c))


def _quarter(s, a, b, c, d):
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl(s[b] ^ s[c], 7)


def _block(key, counter, nonce):
    state = [0x61707865, 0x3320646E, 0x79622D32, 0x6B206574]
    state += list(struct.unpack("<8I", key)) + [counter] + list(struct.unpack("<3I", nonce))
    s = state[:]
    for _ in range(10):
        _quarter(s, 0, 4, 8, 12)
        _quarter(s, 1, 5, 9, 13)
        _quarter(s, 2, 6, 10, 14)
        _quarter(s, 3, 7, 11, 15)
        _quarter(s, 0, 5, 10, 15)
        _quarter(s, 1, 6, 11, 12)
        _quarter(s, 2, 7, 8, 13)
        _quarter(s, 3, 4, 9, 14)
    return struct.pack("<16I", *((x + y) & 0xFFFFFFFF for x, y in zip(s, state)))


def chacha20(key, counter, nonce, data):
    out = bytearray()
    for i in range(0, len(data), 64):
        ks = _block(key, counter + i // 64, nonce)
        out += bytes(a ^ b for a, b in zip(data[i : i + 64], ks))
    return bytes(out)


def poly1305(key, msg):
    r = int.from_bytes(key[:16], "little") & 0x0FFFFFFC0FFFFFFC0FFFFFFC0FFFFFFF
    s = int.from_bytes(key[16:], "little")
    p = (1 << 130) - 5
    acc = 0
    for i in range(0, len(msg), 16):
        n = int.from_bytes(msg[i : i + 16] + b"\x01", "little")
        acc = (acc + n) * r % p
    return ((acc + s) & ((1 << 128) - 1)).to_bytes(16, "little")


def _mac_data(aad, ct):
    pad = lambda b: b + bytes(-len(b) % 16)
    return pad(aad) + pad(ct) + struct.pack("<QQ", len(aad), len(ct))


def seal(key, nonce, plaintext, aad=b""):
    otk = _block(key, 0, nonce)[:32]
    ct = chacha20(key, 1, nonce, plaintext)
    return ct + poly1305(otk, _mac_data(aad, ct))


def open_(key, nonce, sealed, aad=b""):
    """The plaintext, or None where the tag doesn't verify."""
    ct, tag = sealed[:-16], sealed[-16:]
    otk = _block(key, 0, nonce)[:32]
    if not hmac.compare_digest(poly1305(otk, _mac_data(aad, ct)), tag):
        return None
    return chacha20(key, 1, nonce, ct)


# --- age v1


def hkdf(ikm, salt, info, n=32):
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    out, t, i = b"", b"", 1
    while len(out) < n:
        t = hmac.new(prk, t + info + bytes([i]), hashlib.sha256).digest()
        out += t
        i += 1
    return out[:n]


def b64(b):
    return base64.b64encode(b).decode().rstrip("=")


def wrap_key(file_key, passphrase, salt, lg):
    """The body of the scrypt stanza: the file key wrapped by the passphrase."""
    key = hashlib.scrypt(passphrase, salt=b"age-encryption.org/v1/scrypt" + salt, n=1 << lg, r=8, p=1,
                         dklen=32, maxmem=(1 << 31) - 1)
    return seal(key, bytes(12), file_key)


def header_text(salt, lg, wrapped):
    """The textual header up to and including `---`, which the MAC covers."""
    return f"age-encryption.org/v1\n-> scrypt {b64(salt)} {lg}\n{b64(wrapped)}\n---".encode()


def header_mac(file_key, salt, lg, wrapped):
    return hmac.new(hkdf(file_key, b"", b"header"), header_text(salt, lg, wrapped), hashlib.sha256).digest()


def chunk_nonce(i, last):
    return i.to_bytes(11, "big") + (b"\x01" if last else b"\x00")


def encrypt_payload(file_key, nonce, plaintext, last_flag=True):
    """The encrypted chunks of `plaintext` (`last_flag` False leaves the last chunk unmarked)."""
    key = hkdf(file_key, nonce, b"payload")
    pieces = [plaintext[i : i + CHUNK] for i in range(0, len(plaintext), CHUNK)] or [b""]
    return [seal(key, chunk_nonce(i, last_flag and i == len(pieces) - 1), p) for i, p in enumerate(pieces)]


def age_file(salt, lg, wrapped, mac, nonce, chunks):
    """The age file of the same encryption, for decrypting it with age itself."""
    return header_text(salt, lg, wrapped) + b" " + b64(mac).encode() + b"\n" + nonce + b"".join(chunks)


if __name__ == "__main__":
    # RFC 8439 section 2.8.2
    key = bytes(range(0x80, 0xA0))
    nonce = bytes.fromhex("070000004041424344454647")
    aad = bytes.fromhex("50515253c0c1c2c3c4c5c6c7")
    pt = (b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, "
          b"sunscreen would be it.")
    sealed = seal(key, nonce, pt, aad)
    assert sealed[-16:].hex() == "1ae10b594f09e26a7e902ecbd0600691", sealed[-16:].hex()
    assert open_(key, nonce, sealed, aad) == pt
    print("ok")
