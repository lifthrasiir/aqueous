"""Assembles the bytes of a case's `archive` (conformance/case.cddl).

    python assemble.py CASE.edn [-o OUT]

Writes `leading`, the archive and `trailing` to OUT (CASE with `.aqr`, by default).

A prototype: unencrypted archives of spacer, size hint, metadata, data, dictionary, import,
module (the stored form), resource, formula and mirror blocks, and any block given as its bytes. Labels are resolved by assembling again until they stay where they were.
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
    "MIRROR_MAGIC": bytes.fromhex("02AAF5FEBA2BAE23"),
    "OVERLAY_MIRROR_MAGIC": bytes.fromhex("010534FABD11AF53"),
    "RECOVERY_MAGIC": bytes.fromhex("690AD179CD157AB6"),
    "ENCRYPTED_HEADER_MAGIC": bytes.fromhex("F9CAF2A6D02A0100"),
    "ENCRYPTED_MIRRORED_HEADER_MAGIC": bytes.fromhex("F9CAF2A6D02A0200"),
    "ENCRYPTED_MIRROR_MAGIC": bytes.fromhex("78D73F690AFF4483"),
    "OVERLAY_RECOVERY_MAGIC": bytes.fromhex("45E73455EAF2E10D"),
}

OPCODES = {
    "Nop": 0x00, "Literal": 0x01, "ModuleBytes": 0x02, "ResourceBytes": 0x03, "Swap": 0x0F, "Slice": 0x10,
    "Concatenate": 0x12, "Pack": 0x13, "Split": 0x14, "Patch": 0x15, "Apply": 0x16, "Checksum": 0x20,
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
            self.info["blocks"] = {}
            self.info["items"] = []
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
            self.encrypted(w, a)
            return
        self.magic(w, a, "header_magic")
        self.archive_start = len(w)
        self.segment = []
        self.seg_no = 0
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

    def encrypted(self, w, a):
        import age

        def known_bytes(key, n=None):
            v = self.unlabel(a.get(key, UNDEFINED))
            if is_infer(v):
                return None
            b = self.bytes_of(v, key)
            if n is not None and len(b) != n:
                raise CaseError(f"{key}: {len(b)} bytes, not {n}")
            return b

        def passphrase():
            v = self.unlabel(a.get("key_passphrase", UNDEFINED))
            if isinstance(v, str):
                return v.encode("utf-8")
            if isinstance(v, bytes):
                return v
            raise CaseError("key_passphrase: give it to infer encrypted_file_key")

        start = len(w)
        self.magic(w, a, "encrypted_header_magic")
        mirrored = bytes(w.buf[start:]) == CONSTANTS["ENCRYPTED_MIRRORED_HEADER_MAGIC"]
        lg = self.put_num(w, a, "lg_work_factor", lambda n: ule(n, 1))
        self.field(w, a["scrypt_ephemeral_secret"], lambda w, x: w.put(self.bytes_of(x, "scrypt_ephemeral_secret")))
        salt = known_bytes("scrypt_ephemeral_secret")
        file_key = known_bytes("file_key", 16)
        wrapped = []

        def go_key(w, x):
            if is_infer(x):
                if file_key is None or not isinstance(lg, int):
                    raise CaseError("encrypted_file_key: give file_key (and lg_work_factor) to infer it")
                wrapped.append(age.wrap_key(file_key, passphrase(), salt, lg))
            else:
                wrapped.append(self.bytes_of(x, "encrypted_file_key"))
            w.put(wrapped[0])

        self.field(w, a.get("encrypted_file_key", UNDEFINED), go_key)

        def go_mac(w, x):
            if is_infer(x):
                if file_key is None:
                    raise CaseError("age_header_mac: give file_key to infer it")
                w.put(age.header_mac(file_key, salt, lg, wrapped[0]))
            else:
                w.put(self.bytes_of(x, "age_header_mac"))

        self.field(w, a.get("age_header_mac", UNDEFINED), go_mac)
        self.field(w, a["payload_nonce"], lambda w, x: w.put(self.bytes_of(x, "payload_nonce")))
        nonce = known_bytes("payload_nonce")
        fields = bytes(w.buf[start + 8 :])
        if mirrored or "header_checksum" in a:
            head = bytes(w.buf[start:])

            def go_sum(w, x):
                w.put(ule(xxh64(head), 8) if is_infer(x) else self.bytes_of(x, "header_checksum"))

            self.field(w, a.get("header_checksum", UNDEFINED), go_sum)

        def go_chunks(w, x):
            if not is_infer(x):
                w.put(self.bytes_of(x, "chunks"))
                return
            if file_key is None or "payload" not in a:
                raise CaseError("chunks: give file_key and payload to infer them")
            pw = Writer()
            self.field(pw, a["payload"], self.payload)
            self.commit(pw)
            k = 0
            for m, c in enumerate(age.encrypt_payload(file_key, nonce, bytes(pw.buf))):
                if mirrored and (m + 1 in (2, 4, 8, 16, 32) or (m + 1) % 64 == 0):
                    k += 1
                    h = CONSTANTS["ENCRYPTED_MIRROR_MAGIC"] + ule(k, 4) + fields
                    w.put(h + ule(xxh64(h), 8))
                w.put(c)

        self.field(w, a.get("chunks", UNDEFINED), go_chunks)

    def payload(self, w, v):
        """The plaintext of an encrypted archive: an unencrypted archive, bytes, or a list of them."""
        for x in v if isinstance(v, list) else [v]:
            if isinstance(x, Map):
                self.field(w, x, self.archive)
            else:
                w.put(self.bytes_of(x, "payload"))

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
        width = [1, 2, 4, 8][t % 4] if isinstance(t, int) else None
        given_size = self.num(b.get("size", UNDEFINED), "size")

        def header(n):
            """The bytes of the type and size fields of `n` bytes of data, as they are written."""
            n = given_size if isinstance(given_size, int) else n
            return bytes(tw.buf) + ule(n, width)

        data = Writer()
        content = {}
        self.field(
            data, b["data"], lambda d, v: self.block_data(d, v, t, kind, info, b, start, content, header)
        )

        w.append(tw)
        if width is None and is_infer(b.get("size", UNDEFINED)):
            raise CaseError("size: can't be inferred of a type given as bytes")
        self.put_num(w, b, "size", lambda n: ule(n, width), len(data))
        w.append(data)
        odd = (len(w) - start) % 2 == 1
        if odd or "padding" in b:
            self.put_num(w, b, "padding", lambda n: ule(n, 1), 0)
        self.info["blocks"][start] = (t if isinstance(t, int) else None, bytes(data.buf))
        content["stored"] = bytes(data.buf)
        if kind == "metadata":
            self.last_meta = start
            self.prev_meta = start
            self.segment = []
            self.seg_no += 1
        elif kind == "shorthand":
            self.segment.append((start, "dictionary" if t == 0x50 else "module", info, content))
        elif kind in ("data", "dictionary", "module", "resource", "formula", "import"):
            self.segment.append((start, kind, info, content))
            if kind == "data" and not info["leaf"]:
                d = content.get("bytes")
                self.info["items"].append((self.seg_no, start, None if d is None else len(d), d))
            elif kind == "import":
                for size in content.get("imports", []):
                    self.info["items"].append((self.seg_no, start, size, None))

    def block_data(self, w, v, t, kind, info, b, start, content, header):
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
                d = self.field(
                    w, v["compressed_data"], lambda w, x: self.compressed(w, x, self.plain, info["checksum"])
                )
            else:
                d = self.field(
                    w, v["uncompressed_data"], lambda w, x: self.uncompressed(w, x, self.plain, info["checksum"])
                )
            content["bytes"] = d
        elif kind == "dictionary":
            content["bytes"] = self.field(
                w, v["compressed_data"], lambda w, x: self.compressed(w, x, self.dictionary, True)
            )
        elif kind in ("module", "resource"):
            content["bytes"] = self.field(w, v["compressed_data"], lambda w, x: self.compressed(w, x, self.plain, True))
        elif kind == "import":
            sizes = []
            self.field(w, v["compressed_data"], lambda w, x: self.compressed(w, x, lambda w, m: self.imports(w, m, sizes), True))
            content["imports"] = sizes
        elif kind == "formula":
            self.formula(w, v, info, start, content)
        elif kind == "mirror":
            self.mirror(w, v, t, start, header)
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
            if "decoded" in blk:
                out += self.bytes_of(blk["decoded"], "decoded")
            elif bt == 0:
                out += c
            elif bt == 1:
                out += c * size
            else:
                known = False
        return bytes(out) if known else None

    # --- Dictionary, import, formula and mirror blocks

    def dictionary(self, w, v):
        if is_raw(v):
            w.put(raw_bytes(v))
            return
        if "entropy_tables" in v:
            self.field(w, v["entropy_tables"], lambda w, x: w.put(self.bytes_of(x, "entropy_tables")))
        if "repeat_offsets" in v:

            def go(w, lst):
                if is_raw(lst):
                    w.put(raw_bytes(lst))
                    return
                for x in lst:
                    self.put_num(w, x, "repeat_offsets", lambda n: ule(n, 4))

            self.field(w, v["repeat_offsets"], go)
        self.field(w, v["content"], lambda w, x: w.put(self.bytes_of(x, "content")))

    def imports(self, w, m, sizes):
        if is_raw(m):
            w.put(raw_bytes(m))
            return
        lst = self.unlabel(m["imports"])
        self.put_num(w, m, "num_imports", leb128, len(lst) if isinstance(lst, list) else None)

        def go(w, lst):
            if is_raw(lst):
                w.put(raw_bytes(lst))
                return
            for imp in lst:
                self.field(w, imp, lambda w, i: self.import_(w, i, sizes))

        self.field(w, m["imports"], go)

    def import_(self, w, i, sizes):
        if is_raw(i):
            w.put(raw_bytes(i))
            sizes.append(None)
            return
        self.field(w, i["import_path"], lambda w, x: w.put(text_field(x)))
        self.put_num(w, i, "import_path_sentinel", lambda n: ule(n, 1), 0)
        self.put_num(w, i, "import_offset", leb128)
        sizes.append(self.put_num(w, i, "import_size", leb128))

        def go(w, x):
            if is_infer(x):
                raise CaseError("import_checksum: give it (it is not inferred from the preceding result yet)")
            w.put(self.bytes_of(x, "import_checksum"))

        self.field(w, i.get("import_checksum", UNDEFINED), go)

    def formula(self, w, v, info, start, content):
        if info["dictionary"]:
            content["dictionary"] = self.put_num(w, v, "dictionary_index", leb128)
        # the items following the formula block in its segment, as the previous pass found them
        if "items" in self.prev_info:
            items = [(n, d) for seg, s, n, d in self.prev_info["items"] if seg == self.seg_no and s > start]
        else:
            self.unresolved = True
            items = []
        state = {}
        fd = Writer()
        enc = lambda w, f: self.formula_data(w, f, items, state)
        self.field(fd, v["formula_data"], lambda w, x: self.compressed(w, x, enc, True))
        stack = state.get("stack")
        self.put_num(w, v, "num_outputs", leb128, None if stack is None else len(stack))
        w.append(fd)
        content["outputs"] = stack

    def formula_data(self, w, f, items, state):
        if is_raw(f):
            w.put(raw_bytes(f))
            return
        lists = {}
        for count, key in (
            ("num_blocks", "block_ordinal_deltas"),
            ("num_resources", "resource_indices"),
            ("num_modules", "module_indices"),
        ):
            lists[key] = self.put_list_field(w, f, count, key)

        # the referenced items as (length, bytes), what is not known None
        refs = []
        o = -1
        for delta in lists["block_ordinal_deltas"] or []:
            o = o + 1 + delta if isinstance(delta, int) and o is not None else None
            refs.append(items[o] if o is not None and 0 <= o < len(items) else (None, None))
        auxiliary = {k: [c for _, kind, _, c in self.segment if kind == k] for k in ("module", "resource")}

        def aux(kind, i):
            idx = lists[kind + "_indices"]
            if idx is None or not isinstance(i, int) or i >= len(idx) or not isinstance(idx[i], int):
                return None
            blocks = auxiliary[kind]
            return blocks[idx[i]].get("stored") if idx[i] < len(blocks) else None

        stack = [[]]

        def go(w, insns):
            if is_raw(insns):
                w.put(raw_bytes(insns))
                stack[0] = None
                return
            for insn in insns:
                self.field(w, insn, lambda w, i: self.instruction(w, i, stack, refs, aux))

        self.field(w, f["instructions"], go)
        state["stack"] = stack[0]

    def instruction(self, w, i, stack_ref, refs, aux):
        """Writes an instruction, running it on the stack of (length, bytes) as far as they are known."""
        stack = stack_ref[0]
        if is_raw(i):
            w.put(raw_bytes(i))
            stack_ref[0] = None
            return
        op = i["opcode"]
        if op not in OPCODES:
            raise CaseError(f"unknown opcode {op!r}")
        w.put(bytes([OPCODES[op]]))

        def pop(n):
            if stack is None or not isinstance(n, int) or len(stack) < n:
                stack_ref[0] = None
                return None
            out = stack[len(stack) - n :]
            del stack[len(stack) - n :]
            return out

        def push(*xs):
            if stack_ref[0] is not None:
                stack_ref[0].extend(xs)

        def put(key, inferred=None):
            return self.put_num(w, i, key, leb128, inferred)

        def known_bytes(xs):
            return xs is not None and all(d is not None for _, d in xs)

        if op == "Nop":
            pass
        elif op == "Literal":
            d = self.bytes_of(i["literal_data"], "literal_data")
            put("literal_size", len(d))
            self.field(w, i["literal_data"], lambda w, x: w.put(d))
            push((len(d), d))
        elif op in ("ModuleBytes", "ResourceBytes"):
            kind = "module" if op == "ModuleBytes" else "resource"
            idx = put(f"{kind}_bytes_{kind}")
            d = aux(kind, idx)
            n = put(f"{kind}_bytes_size", None if d is None else len(d))
            push((n, d if d is not None and len(d) == n else None))
        elif op == "Swap":
            depth = put("swap_depth")
            if stack is not None and isinstance(depth, int) and len(stack) > depth:
                stack[-1], stack[-1 - depth] = stack[-1 - depth], stack[-1]
            else:
                stack_ref[0] = None
        elif op == "Slice":
            blk, st, n = put("slice_block"), put("slice_start"), put("slice_length")
            d = None
            if isinstance(blk, int) and blk < len(refs) and refs[blk][1] is not None and isinstance(st, int):
                d = refs[blk][1][st : st + n]
                d = d if len(d) == n else None
            push((n, d))
        elif op in ("Concatenate", "Pack"):
            n = put("concat_count" if op == "Concatenate" else "pack_count")
            xs = pop(n)
            if xs is None:
                return
            if any(k is None for k, _ in xs):
                push((None, None))
            elif op == "Concatenate":
                push((sum(k for k, _ in xs), b"".join(d for _, d in xs) if known_bytes(xs) else None))
            else:
                d = None
                if known_bytes(xs):
                    ends, e = [], 0
                    for _, x in xs:
                        e += len(x)
                        ends.append(e)
                    d = ule(len(xs), 8) + b"".join(ule(e, 8) for e in ends) + b"".join(x for _, x in xs)
                push((8 * (len(xs) + 1) + sum(k for k, _ in xs), d))
        elif op == "Split":
            sizes = self.put_list_field(w, i, "split_count", "split_sizes")
            xs = pop(1)
            if xs is None or sizes is None:
                stack_ref[0] = None
                return
            d, at = xs[0][1], 0
            for n in sizes:
                push((n, None if d is None or not isinstance(n, int) else d[at : at + n]))
                at += n if isinstance(n, int) else 0
        elif op == "Patch":
            xs = pop(2)
            out = unzstd_simple(xs[1][1], xs[0][1]) if known_bytes(xs) else None
            n = put("patch_size", None if out is None else len(out))
            push((n, out if out is not None and len(out) == n else None))
        elif op == "Apply":
            put("apply_module")
            self.put_list_field(w, i, "apply_num_resources", "apply_resources")
            params = self.bytes_of(i["apply_params"], "apply_params")
            put("apply_params_size", len(params))
            self.field(w, i["apply_params"], lambda w, x: w.put(params))
            put("apply_memory_limit")
            self.put_list_field(w, i, "apply_num_task_limits", "apply_task_limits")
            n_in = put("apply_num_inputs")
            outs = self.put_list_field(w, i, "apply_num_outputs", "apply_output_sizes")
            if pop(n_in) is None or outs is None:
                stack_ref[0] = None
                return
            push(*[(n, None) for n in outs])
        elif op == "Checksum":
            top = stack[-1] if stack else None

            def go(w, x):
                if is_infer(x):
                    if top is None or top[1] is None:
                        raise CaseError("checksum_value: the top of the stack is not known")
                    w.put(ule(xxh64(top[1]), 8))
                else:
                    w.put(self.bytes_of(x, "checksum_value"))

            self.field(w, i.get("checksum_value", UNDEFINED), go)

    def put_list_field(self, w, m, count, key):
        """Writes a count and its list of leb128, returning the numbers (None where given as bytes)."""
        lst = self.unlabel(m.get(key, []))
        self.put_num(w, m, count, leb128, len(lst) if isinstance(lst, list) else None)
        got = []

        def go(w, lst):
            nonlocal got
            if is_raw(lst):
                w.put(raw_bytes(lst))
                got = None
                return
            for x in lst:
                got.append(self.put_num(w, x, key, leb128))

        self.field(w, m.get(key, []), go)
        return got

    def mirror(self, w, v, t, start, header):
        self.magic(w, v, "mirror_magic")
        to = self.num(v["target_offset"], "target_offset")
        target = None
        if isinstance(to, int):
            pos = start - to if to % 2 == 0 else start + to - 1
            target = self.prev_info.get("blocks", {}).get(pos)
            if target is None:
                self.unresolved = True

        def widened():
            if target is None:
                return 0
            if t - 0xC00 == target[0]:
                return 0
            if t - 0xC01 == target[0]:
                return 1
            raise CaseError(f"archive_offset: a mirror block of type {t:#x} doesn't fit its target of {target[0]:#x}")

        self.put_num(w, v, "archive_offset", leb128, lambda: start - self.archive_start + widened())
        self.put_num(w, v, "target_offset", leb128)

        def go(w, x):
            if is_infer(x):
                w.put(b"" if target is None else target[1])
            else:
                w.put(self.bytes_of(x, "target_data"))

        self.field(w, v.get("target_data", UNDEFINED), go)

        def go_sum(w, x):
            if is_infer(x):
                w.put(ule(xxh64(header(len(w) + 8) + bytes(w.buf)), 8))
            else:
                w.put(self.bytes_of(x, "mirror_checksum"))

        self.field(w, v.get("mirror_checksum", UNDEFINED), go_sum)

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

        # lists that disagree with counts inferred are mistakes of the case, not what it tests
        inferred = lambda key: is_infer(m.get(key, UNDEFINED))
        if all(inferred(k) for k in ("num_directories", "num_files", "num_specials")) and isinstance(entries, list):
            n = len(entries)
            parents = self.unlabel(m["entry_parents"])
            if isinstance(parents, list) and len(parents) != max(n - 1, 0):
                raise CaseError(f"entry_parents: {len(parents)} for {n} entries (the entry 1 has none)")
            fps = self.unlabel(m["file_params"])
            if isinstance(fps, list) and len(fps) != kinds.count("file"):
                raise CaseError(f"file_params: {len(fps)} for {kinds.count('file')} files")

        # entries first, for their size and offsets
        body = Writer()
        file_params = self.unlabel(m["file_params"])
        fps = []

        def file_offset_of(k):
            fp = self.unlabel(file_params[k - d - 1])
            return self.num(fp["file_offset"], "file_offset")

        def file_output_of(k):
            fp = self.unlabel(file_params[k - d - 1])
            n = self.num(fp.get("file_output", UNDEFINED), "file_output")
            if not isinstance(n, int):
                raise CaseError("file_output: give it")
            return n

        def block_at(off):
            pos = start - (off & ~1)
            for s, k, i, c in segment:
                if s == pos:
                    return k, i, c
            if self.unresolved or not self.prev:
                self.unresolved = True
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
                self.field(w, e, lambda w, e: self.entry(w, e, k, kinds[idx], d, file_offset_of, file_output_of, block_at))

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

    def entry(self, w, e, k, kind, d, file_offset_of, file_output_of, block_at):
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
                kind, _, c = block_at(fo)
                if kind == "formula":
                    outs = c.get("outputs")
                    o = 0 if fo % 2 == 0 else file_output_of(k) + 1
                    if outs is None or o >= len(outs) or outs[o][0] is None:
                        raise CaseError("original_size: the output's length is not known")
                    return outs[o][0]
                if fo % 2 or c.get("bytes") is None:
                    raise CaseError("original_size: the content is not known")
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


def unzstd_simple(patch, source):
    """What Zstandard blocks decode to, if they are all raw and RLE blocks (None otherwise)."""
    out, at = bytearray(), 0
    while at + 3 <= len(patch):
        h = int.from_bytes(patch[at : at + 3], "little")
        last, bt, size = h & 1, (h >> 1) & 3, h >> 3
        at += 3
        if bt == 0:
            out += patch[at : at + size]
            at += size
        elif bt == 1:
            out += patch[at : at + 1] * size
            at += 1
        else:
            return None
        if last:
            return bytes(out) if at == len(patch) else None
    return None


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
    if t in (0x50, 0x60):
        return "shorthand", {}
    if 0x51 <= t <= 0x57:
        return "dictionary", {}
    if 0x58 <= t <= 0x5F:
        return "import", {}
    if 0x61 <= t <= 0x63:
        return "module", {}
    if 0x64 <= t <= 0x67:
        return "resource", {}
    if 0x70 <= t <= 0x77:
        return "formula", {"dictionary": bool(t & 4)}
    if 0xC10 <= t <= 0xC17 or 0xC50 <= t <= 0xC67 or 0xC70 <= t <= 0xC77:
        return "mirror", {}
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
