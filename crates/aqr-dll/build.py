"""Builds the modules named as native libraries into target/aqr-dll/.

The modules are binaries of crates/aqr-modules (`lz4-recons`; all of them by default), built one
after another (`python crates/aqr-dll/build.py [MODULE...]`): each a build of this crate with
AQR_DLL_MODULE set, the crates under it compiled once. PROFILE (release by default) is the
profile built with.

The crates that several modules use are in shared libraries (layout.py, run first: the dylibs of
shared/, the runtime aqr-dll-rt among them), built with the first module and copied into
target/aqr-dll/ beside the modules (`aqr_dll_NAME.dll`); each module's library links against
them.

With NOTLS=1 (Windows), the libraries are built for the custom target x86_64-aqr-windows-msvc.json,
which has native thread-locals off so the standard library's thread-locals use TlsAlloc rather
than a TLS directory (one per image, not relocatable). Such a library is mergeable into a stub by
`aqr-ld merge` (its code uses no TLS directory, only imports). It needs the nightly toolchain of
nightly-toolchain (AQR_NIGHTLY to use another) and build-std, so it is slower. aqr-ld links them
(src/msvc.rs): building every module, the shared libraries are linked again exporting only what
the others import from them and what rustc has every link reference (`aqr-ld exports`,
target/aqr-dll/exports/ and needed/), the rest of their code left out, until that no longer
changes (each one less imports from the others).
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = Path("target/aqr-dll")
SHARED = Path("crates/aqr-dll/shared")


def fail(message):
    sys.exit(f"build.py: {message}")


def run(*args, env=None, stdout=None):
    done = subprocess.run(args, env=env, check=False, stdout=stdout)
    if done.returncode != 0:
        fail(f"{' '.join(map(str, args))} failed ({done.returncode})")


def touch_shared():
    for lib in SHARED.glob("*/src/lib.rs"):
        os.utime(lib)


def same_tree(a, b):
    def files(d):
        return {p.relative_to(d): p for p in d.rglob("*") if p.is_file()}

    fa, fb = files(a), files(b)
    return fa.keys() == fb.keys() and all(fa[k].read_bytes() == fb[k].read_bytes() for k in fa)


def main():
    os.chdir(ROOT)
    profile = os.environ.get("PROFILE") or "release"
    if sys.platform == "win32":
        lib, ext, pre_lib = "aqr_dll.dll", "dll", ""
    elif sys.platform == "darwin":
        lib, ext, pre_lib = "libaqr_dll.dylib", "dylib", "lib"
    else:
        lib, ext, pre_lib = "libaqr_dll.so", "so", "lib"
    modules = sys.argv[1:]
    every = not modules
    modules = modules or sorted(p.stem for p in Path("crates/aqr-modules/src/bin").glob("*.rs"))
    exports = OUT / "exports"
    needed = OUT / "needed"
    notls = os.environ.get("NOTLS") == "1"
    env = dict(os.environ)
    if notls:
        # (the nightly of nightly-toolchain, or AQR_NIGHTLY's)
        nightly = os.environ.get("AQR_NIGHTLY") or Path("nightly-toolchain").read_text().strip()
        pre = [f"+{nightly}"]
        post = ["--target", "crates/aqr-dll/x86_64-aqr-windows-msvc.json", "-Z", "json-target-spec", "-Z",
                "build-std"]
        built = Path(f"target/x86_64-aqr-windows-msvc/{profile}")
        run("cargo", "build", "-q", "--release", "-p", "aqr-ld")
        env.update(
            CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER=str(ROOT / "target/release/aqr-ld.exe"),
            AQR_LD_EXPORTS=str(ROOT / exports),
            AQR_LD_NEEDED=str(ROOT / needed),
        )
    else:
        pre, post = [], []
        built = Path(f"target/{profile}")
    run(sys.executable, "-I", "crates/aqr-dll/layout.py", env=env, stdout=subprocess.DEVNULL)
    OUT.mkdir(parents=True, exist_ok=True)
    # (the shared libraries linked whole first: what the modules import from them is what is found)
    shutil.rmtree(exports, ignore_errors=True)
    shutil.rmtree(needed, ignore_errors=True)
    touch_shared()

    def module(m):
        run("cargo", *pre, "build", "-q", "--profile", profile, "-p", "aqr-dll", *post,
            env=dict(env, AQR_DLL_MODULE=m))
        shutil.copyfile(built / lib, OUT / f"{m}.{ext}")

    def libs():
        dirs = sorted(d for d in SHARED.iterdir() if d.is_dir())
        return [OUT / f"{pre_lib}aqr_dll_{d.name.replace('-', '_')}.{ext}" for d in dirs]

    def copy_libs():
        for l in libs():
            shutil.copy2(built / l.name, OUT)

    def size(paths):
        return sum(p.stat().st_size for p in paths)

    for m in modules:
        if not Path(f"crates/aqr-modules/src/bin/{m}.rs").exists():
            fail(f"no module {m}")
        module(m)
        print(f"{OUT.as_posix()}/{m}.{ext}: {size([OUT / f'{m}.{ext}'])} bytes", flush=True)
    copy_libs()
    whole = size(libs())
    if notls and every:
        # (the modules imported from as they are; only the shared libraries linked again, by the
        # first module's build)
        mods = [OUT / f"{m}.dll" for m in modules]
        new = OUT / "exports.new"
        for round in range(1, 7):
            run(ROOT / "target/release/aqr-ld.exe", "exports", "--out", new, "--needed", needed, *libs(), *mods)
            if exports.is_dir() and same_tree(exports, new):
                shutil.rmtree(new)
                break
            shutil.rmtree(exports, ignore_errors=True)
            new.rename(exports)
            touch_shared()
            module(modules[0])
            copy_libs()
            print(f"build.py: round {round}, the shared libraries {size(libs())} bytes", flush=True)
    count = sum(1 for d in SHARED.iterdir() if d.is_dir())
    print(f"{OUT.as_posix()}: {count} shared libraries, {size(libs())} bytes (whole {whole})")


if __name__ == "__main__":
    main()
