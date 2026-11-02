@echo off
rem Builds target\dist\aqr-win-install.exe as the release workflow's job windows does
rem (.github\workflows\release.yml), for testing:
rem
rem   crates\aqr-win-install\build.bat
rem
rem aqr.exe and aqr-win.exe first (the dist profile), then the installer, whose build.rs packs
rem the two. AQR_VERSION, if set, is the binaries' version as a tag's run sets it
rem (crates\aqr-version); otherwise `git describe`'s.
setlocal
if not "%~1"=="" (
    echo usage: %~nx0 1>&2
    exit /b 2
)
cd /d "%~dp0..\.." || exit /b 1
cargo build --profile dist -p aqr -p aqr-win || exit /b 1
cargo build --profile dist -p aqr-win-install || exit /b 1
dir /-c target\dist\aqr.exe target\dist\aqr-win.exe target\dist\aqr-win-install.exe | findstr /i "\.exe"
