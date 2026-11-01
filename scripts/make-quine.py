#!/usr/bin/env python3
"""Write an Aqueous archive holding a single file whose content is the archive itself.

    python3 scripts/make-quine.py OUT.aqr

No module is used: the formula takes the stored bytes of a resource block twice
(ResourceBytes), once as they are and once split into pieces, and concatenates them.
The resource's data is Zstandard with a single raw block, so its stored bytes are

    h = original size, window descriptor, raw block header  (6 bytes)
    T = every byte of the archive outside the resource block's data and the ignored block's
    ck = XXH64(T)                                            (8 bytes)

and the archive is laid out as

    T1 = header magic, resource block's type and size
    S  = h T ck                         (the resource block's data)
    T2 = resource block's padding, formula block, ignored block's type and size
    h ck                                (the ignored block's data, type E0: an experiment)
    T3 = ignored block's padding, metadata block, footer

with T = T1 T2 T3. The formula is ResourceBytes, Split [h, T1, T2, T3, ck], ResourceBytes,
Swaps to [T1, S, T2, h, ck, T3], Concatenate. No checksum depends on itself: ck covers T,
which holds only sizes of S, and the stored bytes carry ck themselves. The only fixed point
is in the sizes (LEB128 lengths), found by iterating. The file has no output checksum: one
covering the whole output would be a checksum of bytes containing it.
"""

import sys

M64 = (1 << 64) - 1
P1, P2, P3, P4, P5 = (
    0x9E3779B185EBCA87,
    0xC2B2AE3D27D4EB4F,
    0x165667B19E3779F9,
    0x85EBCA77C2B2AE63,
    0x27D4EB2F165667C5,
)


def rotl(x, r):
    return ((x << r) | (x >> (64 - r))) & M64


def xxh64(data, seed=0):
    def rnd(acc, lane):
        return (rotl((acc + lane * P2) & M64, 31) * P1) & M64

    def merge(acc, v):
        return ((acc ^ rnd(0, v)) * P1 + P4) & M64

    n, i = len(data), 0
    if n >= 32:
        v = [(seed + P1 + P2) & M64, (seed + P2) & M64, seed, (seed - P1) & M64]
        while i + 32 <= n:
            for k in range(4):
                v[k] = rnd(v[k], int.from_bytes(data[i + 8 * k : i + 8 * k + 8], "little"))
            i += 32
        h = (rotl(v[0], 1) + rotl(v[1], 7) + rotl(v[2], 12) + rotl(v[3], 18)) & M64
        for x in v:
            h = merge(h, x)
    else:
        h = (seed + P5) & M64
    h = (h + n) & M64
    while i + 8 <= n:
        h = (rotl(h ^ rnd(0, int.from_bytes(data[i : i + 8], "little")), 27) * P1 + P4) & M64
        i += 8
    if i + 4 <= n:
        h = (rotl(h ^ (int.from_bytes(data[i : i + 4], "little") * P1) & M64, 23) * P2 + P3) & M64
        i += 4
    while i < n:
        h = (rotl(h ^ (data[i] * P5) & M64, 11) * P1) & M64
        i += 1
    h = ((h ^ (h >> 33)) * P2) & M64
    h = ((h ^ (h >> 29)) * P3) & M64
    return (h ^ (h >> 32)).to_bytes(8, "little")


assert xxh64(b"").hex() == "99e9d85137db46ef"  # 0xEF46DB3751D8E999


def leb(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def block(typ, data):
    """A block of type `typ` (whose low two bits must give a width fitting `data`)."""
    width = {0: 1, 1: 2, 2: 4, 3: 8}[typ % 4]
    assert len(data) < 1 << (8 * width)
    b = leb(typ) + len(data).to_bytes(width, "little") + data
    return b + b"\0" * (len(b) % 2)


def raw_zstd(data):
    """Compressed external structure: Zstandard of a single raw block, then XXH64."""
    assert len(data) <= 1024  # window descriptor 0: a window of 1 KiB
    head = b"\0" + leb(3)  # original size = compressed size - 3 (the raw block header)
    hdr = (1 | len(data) << 3).to_bytes(3, "little")  # last block, raw
    return head + b"\0" + hdr, data, xxh64(data)


HEADER_MAGIC = bytes.fromhex("F804036AD6BE")
FOOTER_MAGIC = bytes.fromhex("FDAAAEDE8512")
FORMULA, METADATA, IGNORED = 0x70, 0x10, 0xE0
NAME = b"quine.aqr"

COMMENT = (
    "quine.aqr is this archive itself: its formula takes the stored bytes of the resource block"
    " twice, splits one of them and puts the pieces around the other (scripts/make-quine.py)"
).encode()


def cbor_text(b):
    if len(b) < 24:
        return bytes([0x60 + len(b)]) + b
    if len(b) < 256:
        return bytes([0x78, len(b)]) + b
    return bytes([0x79]) + len(b).to_bytes(2, "big") + b


OPTIONAL = b"\xa1\x20" + cbor_text(COMMENT)  # {archive-comment (-1): COMMENT}

# Sizes guessed and then refined until the archive agrees with them.
#
# Every block is of even length, and so is the ignored block's data (h and ck), so T's
# length is the resource block's padding plus the footer's length, modulo 2. The padding is
# the parity of T's length itself, so with an odd footer the sizes flip back and forth. The
# footer is odd when exactly one of its two LEB128 fields is 2 bytes long: the comment above
# makes the metadata block long enough for both to be.
t1 = t2 = t3 = 0
for _ in range(20):
    s_len = 6 + (t1 + t2 + t3) + 8
    total = 2 * (t1 + t2 + t3) + 28

    # Formula: ResourceBytes, Split, ResourceBytes, Swaps, Concatenate.
    insns = bytes([0x03]) + leb(0) + leb(s_len)
    insns += bytes([0x14]) + leb(5) + b"".join(leb(n) for n in (6, t1, t2, t3, 8))
    insns += bytes([0x03]) + leb(0) + leb(s_len)
    stack = ["h", "T1", "T2", "T3", "ck", "S"]
    want = ["T1", "S", "T2", "h", "ck", "T3"]
    top = len(stack) - 1

    def swap(d):
        global insns
        insns += bytes([0x0F]) + leb(d)
        stack[top], stack[top - d] = stack[top - d], stack[top]

    for i in range(top + 1):
        if stack[i] == want[i]:
            continue
        j = stack.index(want[i])
        if j != top:
            swap(top - j)  # bring it to the top
        if i != top:
            swap(top - i)  # and down to its place
    assert stack == want
    insns += bytes([0x12]) + leb(6)
    formula = leb(0) + leb(1) + leb(0) + leb(0) + insns  # no blocks, resource 0, no modules
    h_f, d_f, ck_f = raw_zstd(formula)

    width = 1 if s_len < 256 else 2
    t1_bytes = HEADER_MAGIC + leb(0x64 + width - 1) + s_len.to_bytes(width, "little")
    r_pad = b"\0" * ((1 + width + s_len) % 2)
    f_block = block(FORMULA, leb(1) + h_f + d_f + ck_f)
    ign_head = leb(IGNORED) + bytes([14])
    t2_bytes = r_pad + f_block + ign_head
    ign_pad = b"\0" * ((len(ign_head) + 14) % 2)

    # Offsets within the archive.
    r_start = len(HEADER_MAGIC)
    f_start = r_start + 1 + width + s_len + len(r_pad)
    m_start = f_start + len(f_block) + len(ign_head) + 14 + len(ign_pad)
    entry = NAME + b"\0" + leb(total)
    prev = m_start - len(HEADER_MAGIC) + 1  # to the end of the header magic, plus 1

    def u(n, v):
        w = 0 if n == 0 else 1 if n < 1 << 8 else 2 if n < 1 << 16 else 4 if n < 1 << 32 else 8
        return v.to_bytes(w, "little")

    meta = leb(prev) + leb(0) + leb(1) + leb(0)  # modules, resources, dictionaries
    meta += leb(0) + leb(1) + leb(0)  # directories, files, specials
    meta += leb(len(entry)) + leb(len(OPTIONAL))
    meta += u(prev, m_start - r_start)  # resource offset
    meta += u(0, 0) + u(prev, m_start - f_start)  # file: no dictionary, the formula's output 0
    meta += entry + OPTIONAL
    m_block = block(METADATA, meta + xxh64(meta))

    archive_end = m_start + len(m_block)
    footer_tail = leb(archive_end - len(HEADER_MAGIC)) + leb(len(m_block) + 8)
    # metadata_backward_offset: from footer_start back to the metadata block,
    # past the footer magic (6) and footer_size (2).
    footer = FOOTER_MAGIC + len(footer_tail).to_bytes(2, "little") + footer_tail
    footer += len(footer_tail).to_bytes(2, "little")
    t3_bytes = ign_pad + m_block + footer

    new = (len(t1_bytes), len(t2_bytes), len(t3_bytes))
    if new == (t1, t2, t3):
        break
    t1, t2, t3 = new
else:
    sys.exit("sizes did not settle")

T = t1_bytes + t2_bytes + t3_bytes
h_r, d_r, ck_r = raw_zstd(T)
S = h_r + d_r + ck_r
out = t1_bytes + S + t2_bytes + h_r + ck_r + t3_bytes
assert len(footer_tail) % 2 == 0
assert len(out) == total and len(S) == s_len and out[m_start:archive_end] == m_block

with open(sys.argv[1], "wb") as f:
    f.write(out)
print(f"{sys.argv[1]}: {len(out)} bytes")
