#!/bin/sh
# Makes tests/data: small archives written by real tools, each in its own way (tar of every
# form bsdtar and Python write, with long names, links and pax headers; ar of BSD, GNU and
# mingw with symbol tables and long names; cpio of every form). Needs python3, bsdtar, cpio
# (bsdcpio), ar (macOS), llvm-ar, clang and x86_64-w64-mingw32-gcc. Run from anywhere.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out="$here/tests/data"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$work/src/sub" "$work/src/empty" "$work/obj"

# The same files for every tool: text, a duplicate, an empty file, noise of odd size, a long
# name, a symlink and a hard link, a directory.
python3 -I - "$work/src" <<'PY'
import os, random, sys
d = sys.argv[1]
random.seed(1)
text = "".join(f"line {i}: the quick brown fox jumps over {i * 7 % 13} lazy dogs\n" for i in range(150))
open(f"{d}/a.txt", "w").write(text)
open(f"{d}/sub/copy.txt", "w").write(text)
open(f"{d}/sub/empty", "w").close()
open(f"{d}/noise.bin", "wb").write(bytes(random.getrandbits(8) for _ in range(1001)))
long = "a-name-longer-than-one-hundred-bytes-" * 4
open(f"{d}/sub/{long}.txt", "w").write(" ".join(random.choice(["alpha", "beta", "gamma"]) for _ in range(300)))
os.symlink("a.txt", f"{d}/link")
os.link(f"{d}/a.txt", f"{d}/hard.txt")
PY
cd "$work/src"
find . -exec touch -h -t 202001020304.05 {} +
files="a.txt sub noise.bin empty link hard.txt"

export COPYFILE_DISABLE=1
for f in ustar pax gnutar v7tar; do bsdtar --no-xattrs --no-mac-metadata --uid 1000 --gid 1000 --uname user --gname user --format $f -cf "$out/bsdtar-$f.tar" $files; done
for f in newc odc bin; do bsdtar --no-xattrs --no-mac-metadata --uid 1000 --gid 1000 --uname user --gname user --format $f -cf "$out/bsdtar-$f.cpio" $files; done
find $files | cpio -o -H newc 2>/dev/null > "$out/bsdcpio-newc.cpio"

python3 -I - "$work/src" "$out" <<'PY'
import os, struct, sys, tarfile
src, out = sys.argv[1:]
os.chdir(src)
names = ["a.txt", "sub", "noise.bin", "empty", "link", "hard.txt"]
def owner(i):
    i.uid = i.gid = 1000
    i.uname = i.gname = "user"
    return i
for fmt, name in [(tarfile.GNU_FORMAT, "gnu"), (tarfile.PAX_FORMAT, "pax")]:
    with tarfile.open(f"{out}/py-{name}.tar", "w", format=fmt) as t:
        for n in names:
            t.add(n, filter=owner)
# pax headers with a size: a body larger than its header field says (as for 8 GiB files)
with tarfile.open(f"{out}/py-pax-size.tar", "w", format=tarfile.PAX_FORMAT) as t:
    i = owner(t.gettarinfo("noise.bin"))
    i.pax_headers = {"size": str(i.size), "comment": "x" * 300}
    t.addfile(i, open("noise.bin", "rb"))
# cpio's crc form (no tool here writes it), with names padded and upper-case fields
def crc_cpio(entries):
    o = bytearray()
    for name, data, mode in entries:
        n = name.encode() + b"\0"
        s = sum(data) & 0xFFFFFFFF
        fields = [1, mode, 0, 0, 1, 1577934245, len(data), 0, 0, 0, 0, len(n), s]
        o += b"070702" + b"".join(b"%08X" % v for v in fields) + n
        o += b"\0" * (-len(o) % 4) + data
        o += b"\0" * (-len(o) % 4)
    return bytes(o + b"\0" * (-len(o) % 512))
entries = [(n, open(n, "rb").read(), 0o100644) for n in ["a.txt", "noise.bin", "hard.txt"]] + [("TRAILER!!!", b"", 0)]
open(f"{out}/py-crc.cpio", "wb").write(crc_cpio(entries))
PY

# ar: objects with symbols, a long name, and a member of odd size
cd "$work/obj"
for i in 1 2 3; do
    printf 'int f%d(int x) { return x * %d + 1; }\nconst char *s%d = "string %d";\n' $i $i $i $i > m$i.c
done
cp m3.c a-member-with-a-long-name.c
clang -c -O1 m1.c m2.c a-member-with-a-long-name.c
cp "$work/src/noise.bin" odd.bin
ar rcs "$out/macos.a" m1.o m2.o a-member-with-a-long-name.o odd.bin 2>/dev/null
llvm-ar rcs --format=gnu "$out/llvm-gnu.a" m1.o m2.o a-member-with-a-long-name.o odd.bin
llvm-ar rcs --format=bsd "$out/llvm-bsd.a" m1.o m2.o a-member-with-a-long-name.o odd.bin
rm -f *.o
x86_64-w64-mingw32-gcc -c -O1 m1.c m2.c a-member-with-a-long-name.c
x86_64-w64-mingw32-ar rcs "$out/mingw.a" m1.o m2.o a-member-with-a-long-name.o odd.bin
bsdtar --format argnu -cf "$out/bsdtar-argnu.a" m1.o odd.bin
bsdtar --format arbsd -cf "$out/bsdtar-arbsd.a" m1.o odd.bin
ls -l "$out"
