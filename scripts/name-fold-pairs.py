#!/usr/bin/env python3
"""Pairs of names that some file system may take as one name, for the test `name_fold_pairs` of
aqr (crates/aqr/src/fold.rs), which checks that aqr's fold takes each pair as one too.

    name-fold-pairs.py ucd OUT.json FILE...
        The pairs of the Unicode Character Database, of any version (each FILE a UnicodeData
        or a CaseFolding file, by its name): every character and its simple uppercase,
        lowercase and titlecase mappings, its case foldings (C, S and F; T, Turkic, left out)
        and its canonical decomposition (of the file named UnicodeData.txt alone, the current
        version). Case mappings that later versions dropped (Georgian's, say) are what old
        file systems' tables may still have.

    name-fold-pairs.py probe DIR OUT.json
        The pairs that the file system of DIR (an empty directory) takes as one: probe names
        made in DIR, and each that is there already (`O_EXCL`) paired with the name kept there
        (`F_GETPATH` on macOS, `realpath` elsewhere). Names are `p{s}q` for every character s and
        what it maps to (as above, by Python's own data), and `pq` (for characters ignored).
        `--names PAIRS.json` probes the names of those pairs instead, each pair in a
        directory of its own. `--bucket N` makes them in subdirectories of N names, a
        character's names in one (FAT's directories hold about 32 000 names of this size).
        `--cased UCD.json` probes only characters that have case (all but categories Lo, Cn,
        Co and Cs, and those of any case mapping of the `ucd` pairs given): few enough for one
        directory of FAT, and for exFAT, whose directories are slow as they grow on macOS.

        Disk images to probe on macOS: `hdiutil create -size 8g -type SPARSE -fs FS ...` of
        APFS, HFS+, ExFAT or "MS-DOS FAT32"; exFAT and FAT given small clusters
        (`newfs_exfat -R -b 4096`, `newfs_msdos -F 32 -c 1`), as each file takes one.

OUT.json is a list of [name, name, what] (what: the mapping, or the file system).
"""

import argparse
import json
import os
import sys
import unicodedata


def ucd_pairs(files):
    pairs = []
    for path in files:
        name = os.path.basename(path)
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = [line.split("#")[0].strip() for line in f]
        lines = [line for line in lines if line]
        if name.startswith("CaseFolding"):
            for line in lines:
                code, status, mapping = [x.strip() for x in line.split(";")[:3]]
                if status in "CSF":
                    pairs.append([chr(int(code, 16)), "".join(chr(int(x, 16)) for x in mapping.split()), f"{name} {status}"])
            continue
        for r in (line.split(";") for line in lines):
            # (ranges, and rows not of a character: their characters map to nothing)
            if len(r) < 15 or r[1].endswith(", First>") or r[1].endswith(", Last>"):
                continue
            c = chr(int(r[0], 16))
            # (decompositions of the current version alone, named UnicodeData.txt: those of earlier
            # versions corrected since are taken by no file system probed, HFS+ nor APFS)
            if name == "UnicodeData.txt" and r[5].strip() and "<" not in r[5]:
                pairs.append([c, "".join(chr(int(x, 16)) for x in r[5].split()), f"{name} decomposition"])
            for k, what in ((12, "upper"), (13, "lower"), (14, "title")):
                if r[k].strip():
                    pairs.append([c, chr(int(r[k], 16)), f"{name} {what}"])
    # Hangul syllables (decomposed by algorithm)
    for s in range(0xAC00, 0xAC00 + 11172):
        pairs.append([chr(s), unicodedata.normalize("NFD", chr(s)), "hangul"])
    return pairs


def related(c):
    out = {c}
    for f in (str.upper, str.lower, str.casefold, str.title):
        out.add(f(c))
    for form in ("NFD", "NFC"):
        out |= {unicodedata.normalize(form, s) for s in list(out)}
    return out


def stored(fd, path):
    if sys.platform == "darwin":
        import fcntl

        # F_GETPATH: the path as the file system keeps it
        return os.path.basename(fcntl.fcntl(fd, 50, b"\0" * 1024).split(b"\0")[0].decode("utf-8", "surrogateescape"))
    return os.path.basename(os.path.realpath(path))


def probe(d, bucket, given=None, cased=None):
    if given is not None:
        # the names of the pairs given, each pair in a directory of its own
        return probe_pairs(d, given)
    chars = []
    for cp in range(0x20, 0x110000):
        if 0xD800 <= cp < 0xE000 or cp in (0x2F, 0x7F):
            continue
        c = chr(cp)
        cat = unicodedata.category(c)
        if cat == "Cn" and not 0x200B <= cp <= 0x206F and cp != 0xFEFF:
            continue
        # (of --cased: characters of no case left out, but those of some case mapping)
        if cased is not None and cat in ("Lo", "Cn", "Co", "Cs") and c not in cased and not 0x200B <= cp <= 0x206F and cp != 0xFEFF:
            continue
        chars.append(c)
    # by bucket: its names, each character's together
    buckets, names, seen = [], [], set()
    for c in chars:
        for s in sorted(related(c)):
            n = "p" + s + "q"
            if n not in seen:
                seen.add(n)
                names.append(n)
        if bucket and len(names) >= bucket:
            buckets.append(names)
            names = []
    buckets.append(names)
    pairs, failed = [], 0
    fs = os.path.realpath(d)
    for k, names in enumerate(buckets):
        sub = os.path.join(d, f"b{k}") if bucket else d
        os.makedirs(sub, exist_ok=True)
        for n in ["pq"] + names:
            p = os.path.join(sub, n)
            try:
                fd = os.open(p, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                os.close(fd)
            except FileExistsError:
                fd = os.open(p, os.O_RDONLY)
                try:
                    there = stored(fd, p)
                finally:
                    os.close(fd)
                if there != n:
                    pairs.append([n, there, fs])
            except OSError:
                # (names the file system does not take: EILSEQ, EINVAL, EFAULT of FAT on macOS, ...)
                failed += 1
        print(f"bucket {k}: {len(names)} names, {len(pairs)} pairs so far, {failed} not taken", file=sys.stderr)
    return pairs


def probe_pairs(d, given):
    pairs, failed = [], 0
    fs = os.path.realpath(d)
    for k, (a, b, what) in enumerate(given):
        sub = os.path.join(d, f"g{k}")
        os.makedirs(sub)
        made = []
        for n in ("p" + a + "q", "p" + b + "q"):
            p = os.path.join(sub, n)
            try:
                fd = os.open(p, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
                os.close(fd)
                made.append(n)
            except FileExistsError:
                fd = os.open(p, os.O_RDONLY)
                try:
                    there = stored(fd, p)
                finally:
                    os.close(fd)
                if there != n:
                    pairs.append([n, there, f"{fs} ({what})"])
            except OSError:
                failed += 1
    print(f"{len(given)} pairs given, {len(pairs)} taken as one, {failed} names not taken", file=sys.stderr)
    return pairs


def cased(path):
    out = set()
    for a, b, what in json.load(open(path, encoding="utf-8")):
        if "decomposition" not in what and what != "hangul":
            out |= set(a) | set(b)
    return out


def main():
    a = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    s = a.add_subparsers(dest="cmd", required=True)
    u = s.add_parser("ucd")
    u.add_argument("out")
    u.add_argument("files", nargs="+")
    p = s.add_parser("probe")
    p.add_argument("dir")
    p.add_argument("out")
    p.add_argument("--bucket", type=int, default=0)
    p.add_argument("--names", help="a JSON of pairs (as OUT.json): probe their names alone, pair by pair")
    p.add_argument("--cased", help="a JSON of pairs of `ucd`: probe characters of a case alone (all but Lo, Cn, Co and Cs, and those of the pairs' case mappings)")
    args = a.parse_args()
    pairs = ucd_pairs(args.files) if args.cmd == "ucd" else probe(args.dir, args.bucket, args.names and json.load(open(args.names, encoding="utf-8")), args.cased and cased(args.cased))
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(pairs, f, ensure_ascii=False)
    print(f"{len(pairs)} pairs", file=sys.stderr)


if __name__ == "__main__":
    main()
