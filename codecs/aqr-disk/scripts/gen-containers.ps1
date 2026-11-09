# Writes win.vhdx.zst, win.vhd.zst (and win-hv.vhdx.zst when Hyper-V's module is present): dynamic
# VHDX / VHD containers of 64 MiB whose NTFS volume Windows itself formatted and filled (a text file,
# two generated binaries, a JPEG), compressed by zstd -19. No Windows binary goes into them.
# Run in an elevated shell, with zstd on the path:
#   powershell -File gen-containers.ps1 [-Out DIR] [-Jpeg PATH]
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\tests\data'),
    [string]$Jpeg = (Join-Path $PSScriptRoot '..\..\aqr-jpeg\tests\data\gradient.jpg')
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$Jpeg = (Resolve-Path $Jpeg).Path
$work = Join-Path ([IO.Path]::GetTempPath()) "aqr-containers-$PID"
New-Item -ItemType Directory -Force $work | Out-Null

function FreeLetter {
    $used = (Get-PSDrive -PSProvider FileSystem).Name
    $l = [char[]]'RSTUVWXYZ' | Where-Object { $used -notcontains [string]$_ } | Select-Object -First 1
    if (-not $l) { throw 'no free drive letter' }
    [string]$l
}

function RunDiskpart($text) {
    $f = Join-Path $work 'diskpart.txt'
    Set-Content -Encoding ascii $f $text
    $o = diskpart.exe /s $f
    if ($LASTEXITCODE -ne 0) { $o; throw 'diskpart failed' }
}

function Fill($root) {
    Set-Content -NoNewline -Encoding ascii "${root}readme.txt" ("Aqueous container fixture`r`n" * 40)
    $b = New-Object byte[] (1MB)
    for ($i = 0; $i -lt $b.Length; $i++) { $b[$i] = [byte](($i * 7 + ($i -shr 8)) % 251) }
    [IO.File]::WriteAllBytes("${root}data.bin", $b)
    $b = New-Object byte[] (8MB)
    for ($i = 0; $i -lt $b.Length; $i++) { $b[$i] = [byte](($i * 11 + ($i -shr 12)) % 241) }
    [IO.File]::WriteAllBytes("${root}big.bin", $b)
    Copy-Item $Jpeg "${root}photo.jpg"
}

$report = @()

function Pack($path) {
    $name = Split-Path $path -Leaf
    $size = (Get-Item $path).Length
    $info = ''
    if (Get-Command Get-VHD -ErrorAction SilentlyContinue) {
        $v = Get-VHD -Path $path
        $info = "VhdFormat=$($v.VhdFormat) VhdType=$($v.VhdType) BlockSize=$($v.BlockSize) LogicalSectorSize=$($v.LogicalSectorSize)"
    }
    zstd -19 -f -q $path -o (Join-Path $Out "$name.zst")
    if ($LASTEXITCODE -ne 0) { throw 'zstd failed' }
    $script:report += "$name : $size bytes, zst $((Get-Item (Join-Path $Out "$name.zst")).Length) $info"
}

# the diskpart containers: the style from the extension; GPT for .vhdx, MBR for .vhd
foreach ($spec in @(@('win.vhdx', 'gpt'), @('win.vhd', 'mbr'))) {
    $file = Join-Path $work $spec[0]
    $letter = FreeLetter
    RunDiskpart @"
create vdisk file="$file" maximum=64 type=expandable
select vdisk file="$file"
attach vdisk
convert $($spec[1])
create partition primary
format fs=ntfs quick unit=4096 label=AQRCONT
assign letter=$letter
"@
    try { Fill "${letter}:\" }
    finally {
        RunDiskpart @"
select vdisk file="$file"
detach vdisk
"@
    }
    Pack $file
}

# Hyper-V's own
if (Get-Command New-VHD -ErrorAction SilentlyContinue) {
    $file = Join-Path $work 'win-hv.vhdx'
    New-VHD -Path $file -Dynamic -SizeBytes 64MB | Out-Null
    $disk = Mount-VHD -Path $file -Passthru | Initialize-Disk -PartitionStyle GPT -PassThru
    try {
        $vol = $disk | New-Partition -UseMaximumSize -AssignDriveLetter | Format-Volume -FileSystem NTFS -NewFileSystemLabel AQRCONT -Confirm:$false
        Fill "$($vol.DriveLetter):\"
    }
    finally { Dismount-VHD -Path $file }
    Pack $file
} else {
    $report += 'win-hv.vhdx : skipped (no Hyper-V PowerShell module)'
}

Remove-Item -Recurse -Force $work
$report
