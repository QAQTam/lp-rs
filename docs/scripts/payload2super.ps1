# payload2super.ps1 — assemble super.img from an OTA partition dump using a device template
#
#   .\payload2super.ps1 -Config ..\devices\cph2487.json `
#       -ImagesDir .\extracted_partitions -Out .\super_new.img
#
# Pre-flight: every image referenced by the template must exist (OTAs often
# skip my_* partitions - drop stock dumps of the missing ones into -ImagesDir
# first). Post-build: verifies every _a extent size in the produced image
# against the source files. Pass -VerifyOnly to re-check an existing image.
param(
    [Parameter(Mandatory = $true)][string]$Config,
    [string]$ImagesDir = ".",
    [string]$Out = "",
    [switch]$VerifyOnly
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$lpmake  = Join-Path $repoRoot "target\release\lpmake.exe"
$lpdump  = Join-Path $repoRoot "target\release\lpdump.exe"

if (-not (Test-Path $lpmake))  { $lpmake  = "lpmake" }
if (-not (Test-Path $lpdump))  { $lpdump  = "lpdump" }
if (-not $Out) { Write-Error "-Out is required (build target / image to verify)" }

if (-not $VerifyOnly) {
    $cfg = Get-Content $Config -Raw | ConvertFrom-Json
    $missing = @()
    foreach ($p in $cfg.partitions) {
        if ($p.image) {
            $img = if ([System.IO.Path]::IsPathRooted($p.image)) { $p.image } else { Join-Path $ImagesDir $p.image }
            if (-not (Test-Path $img)) { $missing += "$($p.name) -> $img" }
        }
    }
    if ($missing.Count) {
        Write-Host "ERROR: missing partition images:" -ForegroundColor Red
        $missing | ForEach-Object { Write-Host "  $_" }
        Write-Host "Hint: OTAs do not always update every my_* partition - drop the"
        Write-Host "      stock images for the missing ones into $ImagesDir first."
        exit 1
    }

    Write-Host "==> lpmake --config $(Split-Path -Leaf $Config)"
    & $lpmake --config $Config --images-dir $ImagesDir --sparse -o $Out
    if ($LASTEXITCODE -ne 0) { Write-Error "lpmake failed"; exit 1 }
}

Write-Host "==> verify $Out against $ImagesDir"
$meta = & $lpdump --json $Out | ConvertFrom-Json
$devSize = [int64]$meta.block_devices[0].size
$ok = $true; $sum = [int64]0
foreach ($p in $meta.partitions) {
    if ($p.name -notmatch "_a$") { continue }
    if ($p.num_extents -eq 0) { Write-Warning "$($p.name): zero-size in image"; $ok = $false; continue }
    $imgSize = (Get-Item (Join-Path $ImagesDir ($p.name -replace '_a$', '.img'))).Length
    $metaSize = [int64]$meta.extents[$p.first_extent_index].num_sectors * 512
    $sum += $metaSize
    if ($metaSize -ne $imgSize) {
        Write-Host ("FAIL {0}: meta={1} file={2}" -f $p.name, $metaSize, $imgSize) -ForegroundColor Red
        $ok = $false
    }
}
$groupMax = [int64](($meta.groups | Where-Object { $_.name -match "_a$" } | Select-Object -First 1).maximum_size)
Write-Host ("total _a content = {0:N0} bytes | group max = {1:N0} | device = {2:N0}" -f $sum, $groupMax, $devSize)
if ($ok) { Write-Host "VERIFY OK - $Out is consistent" -ForegroundColor Green } else { exit 1 }
