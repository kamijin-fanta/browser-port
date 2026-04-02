param(
    [string]$OutputDir = "",
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $ScriptDir "target"
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $manifest = Get-Content (Join-Path $ScriptDir "manifest.json") -Raw | ConvertFrom-Json
    $Version = [string]$manifest.version
}

$zipPath = Join-Path $OutputDir "browser-port-chrome-extension-$Version.zip"
$stageDir = Join-Path $OutputDir "__chrome_extension_stage"

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
if (Test-Path $stageDir) {
    Remove-Item -Recurse -Force $stageDir
}
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

Get-ChildItem -Path $ScriptDir -Force |
    Where-Object {
        $_.Name -notin @("target", "__chrome_extension_stage", "package-extension.ps1")
    } |
    ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $stageDir -Recurse -Force
    }

if (Test-Path $zipPath) {
    Remove-Item -Force $zipPath
}
Compress-Archive -Path (Join-Path $stageDir "*") -DestinationPath $zipPath
Remove-Item -Recurse -Force $stageDir

Write-Host "[BrowserPort] Created extension zip: $zipPath"
