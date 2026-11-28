# aqr-mount-win

`aqr-mount.exe ARCHIVE [MOUNTPOINT]` mounts the archive as a read-only volume through
[WinFsp](https://winfsp.dev) where it is installed. MOUNTPOINT is a drive letter (`X:`) or a
directory that does not exist yet; without one, the next free drive letter is taken. The volume is there for as long as the
program runs: ending it in any way (Ctrl+C, a kill, a crash) unmounts, as the driver tears
down a volume whose process has closed its handle. Neither mounting nor the driver's start
needs an administrator (WinFsp's installer lets everyone start its driver), and nothing is
written to the registry (the Launcher's `Services` keys are for `net use` and are not used).

The volume serves aqr-mount's tree of the archive (`ServedTree`, over an `aqr --serve`;
`src/fs.rs` is the adapter): names in any case, the volume's label the archive's name. `aqr` is
`AQR`, else the one beside the program, else the one on `PATH`, where the installer puts its
folder. `aqr --serve` ends with the program, however it ends (its standard input closed).

It is released on its own, beside the installer and not in it: it is of use only where WinFsp
is installed, and its license is not the others'. The release's `aqr-STEM-win-mount.zip`
(`.github/workflows/release.yml`, job `windows-mount`: the Windows 7 target, as the others)
holds `aqr-mount.exe` and `COPYING`, the GPL's text (here as Debian's
`/usr/share/common-licenses/GPL-3` has it, verbatim); its source is the tag's, with the crates
of Cargo.lock. The job checks that it starts where WinFsp is not installed, and says so.

## License

The sources are Aqueous's (public domain, MIT-0). The program links the bindings of WinFsp
(`winfsp`, `winfsp-sys`), which are GPL-3.0, so the binary as built is GPL-3.0. This is why it
is a program of its own and released apart: `aqr`, `aqr-win`, the installer and the rest take
none of it.

## Loading WinFsp

The DLL is delay-loaded (`build.rs`: a dependency's `rustc-link-arg` reaches only its own
targets), so the program starts without WinFsp and says it is missing. `src/load.rs` loads the
DLL by the full path in `HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir` (`SOFTWARE\WinFsp` on
ARM64) before the bindings would load it by its bare name, so that a DLL planted beside the
program is never taken.

## Building

winfsp-sys makes its bindings with bindgen, so building needs libclang. LLVM's own installation
(GitHub's Windows runners have one) should find itself and Visual Studio's headers (not tried
yet). A clang that does not find MSVC's headers (MSYS2's, as on the machine it was written on)
needs `LIBCLANG_PATH` set to its `bin` and `INCLUDE` set to MSVC's `include`
and the Windows SDK's `ucrt`, `um` and `shared`. Both x86_64-pc-windows-msvc and the release's
Windows 7 target (`cargo +NIGHTLY win7 -p aqr-mount-win`) build. Other systems build a stub that
says it is the program of Windows.

## Tests

`tests/mount.rs` mounts an archive on a directory (a junction WinFsp makes, of no
administrator) as the program is run (the `aqr` built beside it: `cargo rt -p aqr -p
aqr-mount-win`): its files read whole and in another case, writes refused, the directory gone
with the process killed. It is skipped where WinFsp is not installed (CI's runners).

## Left

- `aqr mount` and aqr-win's verb starting it, and unmounting it.
- A security descriptor of the volume (none is given now: WinFsp then grants what a read-only
  volume allows).
- Not tried on Windows 7 itself: WinFsp 2.1's support of it is unverified.
