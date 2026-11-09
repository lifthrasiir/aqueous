# Writes wof.vhd.zst: a 64 MiB fixed VHD (MBR, NTFS with 4 KiB clusters) whose NTFS volume Windows
# itself filled and compacted: xpress4k\, xpress8k\, xpress16k\ and lzx\ hold WOF-compressed files
# (compact /c /exe:...), lznt1\ NTFS-compressed ones (compact /c); each directory has text.txt,
# gen.bin and mixed.bin (the inputs of gen-xpress.ps1). Prints compact's reports. No Windows
# binary goes into it. It also writes wof-small.vhd.zst (16 MiB: plain.txt, lzx/text.txt and
# xpress4k/text.txt, each the first 256 KiB of text). Run in an elevated shell, with zstd on the path:
#   powershell -File gen-wof.ps1 [-Out DIR] [-Repo DIR]
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\tests\data'),
    [string]$Repo = (Join-Path $PSScriptRoot '..\..\..')
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path $Repo).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
# The inputs (the same bytes in gen-xpress.ps1 and gen-wof.ps1): text is the *.rs files under
# aqr-lzx\src and aqr-flate\src in path order, up to 1 MiB; gen is 1 MiB of (i*7 + (i>>8)) % 251;
# noise is 256 KiB of xorshift64's low bytes; mixed is text, gen and text again, up to 2 MiB.
Add-Type -TypeDefinition @'
using System; using System.IO; using System.Linq; using System.Collections.Generic;
public static class AqrInputs {
    static byte[] Take(IEnumerable<byte> b, int n) { return b.Take(n).ToArray(); }
    public static Dictionary<string, byte[]> Make(string repo) {
        var files = new List<string>();
        foreach (var d in new[] { "codecs/aqr-lzx/src", "codecs/aqr-flate/src" })
            files.AddRange(Directory.GetFiles(Path.Combine(repo, d), "*.rs", SearchOption.AllDirectories));
        files.Sort(StringComparer.Ordinal);
        var ms = new MemoryStream();
        foreach (var f in files) { var b = File.ReadAllBytes(f); ms.Write(b, 0, b.Length); }
        var text = Take(ms.ToArray(), 1 << 20);
        var gen = new byte[1 << 20];
        for (int i = 0; i < gen.Length; i++) gen[i] = (byte)((i * 7L + (i >> 8)) % 251);
        var noise = new byte[256 << 10];
        ulong x = 0x9e3779b97f4a7c15UL;
        for (int i = 0; i < noise.Length; i++) { x ^= x << 13; x ^= x >> 7; x ^= x << 17; noise[i] = (byte)x; }
        var mixed = Take(text.Concat(gen).Concat(text), 2 << 20);
        return new Dictionary<string, byte[]> { { "text", text }, { "gen", gen }, { "noise", noise }, { "mixed", mixed } };
    }
}
'@

$work = Join-Path ([IO.Path]::GetTempPath()) "aqr-wof-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$vhd = Join-Path $work 'wof.vhd'

$used = (Get-PSDrive -PSProvider FileSystem).Name
$letter = [char[]]'RSTUVWXYZ' | Where-Object { $used -notcontains [string]$_ } | Select-Object -First 1
if (-not $letter) { throw 'no free drive letter' }
$root = "${letter}:\"

function RunDiskpart($text) {
    $f = Join-Path $work 'diskpart.txt'
    Set-Content -Encoding ascii $f $text
    $o = diskpart.exe /s $f
    if ($LASTEXITCODE -ne 0) { $o; throw 'diskpart failed' }
}

$inputs = [AqrInputs]::Make($Repo)
RunDiskpart @"
create vdisk file="$vhd" maximum=64 type=fixed
select vdisk file="$vhd"
attach vdisk
create partition primary
format fs=ntfs quick unit=4096 label=AQRWOF
assign letter=$letter
"@
try {
    foreach ($d in 'xpress4k', 'xpress8k', 'xpress16k', 'lzx', 'lznt1') {
        New-Item -ItemType Directory "$root$d" | Out-Null
        [IO.File]::WriteAllBytes("$root$d\text.txt", $inputs['text'])
        [IO.File]::WriteAllBytes("$root$d\gen.bin", $inputs['gen'])
        [IO.File]::WriteAllBytes("$root$d\mixed.bin", $inputs['mixed'])
        foreach ($f in 'text.txt', 'gen.bin', 'mixed.bin') {
            if ($d -eq 'lznt1') { $o = compact /c "$root$d\$f" } else { $o = compact /c "/exe:$d" "$root$d\$f" }
            if ($LASTEXITCODE -ne 0) { $o; throw "compact failed on $d\$f" }
        }
    }
    "--- compact /q"
    foreach ($d in 'xpress4k', 'xpress8k', 'xpress16k', 'lzx', 'lznt1') {
        "[$d]"
        compact /q "$root$d\*"
        compact "$root$d\*"
    }
    "--- file sizes"
    Get-ChildItem -Recurse -File $root | ForEach-Object { '{0}: {1}' -f $_.FullName.Substring(3), $_.Length }
}
finally {
    RunDiskpart @"
select vdisk file="$vhd"
detach vdisk
"@
}
zstd -19 -f -q $vhd -o (Join-Path $Out 'wof.vhd.zst')
if ($LASTEXITCODE -ne 0) { throw 'zstd failed' }
"wof.vhd: $((Get-Item $vhd).Length) bytes, zst $((Get-Item (Join-Path $Out 'wof.vhd.zst')).Length)"

# the small one: 16 MiB, three copies of the first 256 KiB of text
$vhd = Join-Path $work 'wof-small.vhd'
RunDiskpart @"
create vdisk file="$vhd" maximum=16 type=fixed
select vdisk file="$vhd"
attach vdisk
create partition primary
format fs=ntfs quick unit=4096 label=AQRWOFS
assign letter=$letter
"@
try {
    $small = New-Object byte[] (256KB)
    [Array]::Copy($inputs['text'], $small, $small.Length)
    [IO.File]::WriteAllBytes("${root}plain.txt", $small)
    foreach ($d in 'lzx', 'xpress4k') {
        New-Item -ItemType Directory "$root$d" | Out-Null
        [IO.File]::WriteAllBytes("$root$d\text.txt", $small)
        $o = compact /c "/exe:$d" "$root$d\text.txt"
        if ($LASTEXITCODE -ne 0) { $o; throw "compact failed on $d" }
    }
    "--- wof-small compact /q"
    foreach ($d in 'lzx', 'xpress4k') { compact /q "$root$d\text.txt"; compact "$root$d\text.txt" }
}
finally {
    RunDiskpart @"
select vdisk file="$vhd"
detach vdisk
"@
}
zstd -19 -f -q $vhd -o (Join-Path $Out 'wof-small.vhd.zst')
if ($LASTEXITCODE -ne 0) { throw 'zstd failed' }
"wof-small.vhd: $((Get-Item $vhd).Length) bytes, zst $((Get-Item (Join-Path $Out 'wof-small.vhd.zst')).Length)"
Remove-Item -Recurse -Force $work
