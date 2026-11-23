"""XXH64 (seed 0 by default), as the checksums of the specification use it."""

M = (1 << 64) - 1
P1 = 0x9E3779B185EBCA87
P2 = 0xC2B2AE3D27D4EB4F
P3 = 0x165667B19E3779F9
P4 = 0x85EBCA77C2B2AE63
P5 = 0x27D4EB2F165667C5


def _rotl(x, r):
    return ((x << r) | (x >> (64 - r))) & M


def _round(acc, lane):
    acc = (acc + lane * P2) & M
    return (_rotl(acc, 31) * P1) & M


def _merge(acc, v):
    acc ^= _round(0, v)
    return (acc * P1 + P4) & M


def xxh64(data, seed=0):
    n = len(data)
    i = 0
    if n >= 32:
        v1 = (seed + P1 + P2) & M
        v2 = (seed + P2) & M
        v3 = seed
        v4 = (seed - P1) & M
        while i + 32 <= n:
            v1 = _round(v1, int.from_bytes(data[i : i + 8], "little"))
            v2 = _round(v2, int.from_bytes(data[i + 8 : i + 16], "little"))
            v3 = _round(v3, int.from_bytes(data[i + 16 : i + 24], "little"))
            v4 = _round(v4, int.from_bytes(data[i + 24 : i + 32], "little"))
            i += 32
        h = (_rotl(v1, 1) + _rotl(v2, 7) + _rotl(v3, 12) + _rotl(v4, 18)) & M
        for v in (v1, v2, v3, v4):
            h = _merge(h, v)
    else:
        h = (seed + P5) & M
    h = (h + n) & M
    while i + 8 <= n:
        h ^= _round(0, int.from_bytes(data[i : i + 8], "little"))
        h = (_rotl(h, 27) * P1 + P4) & M
        i += 8
    if i + 4 <= n:
        h ^= (int.from_bytes(data[i : i + 4], "little") * P1) & M
        h = (_rotl(h, 23) * P2 + P3) & M
        i += 4
    while i < n:
        h ^= (data[i] * P5) & M
        h = (_rotl(h, 11) * P1) & M
        i += 1
    h ^= h >> 33
    h = (h * P2) & M
    h ^= h >> 29
    h = (h * P3) & M
    h ^= h >> 32
    return h


if __name__ == "__main__":
    # Reference values of xxHash.
    assert xxh64(b"") == 0xEF46DB3751D8E999
    assert xxh64(b"a") == 0xD24EC4F1A98C6E5B
    assert xxh64(b"abc") == 0x44BC2CF5AD770999
    assert xxh64(b"Nobody inspects the spammish repetition") == 0xFBCEA83C8A378BF1
    print("ok")
