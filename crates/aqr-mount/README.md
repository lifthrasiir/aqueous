# aqr-mount

`aqr-mount [MOUNTPOINT]` mounts a read-only volume through [WinFsp](https://winfsp.dev) where
it is installed. MOUNTPOINT is a drive letter (`X:`) or a directory that does not exist yet;
without one, the next free drive letter is taken. The volume is there for as long as the
program runs: ending it in any way (Ctrl+C, a kill, a crash) unmounts, as the driver tears
down a volume whose process has closed its handle. Neither mounting nor the driver's start
needs an administrator (WinFsp's installer lets everyone start its driver), and nothing is
written to the registry (the Launcher's `Services` keys are for `net use` and are not used).

For now the volume holds a single file of fixed contents, `\hello.txt`: the skeleton that an
archive's listing and extraction will fill.

## License

The sources are Aqueous's (public domain, MIT-0). The program links the bindings of WinFsp
(`winfsp`, `winfsp-sys`), which are GPL-3.0, so the binary as built is GPL-3.0. This is why it
is a program of its own: `aqr`, `aqr-win` and the rest take none of it.

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
Windows 7 target (`cargo +NIGHTLY win7 -p aqr-mount`) build. Other systems build a stub that
says WinFsp is needed.

## Left

- An archive's entries through aqr's listing and extraction, and reading at an offset.
- `aqr mount` and aqr-win's verb starting it, and unmounting it.
- A security descriptor of the volume (none is given now: WinFsp then grants what a read-only
  volume allows).
- Tests (they need WinFsp installed; CI's Windows runner does not have it).
- Not tried on Windows 7 itself: WinFsp 2.1's support of it is unverified.
