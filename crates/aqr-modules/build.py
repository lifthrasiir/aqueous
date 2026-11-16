"""Builds the modules (src/bin) through aqr-ld into wasm/, and src/known.rs from them.

With the versions of aqr-decompress's modules, as well as the spec's table of the registry's
initial contents (the crate of a module's document from the first line of its source). Nothing
else is rebuilt from them: the bytes kept are the modules (a toolchain giving other bytes makes
other modules, of other hashes). Any host runs it (`python crates/aqr-modules/build.py`; `cargo
aqr` runs it where what the modules are made of changed); it needs nothing beyond Python's own.
"""

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXE = ".exe" if os.name == "nt" else ""
MODULES = Path("crates/aqr-modules")
KNOWN = MODULES / "src/known.rs"
SPEC = Path("spec/index.bs")


def fail(message):
    sys.exit(f"build.py: {message}")


def cargo(*args, env=None, capture=False):
    run = subprocess.run(["cargo", *args], env=env, check=False, stdout=subprocess.PIPE if capture else None,
                         text=capture)
    if run.returncode != 0:
        fail(f"cargo {' '.join(args)} failed ({run.returncode})")
    return run.stdout


def main():
    os.chdir(ROOT)
    # (the order of the bytes for everything run, as for the lists here: a locale's collation may
    # pass over the hyphens, as Windows' en_US.UTF-8 does)
    env = dict(os.environ, LC_ALL="C")
    cargo("build", "--release", "-p", "aqr-ld", env=env)
    ld = str(ROOT / f"target/release/aqr-ld{EXE}")
    # (paths in the modules, of panics, made the same anywhere, and without the workspace's
    # directories; aqr-ld as rustc's wrapper makes the crates' metadata, which Cargo makes of their
    # paths with the host's separators, of no path, so that the symbols and the order they give the
    # data and functions are the same on any host and wherever a crate is)
    cargo_home = os.environ.get("CARGO_HOME") or str(Path.home() / ".cargo")
    wasm_env = dict(
        env,
        RUSTC_WORKSPACE_WRAPPER=ld,
        CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS=f"--remap-path-prefix={cargo_home}=/cargo "
        "--remap-path-prefix=crates/= --remap-path-prefix=codecs/=",
        CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER=ld,
    )
    cargo("build", "--profile", "release-wasm", "--target", "wasm32-unknown-unknown", "-p", "aqr-modules", "-F",
          "wasm", "--bins", env=wasm_env)
    built = Path("target/wasm32-unknown-unknown/release-wasm")
    listed = cargo("run", "-q", "--release", "-p", "aqr-decompress", "--example", "aqr-decompress-modules", env=env,
                   capture=True)
    # (name, version, binary)
    by_bin = {fields[2]: fields for fields in map(str.split, listed.splitlines()) if len(fields) == 3}

    wasm_dir = MODULES / "wasm"
    wasm_dir.mkdir(exist_ok=True)
    known = ["// Made by build.py.", "", "use crate::{Known, hex};", "", "pub const KNOWN: &[Known] = &["]
    rows = []
    for src in sorted((MODULES / "src/bin").glob("*.rs")):
        bin = src.stem
        if bin not in by_bin:
            fail(f"{bin} is no module of aqr-decompress")
        name, version, _ = by_bin[bin]
        wasm = wasm_dir / f"{bin}.wasm"
        shutil.copyfile(built / f"{bin}.wasm", wasm)
        digest = hashlib.new("sha512_256", wasm.read_bytes()).hexdigest()
        known += [
            "    Known {",
            f'        name: "{name}",',
            f"        version: {version},",
            f'        sha512_256: hex("{digest}"),',
            f'        block: include_bytes!("../wasm/{bin}.block"),',
            "    },",
        ]
        with open(src, encoding="utf-8") as f:
            crate = re.match(r"//!.*\((aqr-[a-z0-9-]*)\)", f.readline())
        if not crate:
            fail(f"no crate in the first line of {src.as_posix()}")
        crate = crate[1]
        rows += [
            "    <tr>",
            f"      <td><code>{digest}</code>",
            f"      <td>{name}@{version}",
            f'      <td><a href="https://github.com/lifthrasiir/aqueous/raw/main/crates/aqr-modules/wasm/{bin}.wasm"'
            f' title="crates/aqr-modules/wasm/{bin}.wasm">D</a>',
            f'        <a href="https://github.com/lifthrasiir/aqueous/blob/main/codecs/{crate}/FORMAT.md"'
            f' title="codecs/{crate}/FORMAT.md">R</a>',
        ]
    known.append("];")

    # the data of their module blocks, compressed once
    wasms = sorted(p.as_posix() for p in wasm_dir.glob("*.wasm"))
    cargo("run", "-q", "--release", "-p", "aqr-decompress", "--example", "aqr-decompress-modules", "--", *wasms,
          env=env)
    write(KNOWN, known)

    # (the rows of the first table after the registry's heading replaced)
    lines = SPEC.read_text(encoding="utf-8").split("\n")
    head = next((i for i, l in enumerate(lines) if l.startswith("<h3 id=modules-registry>")), None)
    start = next((i for i in range(head, len(lines)) if lines[i] == "  <tbody>"), None) if head is not None else None
    end = next((i for i in range(start, len(lines)) if lines[i] == "  </tbody>"), None) if start is not None else None
    if end is None:
        fail("no table of the modules registry in the spec")
    lines[start + 1:end] = rows
    write(SPEC, lines[:-1] if lines[-1] == "" else lines)

    # (made before the crates compiled above, which it would not rebuild by its time)
    os.utime(KNOWN)
    # (the files of modules no longer built; the others were overwritten, as known.rs had them)
    for f in [*wasm_dir.glob("*.wasm"), *wasm_dir.glob("*.block")]:
        if not (MODULES / "src/bin" / f"{f.stem}.rs").exists():
            f.unlink()
    for f in sorted(wasm_dir.iterdir()):
        print(f"{f.stat().st_size:>10} {f.as_posix()}")


def write(path, lines):
    """Writes the lines to the file by way of a new one beside it, with LF endings."""
    new = path.with_name(path.name + ".new")
    with open(new, "w", encoding="utf-8", newline="\n") as f:
        f.write("".join(l + "\n" for l in lines))
    os.replace(new, path)


if __name__ == "__main__":
    main()
