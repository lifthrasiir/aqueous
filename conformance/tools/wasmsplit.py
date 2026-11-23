"""The split form of a module block (spec, "Split modules"): `split` makes the 14 streams of a
WebAssembly module, every code and data section split (a function whose expression has an opcode
not in the spec's table is kept as it is), and `join` makes the module again, as the spec tells.

    python wasmsplit.py MODULE.wasm    # splits, joins and checks they agree
"""

import sys

NAMES = ["skeleton", "sections", "custom", "locals", "op", "local", "label", "brtable", "blocktype",
         "offset", "i64", "bytes", "misc", "data"]
S = {n: i for i, n in enumerate(NAMES)}


class SplitError(Exception):
    pass


def leb_bytes(b, at, m=5):
    """The bytes of a LEB128 of at most `m` bytes at `at`, and its value."""
    v, shift, i = 0, 0, at
    while True:
        if i >= len(b) or i - at >= m:
            raise SplitError("a LEB128 past its bound or the end")
        x = b[i]
        if shift < 64:
            v |= (x & 0x7F) << shift
        shift += 7
        i += 1
        if not x & 0x80:
            return b[at:i], v


def write(n):
    out = bytearray()
    while True:
        x = n & 0x7F
        n >>= 7
        out.append(x | (0x80 if n else 0))
        if not n:
            return bytes(out)


def zz(v):
    return 2 * v if v >= 0 else -2 * v - 1


def unzz(v):
    return v // 2 if v % 2 == 0 else -(v + 1) // 2


# --- split


class Splitter:
    def __init__(self):
        self.s = [bytearray() for _ in NAMES]

    def put(self, name, b):
        self.s[S[name]] += b


def split_expr(sp, b, at):
    """Splits the expression at `at` of `b`, returning where it ends."""
    depth, recent, nxt = 0, [], 0

    def leb(name, m=5):
        nonlocal at
        raw, v = leb_bytes(b, at, m)
        sp.put(name, raw)
        at += len(raw)
        return v

    def take(name, n):
        nonlocal at
        if at + n > len(b):
            raise SplitError("past the end")
        sp.put(name, b[at : at + n])
        at += n

    def memarg():
        nonlocal at
        a = leb("op")
        if a & 0x40:
            leb("misc")
        raw, _ = leb_bytes(b, at, 10)
        sp.put("op", raw[:1])
        sp.put("offset", raw[1:])
        at += len(raw)

    while True:
        if at >= len(b):
            raise SplitError("an expression past the end")
        op = b[at]
        sp.put("op", bytes([op]))
        at += 1
        if op in (0x02, 0x03, 0x04):
            leb("blocktype")
            depth += 1
        elif op == 0x0B:
            if depth == 0:
                return at
            depth -= 1
        elif op in (0x0C, 0x0D):
            leb("label")
        elif op == 0x0E:
            n = leb("brtable")
            for _ in range(n + 1):
                leb("brtable")
        elif op in (0x10, 0x12, 0x41):
            leb("op")
        elif op in (0x11, 0x13):
            leb("misc")
            leb("misc")
        elif op == 0x1C:
            take("misc", leb("misc"))
        elif op in (0x20, 0x21, 0x22):
            raw, i = leb_bytes(b, at, 5)
            if raw != write(i):
                raise SplitError("a local index not in the shortest form")
            at += len(raw)
            c = recent.index(i) if i in recent else len(recent) + zz(i - nxt)
            sp.put("local", write(c))
            if i in recent:
                recent.remove(i)
            recent.insert(0, i)
            del recent[256:]
            nxt = max(nxt, i + 1)
        elif 0x23 <= op <= 0x26 or op in (0x3F, 0x40, 0xD0, 0xD2):
            leb("misc")
        elif 0x28 <= op <= 0x3E:
            memarg()
        elif op == 0x42:
            leb("i64", 10)
        elif op == 0x43:
            take("bytes", 4)
        elif op == 0x44:
            take("bytes", 8)
        elif op in (0x00, 0x01, 0x05, 0x0F, 0x1A, 0x1B, 0xD1) or 0x45 <= op <= 0xC4:
            pass
        elif op == 0xFC:
            x = leb("op")
            if x <= 7:
                pass
            elif x in (8, 10, 12, 14):
                leb("misc")
                leb("misc")
            elif x in (9, 11, 13) or 15 <= x <= 17:
                leb("misc")
            else:
                raise SplitError(f"FC {x}")
        elif op == 0xFD:
            x = leb("op")
            if x <= 11 or x in (92, 93):
                memarg()
            elif x in (12, 13):
                take("bytes", 16)
            elif 21 <= x <= 34:
                take("bytes", 1)
            elif 84 <= x <= 91:
                memarg()
                take("bytes", 1)
            elif not (14 <= x <= 20 or 35 <= x <= 83 or 94 <= x <= 255):
                raise SplitError(f"FD {x}")
        else:
            raise SplitError(f"opcode {op:#x}")


def split(m):
    """The 14 streams of module `m`."""
    sp = Splitter()
    if m[:8] != b"\0asm\x01\0\0\0":
        raise SplitError("not a module")
    sp.put("skeleton", m[:8])
    at = 8
    while at < len(m):
        sid = m[at]
        raw, n = leb_bytes(m, at + 1)
        body = m[at + 1 + len(raw) : at + 1 + len(raw) + n]
        at += 1 + len(raw) + n
        sp.put("skeleton", bytes([sid]))
        if sid in (10, 11) and raw == write(n):
            snap = [bytes(x) for x in sp.s]
            try:
                split_section(sp, sid, body)
                continue
            except SplitError:
                sp.s = [bytearray(x) for x in snap]
        if sid in (10, 11):
            sp.put("skeleton", b"\x01")
        sp.put("skeleton", raw)
        sp.put("custom" if sid == 0 else "sections", body)
    return [bytes(x) for x in sp.s]


def split_section(sp, sid, body):
    sp.put("skeleton", b"\x00")
    raw, count = leb_bytes(body, 0)
    sp.put("skeleton", raw)
    at = len(raw)
    for _ in range(count):
        if sid == 10:
            fraw, size = leb_bytes(body, at)
            f = body[at + len(fraw) : at + len(fraw) + size]
            snap = [bytes(x) for x in sp.s]
            try:
                if fraw != write(size):
                    raise SplitError("a function size not in the shortest form")
                sp.put("skeleton", b"\x00")
                q = 0
                r, n = leb_bytes(f, q)
                sp.put("locals", r)
                q += len(r)
                for _ in range(n):
                    r, _ = leb_bytes(f, q)
                    sp.put("locals", r + f[q + len(r) : q + len(r) + 1])
                    q += len(r) + 1
                if split_expr(sp, f, q) != len(f):
                    raise SplitError("bytes after a function's expression")
            except SplitError:
                sp.s = [bytearray(x) for x in snap]
                sp.put("skeleton", b"\x01")
                sp.put("sections", fraw + f)
            at += len(fraw) + size
        else:
            def leb(name):
                nonlocal at
                r, v = leb_bytes(body, at)
                sp.put(name, r)
                at += len(r)
                return v

            kind = leb("misc")
            if kind == 2:
                leb("misc")
            if kind != 1:
                at = split_expr(sp, body, at)
            n = leb("misc")
            sp.put("data", body[at : at + n])
            at += n
    if at != len(body):
        raise SplitError("bytes after the section's items")


# --- join, as the spec tells


class Reader:
    def __init__(self, streams):
        self.s = list(streams)
        self.at = [0] * len(streams)

    def take(self, name, n):
        i = S[name]
        if self.at[i] + n > len(self.s[i]):
            raise SplitError(f"{name}: read past its end")
        b = self.s[i][self.at[i] : self.at[i] + n]
        self.at[i] += n
        return b

    def leb(self, name, m=5):
        i = S[name]
        raw, v = leb_bytes(self.s[i], self.at[i], m)
        self.at[i] += len(raw)
        return raw, v


def join(streams):
    r = Reader(streams)
    out = bytearray(r.take("skeleton", 8))
    if bytes(out) != b"\0asm\x01\0\0\0":
        raise SplitError("not the header")
    while r.at[0] < len(r.s[0]):
        sid = r.take("skeleton", 1)[0]
        out.append(sid)
        if sid in (10, 11):
            k = r.take("skeleton", 1)[0]
            if k not in (0, 1):
                raise SplitError("k")
            if k == 0:
                c = join_section(r, sid)
                out += write(len(c)) + c
                continue
        raw, n = r.leb("skeleton")
        out += raw + r.take("custom" if sid == 0 else "sections", n)
    if any(a != len(s) for a, s in zip(r.at, r.s)):
        raise SplitError("bytes left in a stream")
    return bytes(out)


def join_section(r, sid):
    c = bytearray()
    raw, count = r.leb("skeleton")
    c += raw
    for _ in range(count):
        if sid == 10:
            f = r.take("skeleton", 1)[0]
            if f == 0:
                b = bytearray()
                raw, n = r.leb("locals")
                b += raw
                for _ in range(n):
                    raw, _ = r.leb("locals")
                    b += raw + r.take("locals", 1)
                b += join_expr(r)
                c += write(len(b)) + b
            elif f == 1:
                raw, n = r.leb("sections")
                c += raw + r.take("sections", n)
            else:
                raise SplitError("f")
        else:
            raw, kind = r.leb("misc")
            c += raw
            if kind == 2:
                c += r.leb("misc")[0]
            if kind != 1:
                c += join_expr(r)
            raw, n = r.leb("misc")
            c += raw + r.take("data", n)
    return bytes(c)


def join_expr(r):
    out, depth, recent, nxt = bytearray(), 0, [], 0

    def memarg():
        raw, a = r.leb("op")
        out.extend(raw)
        if a & 0x40:
            out.extend(r.leb("misc")[0])
        first = r.take("op", 1)
        out.extend(first)
        if first[0] & 0x80:
            # the rest of the offset: a LEB128 continuing it, within 10 bytes in all
            i = S["offset"]
            raw, _ = leb_bytes(r.s[i], r.at[i], 9)
            r.at[i] += len(raw)
            out.extend(raw)

    while True:
        op = r.take("op", 1)[0]
        out.append(op)
        if op in (0x02, 0x03, 0x04):
            out += r.leb("blocktype")[0]
            depth += 1
        elif op == 0x0B:
            if depth == 0:
                return bytes(out)
            depth -= 1
        elif op in (0x0C, 0x0D):
            out += r.leb("label")[0]
        elif op == 0x0E:
            raw, n = r.leb("brtable")
            out += raw
            for _ in range(n + 1):
                out += r.leb("brtable")[0]
        elif op in (0x10, 0x12, 0x41):
            out += r.leb("op")[0]
        elif op in (0x11, 0x13):
            out += r.leb("misc")[0] + r.leb("misc")[0]
        elif op == 0x1C:
            raw, n = r.leb("misc")
            out += raw + r.take("misc", n)
        elif op in (0x20, 0x21, 0x22):
            _, c = r.leb("local", 10)
            i = recent[c] if c < len(recent) else nxt + unzz(c - len(recent))
            if not 0 <= i < 1 << 32:
                raise SplitError("a local index out of range")
            out += write(i)
            if i in recent:
                recent.remove(i)
            recent.insert(0, i)
            del recent[256:]
            nxt = max(nxt, i + 1)
        elif 0x23 <= op <= 0x26 or op in (0x3F, 0x40, 0xD0, 0xD2):
            out += r.leb("misc")[0]
        elif 0x28 <= op <= 0x3E:
            memarg()
        elif op == 0x42:
            out += r.leb("i64", 10)[0]
        elif op == 0x43:
            out += r.take("bytes", 4)
        elif op == 0x44:
            out += r.take("bytes", 8)
        elif op in (0x00, 0x01, 0x05, 0x0F, 0x1A, 0x1B, 0xD1) or 0x45 <= op <= 0xC4:
            pass
        elif op == 0xFC:
            raw, x = r.leb("op")
            out += raw
            if x in (8, 10, 12, 14):
                out += r.leb("misc")[0] + r.leb("misc")[0]
            elif x in (9, 11, 13) or 15 <= x <= 17:
                out += r.leb("misc")[0]
            elif x > 7:
                raise SplitError(f"FC {x}")
        elif op == 0xFD:
            raw, x = r.leb("op")
            out += raw
            if x <= 11 or x in (92, 93):
                memarg()
            elif x in (12, 13):
                out += r.take("bytes", 16)
            elif 21 <= x <= 34:
                out += r.take("bytes", 1)
            elif 84 <= x <= 91:
                memarg()
                out += r.take("bytes", 1)
            elif not (14 <= x <= 20 or 35 <= x <= 83 or 94 <= x <= 255):
                raise SplitError(f"FD {x}")
        else:
            raise SplitError(f"opcode {op:#x}")


if __name__ == "__main__":
    for p in sys.argv[1:]:
        m = open(p, "rb").read()
        streams = split(m)
        assert join(streams) == m, p
        print(p, len(m), [len(s) for s in streams])
