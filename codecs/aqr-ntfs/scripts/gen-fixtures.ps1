# Writes ntfs.vhd.zst: a 32 MiB fixed VHD whose NTFS volume Windows itself formatted and filled
# (resident and non-resident data, a hard link, an alternate stream, a sparse file, an NTFS-
# compressed directory, a WOF-compressed file, a JPEG, and a file in hundreds of pieces whose
# runlist needs an attribute list), compressed by zstd.
# Run in an elevated shell, with zstd on the path:
#   powershell -File gen-fixtures.ps1 [-Out DIR] [-Jpeg PATH]
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\tests\data'),
    [string]$Jpeg = (Join-Path $PSScriptRoot '..\..\aqr-jpeg\tests\data\gradient.jpg')
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$work = Join-Path ([IO.Path]::GetTempPath()) "aqr-ntfs-fixtures-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
$vhd = Join-Path $work 'ntfs.vhd'

# a free drive letter
$used = (Get-PSDrive -PSProvider FileSystem).Name
$letter = [char[]]'RSTUVWXYZ' | Where-Object { $used -notcontains [string]$_ } | Select-Object -First 1
if (-not $letter) { throw 'no free drive letter' }
$root = "${letter}:\"

function RunDiskpart($text) {
    $f = Join-Path $work 'diskpart.txt'
    Set-Content -Encoding ascii $f $text
    $o = diskpart.exe /s $f
    if ($LASTEXITCODE -ne 0) { $o; throw "diskpart failed" }
}

RunDiskpart @"
create vdisk file="$vhd" maximum=32 type=fixed
select vdisk file="$vhd"
attach vdisk
create partition primary
format fs=ntfs quick unit=4096 label=AQRNTFS
assign letter=$letter
"@

try {
    # text, resident-sized and then not
    Set-Content -NoNewline -Encoding ascii "${root}readme.txt" ("Aqueous NTFS fixture`r`n" * 40)
    New-Item -ItemType Directory "${root}bin" | Out-Null
    $bytes = New-Object byte[] 10000
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte](($i * 7 + ($i -shr 8)) % 251) }
    [IO.File]::WriteAllBytes("${root}bin\data.bin", $bytes)
    fsutil hardlink create "${root}link.bin" "${root}bin\data.bin" | Out-Null

    # an alternate data stream
    Set-Content -NoNewline -Encoding ascii "${root}ads.txt" 'main stream'
    Set-Content -NoNewline -Encoding ascii -Path "${root}ads.txt" -Stream extra -Value 'alternate stream'

    # a sparse file: data, a hole, data
    $sp = "${root}sparse.bin"
    [IO.File]::WriteAllBytes($sp, (New-Object byte[] 0))
    fsutil sparse setflag $sp | Out-Null
    $chunk = New-Object byte[] 65536
    for ($i = 0; $i -lt $chunk.Length; $i++) { $chunk[$i] = [byte](($i * 13 + 5) % 253) }
    $fs = [IO.File]::OpenWrite($sp)
    $fs.Write($chunk, 0, $chunk.Length)
    $fs.Seek(1048576 - 65536, 'Begin') | Out-Null
    $fs.Write($chunk, 0, $chunk.Length)
    $fs.Close()
    fsutil sparse setrange $sp 65536 (1048576 - 2 * 65536) | Out-Null

    # an NTFS-compressed directory with a text file
    New-Item -ItemType Directory "${root}packed" | Out-Null
    compact /c "${root}packed" | Out-Null
    $text = "The quick brown fox jumps over the lazy dog.`r`n" * 3000
    Set-Content -NoNewline -Encoding ascii "${root}packed\text.txt" $text

    # a WOF-compressed file (XPRESS4K)
    # (generated data, named like an executable: compact compresses by extension and size, not by
    # contents, and no Windows binary goes into a fixture)
    $wof = New-Object byte[] (256 * 1024)
    for ($i = 0; $i -lt $wof.Length; $i++) { $wof[$i] = [byte](($i * 5 + ($i -shr 7)) % 239) }
    [IO.File]::WriteAllBytes("${root}wof.exe", $wof)
    compact /c /exe:xpress4k "${root}wof.exe" | Out-Null

    Copy-Item $Jpeg "${root}photo.jpg"

    # a file in many pieces: append 4 KiB to frag.bin (1.5 MiB in all), flushed, and between the
    # appends make a spacer file that takes the next cluster; then delete the spacers
    New-Item -ItemType Directory "${root}spacers" | Out-Null
    $frag = New-Object byte[] (1536 * 1024)
    for ($i = 0; $i -lt $frag.Length; $i++) { $frag[$i] = [byte](($i * 31 + ($i -shr 10)) % 249) }
    $block = New-Object byte[] 4096
    $n = $frag.Length / 4096
    for ($i = 0; $i -lt $n; $i++) {
        $fs = New-Object IO.FileStream("${root}frag.bin", 'Append', 'Write', 'None', 4096, 'WriteThrough')
        $fs.Write($frag, $i * 4096, 4096)
        $fs.Flush($true)
        $fs.Dispose()
        [IO.File]::WriteAllBytes(("{0}spacers\s{1:D4}.bin" -f $root, $i), $block)
    }
    Remove-Item -Recurse -Force "${root}spacers"

    $ext = @(fsutil file queryextents "${root}frag.bin")
    "frag.bin extents: " + @($ext | Where-Object { $_ -match 'VCN' }).Count
    fsutil file queryextents "${root}wof.exe"
    fsutil file queryextents "${root}sparse.bin"
} finally {
    RunDiskpart @"
select vdisk file="$vhd"
detach vdisk
"@
}

$dest = Join-Path $Out 'ntfs.vhd'
Copy-Item $vhd $dest -Force
zstd -q -19 -f --rm $dest -o "$dest.zst"
Remove-Item -Recurse -Force $work
"wrote $dest.zst"
