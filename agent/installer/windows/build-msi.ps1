param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutputDir = "",
    [string]$Version = "",
    [string]$ManifestVersion = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$AgentDir = Resolve-Path (Join-Path $ScriptDir "..\..")
$cargoTomlPath = Join-Path $AgentDir "Cargo.toml"
$cargoLockPath = Join-Path $AgentDir "Cargo.lock"

function Get-CargoPackageVersion {
    param([string]$Path)
    $versionLine = Select-String -Path $Path -Pattern '^version\s*=\s*"(.*)"' | Select-Object -First 1
    if (-not $versionLine) {
        throw "Could not determine package version from $Path"
    }
    return $versionLine.Matches[0].Groups[1].Value
}

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $AgentDir "target\dist"
}

$cargoVersion = Get-CargoPackageVersion -Path $cargoTomlPath
if ([string]::IsNullOrWhiteSpace($ManifestVersion)) {
    $ManifestVersion = $cargoVersion
}
if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = $ManifestVersion
}
if ($ManifestVersion -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') {
    throw "ManifestVersion must be numeric semver-ish (for example: 0.1.0): $ManifestVersion"
}
if ([string]::IsNullOrWhiteSpace($env:BROWSER_PORT_APP_VERSION)) {
    $env:BROWSER_PORT_APP_VERSION = $Version
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
$cargoTomlBackupPath = $null
$cargoLockBackupPath = $null
Push-Location $AgentDir
try {
    if ($ManifestVersion -ne $cargoVersion) {
        Write-Host "[BrowserPort] Temporarily syncing Cargo.toml version to $ManifestVersion for MSI metadata"
        $cargoTomlBackupPath = [System.IO.Path]::GetTempFileName()
        Copy-Item -Path $cargoTomlPath -Destination $cargoTomlBackupPath -Force

        $cargoTomlLines = Get-Content -Path $cargoTomlPath
        $updated = $false
        $inPackageSection = $false
        for ($i = 0; $i -lt $cargoTomlLines.Count; $i++) {
            $line = [string]$cargoTomlLines[$i]
            if ($line -match '^\s*\[package\]\s*$') {
                $inPackageSection = $true
                continue
            }
            if ($inPackageSection -and $line -match '^\s*\[.+\]\s*$') {
                break
            }
            if ($inPackageSection -and $line -match '^\s*version\s*=\s*".*"\s*$') {
                $indent = ""
                if ($line -match '^(\s*)') {
                    $indent = $Matches[1]
                }
                $cargoTomlLines[$i] = "$indent" + "version = ""$ManifestVersion"""
                $updated = $true
                break
            }
        }

        if (-not $updated) {
            throw "Failed to update version in $cargoTomlPath"
        }
        Set-Content -Path $cargoTomlPath -Value $cargoTomlLines

        if (Test-Path $cargoLockPath) {
            $cargoLockBackupPath = [System.IO.Path]::GetTempFileName()
            Copy-Item -Path $cargoLockPath -Destination $cargoLockBackupPath -Force
        }
    }

    if (-not $SkipBuild) {
        Write-Host "[BrowserPort] Building release binary for $Target"
        cargo build --release --bin browser-port --target $Target
    }

    if (-not (Get-Command cargo-wix -ErrorAction SilentlyContinue)) {
        Write-Host "[BrowserPort] Installing cargo-wix"
        cargo install cargo-wix
    }

    if (-not (Get-Command candle.exe -ErrorAction SilentlyContinue)) {
        throw @"
WiX Toolset (candle.exe/light.exe) was not found in PATH.
Install WiX v3 before running this script.
Example (admin shell): choco install wixtoolset -y
"@
    }

    $wixDir = Join-Path $AgentDir "wix"
    $wxsFiles = @(Get-ChildItem -Path $wixDir -Filter "*.wxs" -File -ErrorAction SilentlyContinue)
    if ($wxsFiles.Count -eq 0) {
        Write-Host "[BrowserPort] Initializing WiX sources"
        cargo wix init

        $wxsFiles = @(Get-ChildItem -Path $wixDir -Filter "*.wxs" -File -ErrorAction SilentlyContinue)
        if ($wxsFiles.Count -eq 0) {
            throw "WiX source files were not generated under wix\\"
        }
    }

    Write-Host "[BrowserPort] Building MSI (unsigned)"
    cargo wix --target $Target --nocapture

    $msi = Get-ChildItem -Path (Join-Path $AgentDir "target\wix\*.msi") -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $msi) {
        throw "MSI was not generated under target\\wix"
    }

    $installerName = "browser-port-$Version-$Target-unsigned.msi"
    $installerOut = Join-Path $OutputDir $installerName
    Copy-Item -Path $msi.FullName -Destination $installerOut -Force

    $standaloneSrc = Join-Path $AgentDir "target\$Target\release\browser-port.exe"
    if (Test-Path $standaloneSrc) {
        $standaloneOut = Join-Path $OutputDir "browser-port-$Version-$Target.exe"
        Copy-Item -Path $standaloneSrc -Destination $standaloneOut -Force
    } else {
        Write-Warning "[BrowserPort] Standalone executable not found: $standaloneSrc"
    }

    Write-Host "[BrowserPort] Installer: $installerOut"
} finally {
    if ($cargoTomlBackupPath) {
        Copy-Item -Path $cargoTomlBackupPath -Destination $cargoTomlPath -Force
        Remove-Item -Path $cargoTomlBackupPath -Force -ErrorAction SilentlyContinue
    }
    if ($cargoLockBackupPath) {
        Copy-Item -Path $cargoLockBackupPath -Destination $cargoLockPath -Force
        Remove-Item -Path $cargoLockBackupPath -Force -ErrorAction SilentlyContinue
    }
    Pop-Location
}
