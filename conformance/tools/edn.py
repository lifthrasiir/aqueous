"""A reader of the EDN subset the cases use, and a CBOR encoder.

EDN (RFC 8610 Appendix G, draft-ietf-cbor-edn-literals): numbers (decimal, 0x, 0b, 0o, negative),
"text" with JSON escapes, h'hex' and 'text' byte strings, [arrays], {maps}, tags N(item),
true/false/null/undefined, comments `/ ... /` and `# ...` to the end of the line.
Maps keep their order and may have keys of any kind (`Map`, a list of pairs).
"""

import math
import struct


class Undefined:
    def __repr__(self):
        return "undefined"

    def __eq__(self, other):
        return isinstance(other, Undefined)

    def __hash__(self):
        return 0xF7


UNDEFINED = Undefined()


class Tag:
    __slots__ = ("tag", "value")

    def __init__(self, tag, value):
        self.tag = tag
        self.value = value

    def __repr__(self):
        return f"{self.tag}({self.value!r})"

    def __eq__(self, other):
        return isinstance(other, Tag) and self.tag == other.tag and self.value == other.value

    def __hash__(self):
        return hash((self.tag, freeze(self.value)))


class Map:
    """A CBOR map in the order written."""

    __slots__ = ("items",)

    def __init__(self, items=()):
        self.items = list(items)

    def __repr__(self):
        return "{" + ", ".join(f"{k!r}: {v!r}" for k, v in self.items) + "}"

    def __eq__(self, other):
        return isinstance(other, Map) and self.items == other.items

    def __hash__(self):
        return hash(freeze(self))

    def __len__(self):
        return len(self.items)

    def __iter__(self):
        return iter(k for k, _ in self.items)

    def __contains__(self, key):
        return any(same(k, key) for k, _ in self.items)

    def get(self, key, default=None):
        for k, v in self.items:
            if same(k, key):
                return v
        return default

    def __getitem__(self, key):
        for k, v in self.items:
            if same(k, key):
                return v
        raise KeyError(key)

    def keys(self):
        return [k for k, _ in self.items]


def same(a, b):
    # 1 == True in Python; CBOR tells them apart.
    return type(a) is type(b) and a == b


def freeze(v):
    if isinstance(v, list):
        return ("a", tuple(freeze(x) for x in v))
    if isinstance(v, Map):
        return ("m", tuple((freeze(k), freeze(x)) for k, x in v.items))
    if isinstance(v, Tag):
        return ("t", v.tag, freeze(v.value))
    return (type(v).__name__, v)


class EdnError(Exception):
    pass


class _Reader:
    def __init__(self, text):
        self.s = text
        self.i = 0

    def error(self, msg):
        line = self.s.count("\n", 0, self.i) + 1
        return EdnError(f"line {line}: {msg}")

    def skip(self):
        s = self.s
        while self.i < len(s):
            c = s[self.i]
            if c in " \t\r\n,":
                # Commas are taken as separators anywhere; the cases don't rely on their absence.
                self.i += 1
            elif c == "#":
                j = s.find("\n", self.i)
                self.i = len(s) if j < 0 else j + 1
            elif c == "/":
                j = s.find("/", self.i + 1)
                if j < 0:
                    raise self.error("unterminated comment")
                self.i = j + 1
            else:
                break

    def peek(self):
        self.skip()
        return self.s[self.i] if self.i < len(self.s) else ""

    def expect(self, c):
        if self.peek() != c:
            raise self.error(f"expected {c!r}")
        self.i += 1

    def item(self):
        c = self.peek()
        s = self.s
        if c == "[":
            self.i += 1
            out = []
            while self.peek() != "]":
                if not self.peek():
                    raise self.error("unterminated array")
                out.append(self.item())
            self.i += 1
            return out
        if c == "{":
            self.i += 1
            out = Map()
            while self.peek() != "}":
                if not self.peek():
                    raise self.error("unterminated map")
                k = self.item()
                self.expect(":")
                out.items.append((k, self.item()))
            self.i += 1
            return out
        if c == '"':
            return self.text()
        if c == "'":
            return self.text().encode("utf-8")
        if s.startswith("h'", self.i):
            j = s.find("'", self.i + 2)
            if j < 0:
                raise self.error("unterminated h''")
            body = s[self.i + 2 : j]
            # Comments inside h'' as the EDN draft allows: `/ ... /` and `#` to the end of the line.
            hexd = []
            k = 0
            while k < len(body):
                ch = body[k]
                if ch == "/":
                    e = body.find("/", k + 1)
                    k = len(body) if e < 0 else e + 1
                elif ch == "#":
                    e = body.find("\n", k)
                    k = len(body) if e < 0 else e + 1
                else:
                    if not ch.isspace():
                        hexd.append(ch)
                    k += 1
            self.i = j + 1
            try:
                return bytes.fromhex("".join(hexd))
            except ValueError:
                raise self.error("bad hex in h''")
        if c == "-" or c.isdigit():
            return self.number()
        for word, val in (("true", True), ("false", False), ("null", None), ("undefined", UNDEFINED)):
            if s.startswith(word, self.i) and not (s[self.i + len(word) : self.i + len(word) + 1].isalnum()):
                self.i += len(word)
                return val
        raise self.error(f"unexpected {c!r}")

    def number(self):
        s = self.s
        j = self.i
        if s[j] == "-":
            j += 1
        k = j
        while k < len(s) and (s[k].isalnum() or s[k] in "._"):
            k += 1
        tok = s[self.i : k]
        try:
            n = int(tok.replace("_", ""), 0)
        except ValueError:
            raise self.error(f"bad number {tok!r}")
        self.i = k
        if self.peek() == "(" and n >= 0 and s[self.i] == "(":
            # A tag: digits immediately or after spaces followed by `(`.
            self.i += 1
            v = self.item()
            self.expect(")")
            return Tag(n, v)
        return n

    def text(self):
        q = self.s[self.i]
        self.i += 1
        out = []
        s = self.s
        while True:
            if self.i >= len(s):
                raise self.error("unterminated string")
            c = s[self.i]
            if c == q:
                self.i += 1
                return "".join(out)
            if c == "\\":
                e = s[self.i + 1]
                self.i += 2
                if e == "u":
                    cp = int(s[self.i : self.i + 4], 16)
                    self.i += 4
                    if 0xD800 <= cp < 0xDC00 and s.startswith("\\u", self.i):
                        lo = int(s[self.i + 2 : self.i + 6], 16)
                        if 0xDC00 <= lo < 0xE000:
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)
                            self.i += 6
                    out.append(chr(cp))
                else:
                    out.append({"n": "\n", "t": "\t", "r": "\r", "b": "\b", "f": "\f"}.get(e, e))
            else:
                out.append(c)
                self.i += 1


def loads(text):
    r = _Reader(text)
    v = r.item()
    if r.peek():
        raise r.error("trailing text")
    return v


# --- CBOR (RFC 8949), preferred serialisation, maps in the order given


def _head(major, n):
    if n < 24:
        return bytes([major << 5 | n])
    if n < 0x100:
        return bytes([major << 5 | 24, n])
    if n < 0x10000:
        return bytes([major << 5 | 25]) + n.to_bytes(2, "big")
    if n < 0x100000000:
        return bytes([major << 5 | 26]) + n.to_bytes(4, "big")
    return bytes([major << 5 | 27]) + n.to_bytes(8, "big")


def cbor(v, hook=None):
    """Encodes `v`; `hook(v)` may return the bytes of an item to use instead (or None)."""
    if hook is not None:
        h = hook(v)
        if h is not None:
            return h
    if v is True:
        return b"\xf5"
    if v is False:
        return b"\xf4"
    if v is None:
        return b"\xf6"
    if isinstance(v, Undefined):
        return b"\xf7"
    if isinstance(v, int):
        return _head(0, v) if v >= 0 else _head(1, -1 - v)
    if isinstance(v, bytes):
        return _head(2, len(v)) + v
    if isinstance(v, str):
        b = v.encode("utf-8")
        return _head(3, len(b)) + b
    if isinstance(v, list):
        return _head(4, len(v)) + b"".join(cbor(x, hook) for x in v)
    if isinstance(v, Map):
        return _head(5, len(v)) + b"".join(cbor(k, hook) + cbor(x, hook) for k, x in v.items)
    if isinstance(v, Tag):
        return _head(6, v.tag) + cbor(v.value, hook)
    if isinstance(v, float):
        if math.isnan(v):
            return b"\xf9\x7e\x00"
        return b"\xfb" + struct.pack(">d", v)
    raise TypeError(f"not CBOR: {v!r}")
