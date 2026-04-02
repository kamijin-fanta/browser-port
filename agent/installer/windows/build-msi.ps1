param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutputDir = "",
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$AgentDir = Resolve-Path (Join-Path $ScriptDir "..\..")

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $AgentDir "target\dist"
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $cargoTomlPath = Join-Path $AgentDir "Cargo.toml"
    $versionLine = Select-String -Path $cargoTomlPath -Pattern '^version\s*=\s*"(.*)"' | Select-Object -First 1
    if (-not $versionLine) {
        throw "Could not determine package version from $cargoTomlPath"
    }
    $Version = $versionLine.Matches[0].Groups[1].Value
}

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
Push-Location $AgentDir
try {
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
        cargo wix init --nocapture

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
    Pop-Location
}
