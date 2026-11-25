# flash_super.ps1 — flash an assembled super.img + the static partitions of an OTA dump.
#
#   .\flash_super.ps1 -SuperImage .\super_new.img `
#       -ImagesDir .\extracted_partitions -Config ..\devices\cph2487.json
#
# Dynamic partition names come from the device template; every OTHER *.img in
# -ImagesDir is treated as a static (bootloader-side) partition and flashed
# with the current slot suffix. super itself is flashed ONCE.
# Run from a PC with fastboot on PATH; the device will be rebooted to
# fastbootd automatically if it is in bootloader fastboot.
param(
    [string]$SuperImage,
    [Parameter(Mandatory = $true)][string]$ImagesDir,
    [Parameter(Mandatory = $true)][string]$Config,
    [switch]$SkipSuper,
    [switch]$SkipStatic,
    [switch]$DryRun
)

$ErrorActionPreference = "Continue"

function Invoke-Fastboot {
    param([string[]]$FbArgs)
    if ($DryRun) { Write-Host "DRY: fastboot $($FbArgs -join ' ')"; return @{ Code = 0; Output = "dry-run" } }
    $out = & fastboot @FbArgs 2>&1
    return @{ Code = $LASTEXITCODE; Output = ($out | Out-String).Trim() }
}

$userspace = Invoke-Fastboot @("getvar", "is-userspace")
if (-not $DryRun -and $userspace.Output -notmatch "is-userspace:\s*yes") {
    Write-Host "Rebooting device into fastbootd..."
    $rb = Invoke-Fastboot @("reboot", "fastboot")
    Write-Host $rb.Output
    if ($rb.Code -ne 0) { exit $rb.Code }
    $userspace = Invoke-Fastboot @("getvar", "is-userspace")
}
if (-not $DryRun -and $userspace.Output -notmatch "is-userspace:\s*yes") {
    Write-Error "Device not in fastbootd: $($userspace.Output)"
    exit 1
}

$slotRes = Invoke-Fastboot @("getvar", "current-slot")
$slot = "a"
if ($slotRes.Output -match "current-slot:\s*([ab])") { $slot = $Matches[1] }
Write-Host "fastbootd | slot: $slot"

# Dynamic base names (strip slot suffix) derived from the template.
$cfg = Get-Content $Config -Raw | ConvertFrom-Json
$dynamic = @($cfg.partitions | ForEach-Object { $_.name -replace '_[ab]$', '' } | Sort-Object -Unique)

if ($SkipSuper) {
    Write-Host "==> Skipping super flash (-SkipSuper)"
} else {
    Write-Host "==> super (one shot, replaces $($dynamic.Count) logical partitions)"
    if (-not $SuperImage -or -not (Test-Path $SuperImage)) { Write-Error "Missing -SuperImage (or pass -SkipSuper)"; exit 1 }
    $res = Invoke-Fastboot @("flash", "super", (Resolve-Path $SuperImage).Path)
    Write-Host $res.Output
    if ($res.Code -ne 0) { Write-Error "flash super failed"; exit 1 }
}

if (-not $SkipStatic) {
    $failed = @()
    Get-ChildItem $ImagesDir -Filter *.img | ForEach-Object {
        $base = $_.BaseName
        if ($dynamic -contains $base) { return }          # lives inside super, already done
        $target = "${base}_${slot}"
        $r = Invoke-Fastboot @("flash", $target, $_.FullName)
        if ($r.Code -eq 0) { Write-Host "  [OK] $target" }
        else { Write-Warning "  [FAIL] $target : $($r.Output)"; $failed += $target }
    }
    if ($failed.Count) {
        Write-Warning "Not flashed (missing partition on device? skip): $($failed -join ', ')"
    }
}

Write-Host "=== Done. Next: fastboot --set-active=$slot ; fastboot reboot ==="
