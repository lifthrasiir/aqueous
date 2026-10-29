# Writes tests/data: small images made by Windows' IMAPI2 (the file systems of Windows' own
# discs), of files made here, compressed by zstd (mostly zeros, as UDF pads its images).
# Run on Windows, with zstd on the path: powershell -File scripts/gen-fixtures.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'tests\data'
$src = Join-Path ([IO.Path]::GetTempPath()) 'aqr-iso-fixtures'
Remove-Item -Recurse -Force $src -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $out, "$src\docs\deeper", "$src\bin" | Out-Null

# the files: text, a file of several sectors, an empty one, the same contents twice, a long
# name and a non-ASCII one
Set-Content -NoNewline -Encoding ascii "$src\readme.txt" ("Aqueous ISO fixture`r`n" * 40)
$bytes = New-Object byte[] 10000
for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte](($i * 7 + ($i -shr 8)) % 251) }
[IO.File]::WriteAllBytes("$src\bin\data.bin", $bytes)
[IO.File]::WriteAllBytes("$src\bin\copy.bin", $bytes)
[IO.File]::WriteAllBytes("$src\docs\empty.dat", (New-Object byte[] 0))
Set-Content -NoNewline -Encoding ascii "$src\docs\deeper\a rather long file name.json" '{"a": [1, 2, 3], "b": "c"}'
# (Hangul by code points: Windows PowerShell reads scripts in the ANSI code page)
$hangul = [string][char]0xD55C + [char]0xAE00
[IO.File]::WriteAllText("$src\docs\$hangul.txt", ([string][char]0xBB3C + ' aqueous'))

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices.ComTypes;
public static class StreamOut {
    public static void Save(object o, string path) {
        var s = (IStream)o;
        var buf = new byte[65536];
        var read = Marshal_AllocInt();
        using (var f = File.Create(path)) {
            while (true) {
                s.Read(buf, buf.Length, read);
                int n = System.Runtime.InteropServices.Marshal.ReadInt32(read);
                if (n == 0) break;
                f.Write(buf, 0, n);
            }
        }
        System.Runtime.InteropServices.Marshal.FreeHGlobal(read);
    }
    static IntPtr Marshal_AllocInt() { return System.Runtime.InteropServices.Marshal.AllocHGlobal(4); }
}
'@

# file systems: 1 ISO 9660, 2 Joliet, 4 UDF
function Image($name, $systems, $udf) {
    $fs = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
    $fs.FileSystemsToCreate = $systems
    if ($udf) { $fs.UDFRevision = $udf }
    $fs.VolumeName = 'AQR_ISO'
    $fs.Root.AddTree($src, $false)
    $img = $fs.CreateResultImage()
    $iso = Join-Path $out $name
    [StreamOut]::Save($img.ImageStream, $iso)
    zstd -q -19 -f --rm $iso -o "$iso.zst"
}

Image 'iso-joliet-udf102.iso' 7 0x102
Image 'iso.iso' 1 $null
Image 'joliet.iso' 3 $null
Image 'udf201.iso' 4 0x201
Image 'udf250.iso' 4 0x250
Remove-Item -Recurse -Force $src
