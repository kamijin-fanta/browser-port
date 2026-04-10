param(
    [string]$OutputDir = "",
    [string]$Version = "",
    [string]$ManifestVersion = "",
    [string]$VersionName = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $ScriptDir "target"
}

$licensesScriptPath = Join-Path $ScriptDir "generate-licenses.ps1"

$manifest = Get-Content (Join-Path $ScriptDir "manifest.json") -Raw | ConvertFrom-Json

if ([string]::IsNullOrWhiteSpace($ManifestVersion)) {
    $ManifestVersion = [string]$manifest.version
}
if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = $ManifestVersion
}

if ($ManifestVersion -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') {
    throw "ManifestVersion must be numeric semver-ish (for example: 0.1.0): $ManifestVersion"
}

$zipPath = Join-Path $OutputDir "browser-port-chrome-extension-$Version.zip"
$stageDir = Join-Path $OutputDir "__chrome_extension_stage"

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
if (Test-Path $stageDir) {
    Remove-Item -Recurse -Force $stageDir
}
New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

$excludeNames = @(
    "target",
    "__chrome_extension_stage",
    "package-extension.ps1",
    "generate-licenses.ps1"
)
$resolvedOutputDir = (Resolve-Path $OutputDir).Path
$outputLeafName = Split-Path -Leaf $resolvedOutputDir
if (-not [string]::IsNullOrWhiteSpace($outputLeafName)) {
    $excludeNames += $outputLeafName
}

Get-ChildItem -Path $ScriptDir -Force |
    Where-Object {
        $_.Name -notin $excludeNames
    } |
    ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $stageDir -Recurse -Force
    }

if (Test-Path $licensesScriptPath) {
    & $licensesScriptPath -OutputPath (Join-Path $stageDir "licenses-third-party.json")
}

$stagedManifestPath = Join-Path $stageDir "manifest.json"
$stagedManifest = Get-Content $stagedManifestPath -Raw | ConvertFrom-Json
$stagedManifest.version = $ManifestVersion

if ([string]::IsNullOrWhiteSpace($VersionName)) {
    if ($stagedManifest.PSObject.Properties.Name -contains "version_name") {
        $stagedManifest.PSObject.Properties.Remove("version_name")
    }
} else {
    if ($stagedManifest.PSObject.Properties.Name -contains "version_name") {
        $stagedManifest.version_name = $VersionName
    } else {
        $stagedManifest | Add-Member -MemberType NoteProperty -Name "version_name" -Value $VersionName
    }
}

$stagedManifest | ConvertTo-Json -Depth 16 | Set-Content -Path $stagedManifestPath

if (Test-Path $zipPath) {
    Remove-Item -Force $zipPath
}
Compress-Archive -Path (Join-Path $stageDir "*") -DestinationPath $zipPath
Remove-Item -Recurse -Force $stageDir

Write-Host "[BrowserPort] Created extension zip: $zipPath"
