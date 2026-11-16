"""Gathers what aqr embeds (Windows x86-64: the interpreter blob, and what `aqr --format sfx` puts
into a self-extracting program) into target/aqr-sfx/, and packs it into sfx.pack, which aqr's
build script includes:

- aqr-sfx-shim.exe, the stub (release), packed by UPX (`upx -9`, as the release packs aqr.exe; the
  one AQR_UPX names (not UPX: upx reads that as its options), else `upx` on the path; where there
  is none, left as it is with a warning);
- aqr-interp.blob, the interpreter blob (aqr-interp built for the no-TLS target, digested by
  `aqr-ld blob`), for the modules with no native blob;
- modules/NAME.blob, each module of aqr-modules built as a native library for the no-TLS target
  (crates/aqr-dll/build.py with NOTLS=1), and libs/LIB.blob, the shared libraries the modules
  link against (crates/aqr-dll/layout.py), all digested together by `aqr-ld blobs` (the
  imports between them bound); and modules.txt: each NAME and the SHA-512/256 of its module
  when packed (crates/aqr-modules/src/known.rs), so that aqr uses a blob only for the module it
  was built from;
- sfx.pack, all of these as aqr carries them (aqr-sfx's `aqr-sfx-pack`: the blobs' code through
  aqr-exe's x86 disfilter, then raw LZMA2 at preset 9, which aqr alone reads);
- pack.id, a digest of the files.

The modules named (`python crates/aqr-sfx/pack.py [MODULE...]`) are packed (all by default). It
needs the nightly toolchain of the file nightly-toolchain (AQR_NIGHTLY to use another) and
build-std (as NOTLS does). `cargo aqr` (aqr-bincache) runs it again where its modules.txt is not of
the modules of known.rs.
"""

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = Path("target/aqr-sfx")
KNOWN = Path("crates/aqr-modules/src/known.rs")


def fail(message):
    sys.exit(f"pack.py: {message}")


def run(*args, env=None, stdout=None):
    done = subprocess.run(args, env=env, check=False, stdout=stdout)
    if done.returncode != 0:
        fail(f"{' '.join(map(str, args))} failed ({done.returncode})")


def main():
    os.chdir(ROOT)
    if sys.platform != "win32":
        fail("Windows only (the blobs are PE images)")
    modules = sys.argv[1:]

    run("cargo", "build", "-q", "--release", "-p", "aqr-ld", "-p", "aqr-sfx-shim")
    ld = ROOT / "target/release/aqr-ld.exe"
    # (all of them, for build.py to cut the shared libraries to what they use)
    run(sys.executable, "-I", "crates/aqr-dll/build.py", *modules, env=dict(os.environ, NOTLS="1"))
    modules = modules or sorted(p.stem for p in Path("crates/aqr-modules/src/bin").glob("*.rs"))
    nightly = os.environ.get("AQR_NIGHTLY") or Path("nightly-toolchain").read_text().strip()
    # (linked by aqr-ld as build.py links, so that the standard library built for the target is not
    # built again for a linker of its own)
    run("cargo", f"+{nightly}", "build", "-q", "--release", "-p", "aqr-interp", "--target",
        "crates/aqr-dll/x86_64-aqr-windows-msvc.json", "-Z", "json-target-spec", "-Z", "build-std",
        env=dict(os.environ, CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER=str(ld)))

    shutil.rmtree(OUT, ignore_errors=True)
    (OUT / "modules").mkdir(parents=True)
    (OUT / "libs").mkdir()
    stub = OUT / "aqr-sfx-shim.exe"
    shutil.copyfile("target/release/aqr-sfx-shim.exe", stub)
    upx_name = os.environ.get("AQR_UPX") or "upx"
    upx = shutil.which(upx_name)
    if upx:
        run(upx, "-9", "-q", stub, stdout=subprocess.DEVNULL)
    else:
        print(f"pack.py: no UPX ({upx_name}): the stub is left unpacked", file=sys.stderr)
    run(ld, "blob", "--apply", "aqr_interp_vtable", "--info", "-", "--out", OUT / "aqr-interp.blob",
        "target/x86_64-aqr-windows-msvc/release/aqr_interp.dll")
    libs = [f"target/aqr-dll/aqr_dll_{d.name.replace('-', '_')}.dll"
            for d in sorted(Path("crates/aqr-dll/shared").iterdir()) if d.is_dir()]
    run(ld, "blobs", "--out", OUT / "all", *libs, *(f"target/aqr-dll/{m}.dll" for m in modules))
    for blob in (OUT / "all").glob("*.blob"):
        blob.replace(OUT / ("libs" if blob.name.startswith("aqr_dll_") else "modules") / blob.name)
    (OUT / "all").rmdir()

    # (the entry whose block is the binary's: a name of known.rs may have a / for the binary's -)
    hashes, last = {}, None
    for line in KNOWN.read_text(encoding="utf-8").splitlines():
        if h := re.search(r'sha512_256: hex\("([0-9a-f]*)"\)', line):
            last = h[1]
        elif b := re.search(r'/wasm/([^/"]*)\.block"\),', line):
            hashes.setdefault(b[1], last)
    lines = []
    for m in modules:
        if not hashes.get(m):
            fail(f"{m} not in known.rs")
        lines.append(f"{m} {hashes[m]}\n")
    write(OUT / "modules.txt", "".join(lines))
    lib_names = sorted(p.name for p in (OUT / "libs").iterdir())
    write(OUT / "libs.txt", "".join(f"{n}\n" for n in lib_names))
    run("cargo", "run", "-q", "--release", "-p", "aqr-sfx", "-F", "encode,xz", "--bin", "aqr-sfx-pack", "--", OUT)

    # what the pack is, for builds to tell one from another (aqr includes it, so that its build is
    # not taken for one of another pack): sha256sum's lines of the files, digested
    files = sorted(p.relative_to(OUT).as_posix() for p in OUT.rglob("*") if p.is_file())
    sums = "".join(f"{hashlib.sha256((OUT / f).read_bytes()).hexdigest()}  ./{f}\n" for f in files)
    write(OUT / "pack.id", hashlib.sha256(sums.encode()).hexdigest() + "\n")

    def size(pattern):
        return sum(p.stat().st_size for p in OUT.glob(pattern) if p.is_file())

    print(f"{OUT.as_posix()}: {size('**/*')} bytes, {len(modules)} modules, {len(lib_names)} shared libraries")
    print(f"  modules {size('modules/*.blob')}, shared libraries {size('libs/*.blob')} bytes")


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


if __name__ == "__main__":
    main()
