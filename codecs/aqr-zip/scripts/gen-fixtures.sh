#!/bin/sh
# Makes tests/data: small archives written by real tools, each in its own way (data descriptors
# with or without signature, zip64 fields, extra fields, comments, stored and deflated entries).
# Needs python3, zip, ditto (macOS), bsdtar and go. Run from anywhere.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out="$here/tests/data"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$work/src/sub" "$work/src/empty"

# The same files for every tool: text, a duplicate, an empty file, noise, a directory.
python3 -I - "$work/src" <<'PY'
import os, random, sys
d = sys.argv[1]
random.seed(1)
text = "".join(f"line {i}: the quick brown fox jumps over {i * 7 % 13} lazy dogs\n" for i in range(800))
open(f"{d}/a.txt", "w").write(text)
open(f"{d}/sub/copy.txt", "w").write(text)
open(f"{d}/sub/empty", "w").close()
open(f"{d}/noise.bin", "wb").write(bytes(random.getrandbits(8) for _ in range(3000)))
open(f"{d}/sub/words.txt", "w").write(" ".join(random.choice(["alpha", "beta", "gamma", "delta"]) for _ in range(2000)))
PY
for f in a.txt sub/copy.txt sub/empty noise.bin sub/words.txt empty; do touch -t 202001020304.05 "$work/src/$f"; done

python3 -I - "$work/src" "$out" <<'PY'
import io, os, sys, zipfile
src, out = sys.argv[1:]
files = ["a.txt", "sub/copy.txt", "sub/empty", "noise.bin", "sub/words.txt"]
def write(name, **kw):
    with zipfile.ZipFile(f"{out}/{name}", "w", **kw) as z:
        for f in files:
            z.write(f"{src}/{f}", f)
write("py-deflate.zip", compression=zipfile.ZIP_DEFLATED)
write("py-stored.zip", compression=zipfile.ZIP_STORED)
write("py-level9.zip", compression=zipfile.ZIP_DEFLATED, compresslevel=9)
# to a stream that cannot seek: data descriptors with their signature
class Unseekable(io.RawIOBase):
    def __init__(self): self.buf = bytearray()
    def writable(self): return True
    def write(self, b): self.buf += b; return len(b)
u = Unseekable()
with zipfile.ZipFile(u, "w", compression=zipfile.ZIP_DEFLATED) as z:
    for f in files:
        with open(f"{src}/{f}", "rb") as i, z.open(f, "w", force_zip64=f == "noise.bin") as o:
            o.write(i.read())
open(f"{out}/py-stream.zip", "wb").write(u.buf)
# zip64 extra fields in every header, and a comment
with zipfile.ZipFile(f"{out}/py-zip64.zip", "w", compression=zipfile.ZIP_DEFLATED) as z:
    z.comment = b"a comment with PK\x05\x06 inside"
    for f in files:
        with open(f"{src}/{f}", "rb") as i, z.open(f, "w", force_zip64=True) as o:
            o.write(i.read())
PY

cd "$work/src"
zip -q -X -r "$out/infozip.zip" a.txt sub noise.bin
zip -q -r -9 "$out/infozip-extra.zip" a.txt sub noise.bin empty
# to stdout: data descriptors
zip -q -r - a.txt sub noise.bin | cat > "$out/infozip-stream.zip"
ditto -c -k --sequesterRsrc . "$out/ditto.zip"
bsdtar --format zip -cf "$out/bsdtar.zip" a.txt sub noise.bin
cat > "$work/w.go" <<'GO'
package main

import (
	"archive/zip"
	"os"
)

func main() {
	f, _ := os.Create(os.Args[1])
	w := zip.NewWriter(f)
	w.SetComment("go")
	for _, n := range []string{"a.txt", "sub/copy.txt", "noise.bin", "sub/words.txt"} {
		d, _ := os.ReadFile(n)
		e, _ := w.Create(n)
		e.Write(d)
	}
	w.Close()
	f.Close()
}
GO
go run "$work/w.go" "$out/go.zip"
ls -l "$out"
