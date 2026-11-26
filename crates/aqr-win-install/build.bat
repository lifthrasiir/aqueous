@echo off
rem Builds target\dist\aqr-win-install.exe as the release workflow's job windows does
rem (.github\workflows\release.yml), for testing:
rem
rem   crates\aqr-win-install\build.bat
rem
rem aqr.exe, aqr-win.exe and aqr_shell.dll first (the dist profile), then the installer, whose
rem build.rs packs them. AQR_VERSION, if set, is the binaries' version as a tag's run sets it
rem (crates\aqr-version); otherwise `git describe`'s.
setlocal
if not "%~1"=="" (
    echo usage: %~nx0 1>&2
    exit /b 2
)
cd /d "%~dp0..\.." || exit /b 1
cargo build --profile dist -p aqr -p aqr-win -p aqr-win-shell || exit /b 1
cargo build --profile dist -p aqr-win-install || exit /b 1
dir /-c target\dist\aqr.exe target\dist\aqr-win.exe target\dist\aqr-win-install.exe | findstr /i "\.exe"
