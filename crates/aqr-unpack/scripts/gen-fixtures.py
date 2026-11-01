#!/usr/bin/env python3
"""Writes tests/data: a small tree as Python's tarfile and zipfile archive it, with what the
readers must take or leave out.

- tree-pax.tar, tree-gnu.tar: a directory, files (one read-only), a hard link and a symbolic
  link to a file, a link reaching out, `../evil.txt`, `/abs.txt`, a long path, a name of a
  colon, a path given twice (the later kept), a FIFO, a Korean path; the pax one with an
  extended attribute and fractional times.
- tree.tar.gz (two members), .tar.bz2, .tar.xz, .tar.lzma: tree-pax.tar compressed.
- tree.zip: stored, DEFLATE, bzip2 and LZMA entries, a directory, a Unix symbolic link, a
  Korean name (UTF-8 flagged), an MS-DOS time, `../evil.txt`.
- wrapped.zip: a single directory at the top (`proj/`), of files and links (one reaching out of
  it): folded where extracted into `proj`.
- skip.zip: an entry flagged encrypted and one of Deflate64 (its method changed), left out.
- one.txt.gz, .bz2, .xz, .lzma: a single file.

    python3 -I crates/aqr-unpack/scripts/gen-fixtures.py
"""
import bz2
import gzip
import io
import lzma
import os
import tarfile
import zipfile

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'tests', 'data')
TEXT = b''.join(b'line %d of some text\n' % i for i in range(1000))


def member(t, name, data=b'', **kw):
    i = tarfile.TarInfo(name)
    i.mtime = 1_500_000_000
    for k, v in kw.items():
        setattr(i, k, v)
    if i.type in (tarfile.REGTYPE, tarfile.AREGTYPE):
        i.size = len(data)
        t.addfile(i, io.BytesIO(data))
    else:
        t.addfile(i)


def tree_tar(fmt):
    b = io.BytesIO()
    with tarfile.open(fileobj=b, mode='w', format=fmt) as t:
        member(t, 'dir', type=tarfile.DIRTYPE, mode=0o755, mtime=1_400_000_000)
        pax = {'SCHILY.xattr.user.note': 'kept', 'mtime': '1500000001.25'} if fmt == tarfile.PAX_FORMAT else {}
        member(t, 'dir/file.txt', TEXT, mode=0o644, mtime=1_500_000_001, pax_headers=pax)
        member(t, 'dir/ro.txt', b'read only\n', mode=0o444)
        member(t, 'dir/hard', type=tarfile.LNKTYPE, linkname='dir/file.txt')
        member(t, 'dir/sym', type=tarfile.SYMTYPE, linkname='file.txt')
        member(t, 'dir/escape', type=tarfile.SYMTYPE, linkname='../../outside')
        member(t, '../evil.txt', b'evil')
        member(t, '/abs.txt', b'absolute')
        member(t, 'long/' + 'n' * 150 + '/' + 'x' * 120 + '.txt', b'long name')
        member(t, '12:30.txt', b'colon')
        member(t, 'dup.txt', b'first')
        member(t, 'dup.txt', b'second')
        member(t, 'fifo', type=tarfile.FIFOTYPE)
        member(t, '한글/파일.txt', '유니코드'.encode())
    return b.getvalue()


def write(name, data):
    with open(os.path.join(OUT, name), 'wb') as f:
        f.write(data)


def main():
    os.makedirs(OUT, exist_ok=True)
    pax = tree_tar(tarfile.PAX_FORMAT)
    write('tree-pax.tar', pax)
    write('tree-gnu.tar', tree_tar(tarfile.GNU_FORMAT))
    half = len(pax) // 2
    write('tree.tar.gz', gzip.compress(pax[:half], mtime=0) + gzip.compress(pax[half:], mtime=0))
    write('tree.tar.bz2', bz2.compress(pax))
    write('tree.tar.xz', lzma.compress(pax))
    write('tree.tar.lzma', lzma.compress(pax, format=lzma.FORMAT_ALONE))

    b = io.BytesIO()
    with zipfile.ZipFile(b, 'w') as z:
        when = (2021, 2, 3, 4, 5, 6)
        for name, method in [('stored.txt', zipfile.ZIP_STORED), ('deflated.txt', zipfile.ZIP_DEFLATED),
                             ('bzip2.txt', zipfile.ZIP_BZIP2), ('lzma.txt', zipfile.ZIP_LZMA)]:
            z.writestr(zipfile.ZipInfo(name, when), TEXT, compress_type=method)
        z.writestr(zipfile.ZipInfo('dir/', when), b'')
        z.writestr(zipfile.ZipInfo('dir/in.txt', when), b'in dir')
        z.writestr(zipfile.ZipInfo('한글.txt', when), 'utf-8'.encode())
        z.writestr(zipfile.ZipInfo('../evil.txt', when), b'evil')
        i = zipfile.ZipInfo('link', when)
        i.create_system = 3
        i.external_attr = 0o120777 << 16
        z.writestr(i, b'stored.txt')
    write('tree.zip', b.getvalue())

    b = io.BytesIO()
    with zipfile.ZipFile(b, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr(zipfile.ZipInfo('proj/', (2019, 5, 6, 7, 8, 10)), b'')
        z.writestr(zipfile.ZipInfo('proj/a.txt', (2021, 2, 3, 4, 5, 6)), b'a')
        z.writestr(zipfile.ZipInfo('proj/sub/b.txt', (2021, 2, 3, 4, 5, 6)), b'b')
        for name, target in [('proj/link', b'a.txt'), ('proj/out', b'../x')]:
            i = zipfile.ZipInfo(name, (2021, 2, 3, 4, 5, 6))
            i.create_system = 3
            i.external_attr = 0o120777 << 16
            z.writestr(i, target)
    write('wrapped.zip', b.getvalue())

    b = io.BytesIO()
    with zipfile.ZipFile(b, 'w') as z:
        for name in ['ok.txt', 'secret.txt', 'd64.txt']:
            z.writestr(zipfile.ZipInfo(name, (2021, 1, 1, 0, 0, 0)), name.encode())
    d = bytearray(b.getvalue())
    # (local headers: the name's length at 26, the flags at 6; central ones: at 28 and 8)
    for sig, fixed, length, flags in [(b'PK\x03\x04', 30, 26, 6), (b'PK\x01\x02', 46, 28, 8)]:
        at = d.find(sig)
        while at >= 0:
            n = int.from_bytes(d[at + length:at + length + 2], 'little')
            name = bytes(d[at + fixed:at + fixed + n])
            if name == b'secret.txt':
                d[at + flags] |= 1
            if name == b'd64.txt':
                d[at + flags + 2:at + flags + 4] = (9).to_bytes(2, 'little')
            at = d.find(sig, at + 4)
    write('skip.zip', bytes(d))

    write('one.txt.gz', gzip.compress(TEXT, mtime=0))
    write('one.txt.bz2', bz2.compress(TEXT))
    write('one.txt.xz', lzma.compress(TEXT))
    write('one.txt.lzma', lzma.compress(TEXT, format=lzma.FORMAT_ALONE))


if __name__ == '__main__':
    main()
