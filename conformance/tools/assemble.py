"""Assembles the bytes of a case's `archive` (conformance/case.cddl).

    python assemble.py CASE.edn [-o OUT]

Writes `leading`, the archive and `trailing` to OUT (CASE with `.aqr`, by default).

A prototype: unencrypted archives of spacer, size hint, metadata and data blocks, and any block
given as its bytes. Labels are resolved by assembling again until they stay where they were.
"""

import os
import sys

from edn import UNDEFINED, Map, Tag, Undefined, cbor, loads
from xxh64 import xxh64

RAW, LABEL_START, LABEL_END, EXT = 80000, 80001, 80002, 80003

CONSTANTS = {
    "HEADER_MAGIC": bytes.fromhex("F804036AD6BE"),
    "OVERLAY_HEADER_MAGIC": bytes.fromhex("690D15AFF6B2"),
    "FOOTER_MAGIC": bytes.fromhex("FDAAAEDE8512"),
    "OVERLAY_FOOTER_MAGIC": bytes.fromhex("FC0F6E7B4BAC"),
}

ZSTD_BLOCK_TYPES = {"Raw_Block": 0, "RLE_Block": 1, "Compressed_Block": 2, "Reserved": 3}


class CaseError(Exception):
    pass


# --- Encodings of the specification


def leb128(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def u_width(n):
    """The byte length of u(n)."""
    if n == 0:
        return 0
    if n < 1 << 8:
        return 1
    if n < 1 << 16:
        return 2
    if n < 1 << 32:
        return 4
    return 8


def ule(n, width):
    if n >= 1 << (8 * width) and width:
        raise CaseError(f"{n} doesn't fit in {width} bytes")
    if width == 0 and n:
        raise CaseError(f"{n} doesn't fit in a zero-sized field")
    return n.to_bytes(width, "little")


# --- Values of the notation


def is_infer(v):
    return isinstance(v, Undefined) or (isinstance(v, Tag) and v.tag == RAW and isinstance(v.value, Undefined))


def is_raw(v):
    return isinstance(v, bytes) or (isinstance(v, Tag) and v.tag == RAW and isinstance(v.value, bytes))


def raw_bytes(v):
    return v if isinstance(v, bytes) else v.value


def is_diff(v):
    if isinstance(v, Tag) and v.tag == RAW:
        v = v.value
    return isinstance(v, list) and len(v) in (2, 3) and all(isinstance(x, str) for x in v[:2])


class Writer:
    """Bytes of one byte sequence being made, with the labels in them."""

    def __init__(self):
        self.buf = bytearray()
        self.labels = []

    def __len__(self):
        return len(self.buf)

    def put(self, b):
        self.buf += b

    def append(self, child):
        for name, pos in child.labels:
            self.labels.append((name, len(self.buf) + pos))
        self.buf += child.buf


class Assembler:
    def __init__(self, case_dir):
        self.case_dir = case_dir
        self.prev = {}  # label -> (space, position) of the previous pass
        self.prev_info = {}
        self.unresolved = False

    # Each pass gives every byte sequence a space of its own, numbered in the order they are made,
    # the same in every pass as long as the structure doesn't change.

    def run(self, case):
        for _ in range(16):
            self.cur = {}
            self.info = {}
            self.spaces = 0
            self.unresolved = False
            out = self.case_bytes(case)
            if not self.unresolved and self.cur == self.prev and self.info == self.prev_info:
                return out
            self.prev, self.prev_info = self.cur, self.info
        raise CaseError("the labels don't settle: a value depends on its own encoding")

    def commit(self, w):
        space = self.spaces
        self.spaces += 1
        for name, pos in w.labels:
            if name in self.cur:
                raise CaseError(f"label {name!r} given twice")
            self.cur[name] = (space, pos)
        return space

    def diff(self, v):
        if isinstance(v, Tag):
            v = v.value
        a, b = v[0], v[1]
        k = v[2] if len(v) == 3 else 0
        if a not in self.prev or b not in self.prev:
            self.unresolved = True
            return 0
        (sa, pa), (sb, pb) = self.prev[a], self.prev[b]
        if sa != sb:
            raise CaseError(f"labels {a!r} and {b!r} are of different byte sequences")
        if pa > pb:
            raise CaseError(f"label {a!r} is after {b!r}")
        return pb - pa + k

    # A value may be wrapped in labels; `field` takes them off and records them around what
    # `fn(writer, value)` writes.

    def field(self, w, v, fn):
        starts, ends = [], []
        while isinstance(v, Tag) and v.tag in (LABEL_START, LABEL_END):
            if not (isinstance(v.value, list) and len(v.value) == 2):
                raise CaseError(f"bad label {v!r}")
            if v.tag == LABEL_START:
                starts.append(v.value[0])
                v = v.value[1]
            else:
                ends.append(v.value[1])
                v = v.value[0]
        for s in starts:
            w.labels.append((s, len(w)))
        r = fn(w, v)
        for e in reversed(ends):
            w.labels.append((e, len(w)))
        return r

    def unlabel(self, v):
        while isinstance(v, Tag) and v.tag in (LABEL_START, LABEL_END):
            v = v.value[1] if v.tag == LABEL_START else v.value[0]
        return v

    def num(self, v, what):
        """The number `v` gives: an int, raw bytes, or None where it is to be inferred."""
        v = self.unlabel(v)
        if v is None or is_infer(v):
            return None
        if isinstance(v, bool):
            raise CaseError(f"{what}: not a number: {v!r}")
        if isinstance(v, int):
            if v < 0:
                raise CaseError(f"{what}: negative")
            return v
        if is_diff(v):
            return self.diff(v)
        if is_raw(v):
            return raw_bytes(v)
        raise CaseError(f"{what}: not a number: {v!r}")

    def bytes_of(self, v, what):
        v = self.unlabel(v)
        if isinstance(v, bytes):
            return v
        if isinstance(v, Tag) and v.tag == RAW and isinstance(v.value, bytes):
            return v.value
        if isinstance(v, Tag) and v.tag == EXT and isinstance(v.value, str):
            with open(os.path.join(self.case_dir, v.value), "rb") as f:
                return f.read()
        raise CaseError(f"{what}: not bytes: {v!r}")

    def put_num(self, w, m, key, enc, inferred=None):
        """Writes the field `key` of the map `m` by `enc` (a function of an int), returning its value
        (None for raw bytes). `inferred` is the value to use if it is left to inference."""
        v = m.get(key, UNDEFINED) if isinstance(m, Map) else m

        def go(w, v):
            n = self.num(v, key)
            if n is None:
                if inferred is None:
                    raise CaseError(f"{key}: can't be inferred here")
                n = inferred() if callable(inferred) else inferred
            if isinstance(n, (bytes, bytearray)):
                w.put(n)
                return None
            w.put(enc(n))
            return n

        return self.field(w, v, go)

    # --- The file

    def case_bytes(self, case):
        out = bytearray()
        if "leading" in case:
            out += self.bytes_of(case["leading"], "leading")
        a = case["archive"]
        root = Writer()
        self.field(root, a, self.archive)
        self.commit(root)
        out += root.buf
        if "trailing" in case:
            out += self.bytes_of(case["trailing"], "trailing")
        return bytes(out)

    def archive(self, w, a):
        if is_raw(a) or (isinstance(a, Tag) and a.tag == EXT):
            w.put(self.bytes_of(a, "archive"))
            return
        if "encrypted_header_magic" in a:
            raise CaseError("encrypted archives are not assembled yet")
        self.magic(w, a, "header_magic")
        self.archive_start = len(w)
        self.segment = []
        self.last_meta = None
        self.prev_meta = None
        self.header_end = len(w)
        if "size_hint_block" in a:
            self.field(w, a["size_hint_block"], self.block)
        blocks = a["blocks"]
        if not isinstance(blocks, list):
            raise CaseError("blocks: not a list")
        for b in blocks:
            self.field(w, b, self.block)
        archive_end = len(w)
        self.info["archive_end"] = archive_end
        self.magic(w, a, "footer_magic")

        tail = Writer()
        self.put_num(tail, a, "size_hint", leb128, archive_end - self.archive_start)
        footer_start = len(w) + 2  # after footer_size, if it is 2 bytes as it should be
        mbo = 0 if self.last_meta is None else footer_start - self.last_meta
        self.put_num(tail, a, "metadata_backward_offset", leb128, mbo)
        self.put_num(w, a, "footer_size", lambda n: ule(n, 2), len(tail))
        w.append(tail)
        self.put_num(w, a, "footer_size_copy", lambda n: ule(n, 2), len(tail))

    def magic(self, w, a, key):
        def go(w, v):
            if isinstance(v, str):
                if v not in CONSTANTS:
                    raise CaseError(f"{key}: unknown constant {v!r}")
                w.put(CONSTANTS[v])
            else:
                w.put(self.bytes_of(v, key))

        self.field(w, a[key], go)

    # --- Blocks

    def block(self, w, b):
        start = len(w)
        if is_raw(b):
            w.put(raw_bytes(b))
            return
        t = self.num(b["type"], "type")
        if t is None:
            raise CaseError("type: can't be inferred")
        tw = Writer()
        self.put_num(tw, b, "type", leb128)
        kind, info = classify(t) if isinstance(t, int) else ("given", {})

        data = Writer()
        content = {}
        self.field(data, b["data"], lambda d, v: self.block_data(d, v, t, kind, info, b, start, content))

        w.append(tw)
        width = [1, 2, 4, 8][t % 4] if isinstance(t, int) else None
        if width is None and is_infer(b.get("size", UNDEFINED)):
            raise CaseError("size: can't be inferred of a type given as bytes")
        self.put_num(w, b, "size", lambda n: ule(n, width), len(data))
        w.append(data)
        odd = (len(w) - start) % 2 == 1
        if odd or "padding" in b:
            self.put_num(w, b, "padding", lambda n: ule(n, 1), 0)
        if kind == "metadata":
            self.last_meta = start
            self.prev_meta = start
            self.segment = []
        elif kind in ("data", "dictionary", "module", "resource", "formula", "import"):
            self.segment.append((start, kind, info, content))

    def block_data(self, w, v, t, kind, info, b, start, content):
        if is_raw(v) or (isinstance(v, Tag) and v.tag == EXT):
            w.put(self.bytes_of(v, "data"))
            content["unknown"] = True
            return
        if kind == "spacer":
            sp = v.get("space", UNDEFINED)
            if is_infer(sp):
                size = self.num(b.get("size", UNDEFINED), "size")
                if not isinstance(size, int):
                    raise CaseError("space: inferred only of a size given")
                w.put(bytes(size))
            else:
                self.field(w, sp, lambda w, x: w.put(self.bytes_of(x, "space")))
        elif kind == "size_hint":
            end = self.prev_info.get("archive_end")
            if end is None:
                self.unresolved = True
                end = self.archive_start
            self.put_num(w, v, "size_hint", leb128, max(end - self.archive_start, 1))
        elif kind == "metadata":
            self.metadata(w, v, info, start)
        elif kind == "data":
            if info["dictionary"]:
                content["dictionary"] = self.put_num(w, v, "dictionary_index", leb128)
            if info["compressed"]:
                d = self.field(w, v["compressed_data"], lambda w, x: self.compressed(w, x, self.plain, True))
            else:
                d = self.field(
                    w, v["uncompressed_data"], lambda w, x: self.uncompressed(w, x, self.plain, info["checksum"])
                )
            content["bytes"] = d
        else:
            raise CaseError(f"block type {t:#x}: give its data as bytes (not assembled yet)")

    # The decoded data of an external structure is a byte sequence of its own.

    def plain(self, w, v):
        w.put(self.bytes_of(v, "data"))

    def decoded(self, d, enc):
        w = Writer()
        self.field(w, d, enc)
        self.commit(w)
        return bytes(w.buf)

    def uncompressed(self, w, v, enc, checksum):
        d = self.decoded(v["data"], enc)
        w.put(d)
        if checksum:
            self.put_checksum(w, v, d)
        return d

    def put_checksum(self, w, v, d):
        def go(w, x):
            if is_infer(x):
                if d is None:
                    raise CaseError("checksum: the decoded data is not known")
                w.put(ule(xxh64(d), 8))
            else:
                w.put(self.bytes_of(x, "checksum"))

        self.field(w, v.get("checksum", UNDEFINED), go)

    def compressed(self, w, v, enc, checksum):
        params = Writer()
        wd = None

        def go_params(w, p):
            nonlocal wd
            if is_raw(p):
                w.put(raw_bytes(p))
            else:
                wd = self.put_num(w, p, "window_descriptor", lambda n: ule(n, 1))

        self.field(params, v["compression_params"], go_params)
        stored = Writer()
        d = self.field(stored, v["data"], lambda w, x: self.zstd(w, x, enc, wd))

        cs = len(stored)
        os_ = None if d is None else len(d)
        head = Writer()
        delta = None if os_ is None else (os_ - cs if os_ > cs else 0)
        got = self.put_num(head, v, "original_size_delta", leb128, delta)
        if got == 0 or (got is None and "original_size_shrink" in v):
            self.put_num(head, v, "original_size_shrink", leb128, None if os_ is None else cs - os_)
        w.append(head)
        w.append(params)
        w.append(stored)
        if checksum:
            self.put_checksum(w, v, d)
        return d

    def zstd(self, w, v, enc, wd):
        """Writes Zstandard blocks, returning their decoded data (None if not known)."""
        if is_raw(v):
            w.put(raw_bytes(v))
            return None
        if isinstance(v, Map):
            if wd is None:
                raise CaseError("decoded: the window descriptor is not known")
            d = self.decoded(v["decoded"], enc)
            exp, man = wd >> 3, wd & 7
            base = 1 << (10 + exp)
            bmax = min(base + (base // 8) * man, 128 * 1024)
            pieces = [d[i : i + bmax] for i in range(0, len(d), bmax)] or [b""]
            for i, p in enumerate(pieces):
                last = i == len(pieces) - 1
                w.put((len(p) << 3 | 0 << 1 | last).to_bytes(3, "little"))
                w.put(p)
            return d
        out = bytearray()
        known = True
        for i, blk in enumerate(v):
            blk = self.unlabel(blk)
            if is_raw(blk):
                w.put(raw_bytes(blk))
                known = False
                continue
            bt = ZSTD_BLOCK_TYPES[blk["block_type"]]
            last = blk.get("last", UNDEFINED)
            last = (i == len(v) - 1) if is_infer(last) else bool(last)
            c = self.bytes_of(blk["content"], "content")
            size = self.num(blk.get("block_size", UNDEFINED), "block_size")
            if size is None:
                if bt == 1:
                    raise CaseError("block_size of an RLE_Block: give the count")
                size = len(c)
            w.put((size << 3 | bt << 1 | last).to_bytes(3, "little"))
            w.put(c)
            if bt == 0:
                out += c
            elif bt == 1:
                out += c * size
            else:
                known = False
        return bytes(out) if known else None

    # --- Metadata block

    def metadata(self, w, v, info, start):
        enc = lambda w, m: self.meta(w, m, start)
        if info["compressed"]:
            self.field(w, v["compressed_data"], lambda w, x: self.compressed(w, x, enc, True))
        else:
            self.field(w, v["uncompressed_data"], lambda w, x: self.uncompressed(w, x, enc, True))

    def meta(self, w, m, start):
        if is_raw(m):
            w.put(raw_bytes(m))
            return
        segment = self.segment
        prev = self.prev_meta
        inferred_prev = start - (prev if prev is not None else self.header_end) + (1 if prev is None else 0)

        entries = m["entries"]
        kinds = []
        if isinstance(entries, list):
            for e in entries:
                e = self.unlabel(e)
                if is_raw(e):
                    kinds.append(None)
                elif "original_size" in e:
                    kinds.append("file")
                elif "special_kind" in e:
                    kinds.append("special")
                else:
                    kinds.append("directory")

        def count(kind):
            if not isinstance(entries, list) or None in kinds:
                return None
            return kinds.count(kind)

        def offsets_of(kind):
            return [start - s for s, k, _, _ in segment if k == kind]

        def known(v, what):
            if not isinstance(v, int):
                raise CaseError(f"{what}: its value is needed but not known")
            return v

        head = Writer()
        P = known(self.put_num(head, m, "previous_metadata_offset", leb128, inferred_prev), "previous_metadata_offset")
        counts = {}
        for key, kind in (("num_modules", "module"), ("num_resources", "resource"), ("num_dictionaries", "dictionary")):
            lst = self.unlabel(m.get(kind + "_offsets", UNDEFINED))
            n = len(lst) if isinstance(lst, list) else len(offsets_of(kind))
            counts[key] = self.put_num(head, m, key, leb128, n)
        for key, kind in (("num_directories", "directory"), ("num_files", "file"), ("num_specials", "special")):
            counts[key] = self.put_num(head, m, key, leb128, count(kind))
        d = known(counts["num_directories"], "num_directories")
        nd = counts["num_dictionaries"]

        # entries first, for their size and offsets
        body = Writer()
        file_params = self.unlabel(m["file_params"])
        fps = []

        def file_offset_of(k):
            fp = self.unlabel(file_params[k - d - 1])
            return self.num(fp["file_offset"], "file_offset")

        def block_at(off):
            pos = start - (off & ~1)
            for s, k, i, c in segment:
                if s == pos:
                    return k, i, c
            if self.unresolved:
                return "data", {}, {"bytes": b""}  # labels not known yet: assembled again
            raise CaseError(f"no content block of the segment at the relative offset {off}")

        entry_starts = []
        ew = Writer()

        def put_entries(w, es):
            if is_raw(es):
                w.put(raw_bytes(es))
                return
            for idx, e in enumerate(es):
                k = idx + 1
                entry_starts.append(len(w))
                self.field(w, e, lambda w, e: self.entry(w, e, k, kinds[idx], d, file_offset_of, block_at))

        self.field(ew, entries, put_entries)
        entries_size = self.put_num(head, m, "entries_size", leb128, len(ew))

        om = m.get("optional_metadata", None)
        omw = Writer()
        if om is not None:
            self.field(omw, om, lambda w, x: w.put(raw_bytes(x) if isinstance(x, Tag) and x.tag == RAW and isinstance(x.value, bytes) else cbor(x, self.cbor_hook)))
        self.put_num(head, m, "optional_metadata_size", leb128, len(omw))
        w.append(head)

        def put_list(key, width, inferred):
            def go(w, lst):
                if is_infer(lst):
                    if inferred is None:
                        raise CaseError(f"{key}: can't be inferred")
                    lst = inferred
                if is_raw(lst):
                    w.put(raw_bytes(lst))
                    return
                for x in lst:
                    self.put_num(w, x, key, lambda n: ule(n, width))

            self.field(w, m.get(key, UNDEFINED), go)

        put_list("module_offsets", u_width(P), offsets_of("module"))
        put_list("resource_offsets", u_width(P), offsets_of("resource"))
        put_list("dictionary_offsets", u_width(P), offsets_of("dictionary"))
        put_list("entry_parents", u_width(d), None)

        def put_fps(w, fps):
            if is_raw(fps):
                w.put(raw_bytes(fps))
                return
            for fp in fps:
                self.field(w, fp, lambda w, fp: self.file_param(w, fp, P, nd, block_at))

        self.field(w, m["file_params"], put_fps)
        if not isinstance(entries_size, int):
            raise CaseError("entries_size: its value is needed but not known")
        put_list("entry_offsets", u_width(entries_size), entry_starts[1:] if not is_raw(entries) else None)
        w.append(ew)
        w.append(omw)

    def file_param(self, w, fp, P, nd, block_at):
        if is_raw(fp):
            w.put(raw_bytes(fp))
            return
        fo = self.num(fp["file_offset"], "file_offset")

        def dict_of():
            if not isinstance(fo, int):
                raise CaseError("file_dictionary: the file_offset is not known")
            k, _, c = block_at(fo)
            di = c.get("dictionary")
            return 0 if di is None else di + 1

        if not isinstance(nd, int):
            raise CaseError("file_dictionary: num_dictionaries is not known")
        self.put_num(w, fp, "file_dictionary", lambda n: ule(n, u_width(nd)), dict_of)
        self.put_num(w, fp, "file_offset", lambda n: ule(n, u_width(P)))
        if "file_output" in fp or (isinstance(fo, int) and fo % 2 == 1):
            self.put_num(w, fp, "file_output", leb128)

    def entry(self, w, e, k, kind, d, file_offset_of, block_at):
        if is_raw(e):
            w.put(raw_bytes(e))
            return
        self.field(w, e["entry_name"], lambda w, x: w.put(text_field(x)))
        self.put_num(w, e, "entry_name_sentinel", lambda n: ule(n, 1), 0)
        if kind == "file":

            def size():
                fo = file_offset_of(k)
                if not isinstance(fo, int):
                    raise CaseError("original_size: the file_offset is not known")
                _, _, c = block_at(fo)
                if fo % 2 or c.get("bytes") is None:
                    raise CaseError("original_size: the content is not known without evaluating")
                return len(c["bytes"])

            self.put_num(w, e, "original_size", leb128, size)
        elif kind == "special":
            sk = self.put_num(w, e, "special_kind", leb128)
            if sk == 1:
                self.field(w, e["link_target"], lambda w, x: w.put(text_field(x)))
                self.put_num(w, e, "link_target_sentinel", lambda n: ule(n, 1), 0)
            elif sk in (3, 4):
                self.put_num(w, e, "device_major", leb128)
                self.put_num(w, e, "device_minor", leb128)
            elif isinstance(sk, int) and sk > 4 and "special_data" in e:
                self.field(w, e["special_data"], lambda w, x: w.put(self.bytes_of(x, "special_data")))

    # Inside a CBOR item only the tags 80000 to 80003 mean something.

    def cbor_hook(self, v):
        if isinstance(v, Tag) and v.tag in (LABEL_START, LABEL_END):
            raise CaseError("labels inside a CBOR item are not supported")
        if isinstance(v, Tag) and v.tag == RAW:
            if isinstance(v.value, bytes):
                return v.value
            if is_diff(v):
                return cbor(self.diff(v))
            raise CaseError(f"{v!r} inside a CBOR item")
        if isinstance(v, Tag) and v.tag == EXT:
            return cbor(self.bytes_of(v, "file"))
        return None


def text_field(v):
    if isinstance(v, str):
        return v.encode("utf-8", "surrogatepass")
    if isinstance(v, Tag) and v.tag == 273 and isinstance(v.value, bytes):
        return v.value
    if isinstance(v, bytes):
        return b"\xc1" + v
    if isinstance(v, Tag) and v.tag == RAW and isinstance(v.value, bytes):
        return v.value
    raise CaseError(f"not a text: {v!r}")


def classify(t):
    """The kind of a block type, and what its bits tell."""
    if t <= 3:
        return "spacer", {}
    if t == 4:
        return "size_hint", {}
    if 0x10 <= t <= 0x17:
        return "metadata", {"compressed": bool(t & 4)}
    if 0x20 <= t <= 0x37:
        kk = (t >> 3) & 3
        return "data", {"leaf": True, "compressed": kk != 0, "dictionary": kk == 2, "checksum": bool(t & 4)}
    if 0x40 <= t <= 0x4B:
        kk = (t >> 2) & 3
        return "data", {"leaf": False, "compressed": kk != 0, "dictionary": kk == 2, "checksum": True}
    if 0x50 <= t <= 0x57:
        return "dictionary", {}
    if 0x58 <= t <= 0x5F:
        return "import", {}
    if 0x60 <= t <= 0x63:
        return "module", {}
    if 0x64 <= t <= 0x67:
        return "resource", {}
    if 0x70 <= t <= 0x77:
        return "formula", {}
    return "other", {}


def assemble(path):
    with open(path, encoding="utf-8") as f:
        case = loads(f.read())
    return case, Assembler(os.path.dirname(os.path.abspath(path))).run(case)


def main(argv):
    import argparse

    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("case")
    p.add_argument("-o", "--output")
    a = p.parse_args(argv)
    _, out = assemble(a.case)
    dst = a.output or os.path.splitext(a.case)[0] + ".aqr"
    with open(dst, "wb") as f:
        f.write(out)
    print(f"{dst}: {len(out)} bytes")


if __name__ == "__main__":
    main(sys.argv[1:])
