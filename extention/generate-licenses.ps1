param(
    [string]$OutputPath = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = (Resolve-Path (Join-Path $ScriptDir "..")).Path
$AgentManifestPath = Join-Path $ProjectRoot "agent/Cargo.toml"

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $ScriptDir "licenses-third-party.json"
}

function Convert-ToProjectRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$AbsolutePath
    )

    $rootPath = (Resolve-Path $ProjectRoot).Path
    $targetPath = (Resolve-Path $AbsolutePath).Path
    $method = [System.IO.Path].GetMethod(
        "GetRelativePath",
        [Type[]]@([string], [string])
    )
    if ($method) {
        return [System.IO.Path]::GetRelativePath($rootPath, $targetPath)
    }

    # Fallback for older PowerShell/.NET runtimes without Path.GetRelativePath.
    $sep = [System.IO.Path]::DirectorySeparatorChar
    $rootFullPath = [System.IO.Path]::GetFullPath($rootPath)
    if (-not $rootFullPath.EndsWith($sep)) {
        $rootFullPath += $sep
    }
    $targetFullPath = [System.IO.Path]::GetFullPath($targetPath)

    $rootUriText = "file:///" + ($rootFullPath -replace "\\", "/").TrimStart("/")
    $targetUriText = "file:///" + ($targetFullPath -replace "\\", "/").TrimStart("/")
    $rootUri = [System.Uri]$rootUriText
    $targetUri = [System.Uri]$targetUriText
    $relativeUri = $rootUri.MakeRelativeUri($targetUri)

    return [System.Uri]::UnescapeDataString($relativeUri.ToString()).Replace("/", $sep)
}

function Invoke-CargoMetadata {
    param(
        [Parameter(Mandatory = $true)][string]$ManifestPath
    )

    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        return $null
    }

    $baseArgs = @(
        "metadata",
        "--format-version", "1",
        "--manifest-path", $ManifestPath,
        "--locked"
    )

    foreach ($extraArgs in @(@("--offline"), @())) {
        $allArgs = @($baseArgs + $extraArgs)
        try {
            $json = & cargo @allArgs 2>$null
            if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($json)) {
                return $json | ConvertFrom-Json
            }
        } catch {
            # Retry without --offline if needed.
        }
    }

    return $null
}

function Get-RustThirdPartyPackages {
    param(
        [Parameter(Mandatory = $true)]$Metadata
    )

    if (-not $Metadata.resolve -or -not $Metadata.resolve.root) {
        return @()
    }

    $packageById = @{}
    foreach ($package in $Metadata.packages) {
        $packageById[$package.id] = $package
    }

    $nodeById = @{}
    foreach ($node in $Metadata.resolve.nodes) {
        $nodeById[$node.id] = $node
    }

    $queue = [System.Collections.Generic.Queue[string]]::new()
    $visited = [System.Collections.Generic.HashSet[string]]::new()
    $queue.Enqueue([string]$Metadata.resolve.root)

    $rows = New-Object System.Collections.Generic.List[object]

    while ($queue.Count -gt 0) {
        $id = $queue.Dequeue()
        if (-not $visited.Add($id)) {
            continue
        }

        if ($id -ne $Metadata.resolve.root -and $packageById.ContainsKey($id)) {
            $pkg = $packageById[$id]
            if ($pkg.source) {
                $rows.Add([pscustomobject][ordered]@{
                        name = [string]$pkg.name
                        version = [string]$pkg.version
                        license = if ([string]::IsNullOrWhiteSpace([string]$pkg.license)) { "UNKNOWN" } else { [string]$pkg.license }
                        repository = [string]$pkg.repository
                        homepage = [string]$pkg.homepage
                        source = [string]$pkg.source
                    })
            }
        }

        if ($nodeById.ContainsKey($id)) {
            foreach ($depId in $nodeById[$id].dependencies) {
                $queue.Enqueue([string]$depId)
            }
        }
    }

    return $rows |
        Sort-Object name, version -Unique
}

function Get-ExtensionNpmPackages {
    $packageLockPath = Join-Path $ScriptDir "package-lock.json"
    if (-not (Test-Path $packageLockPath)) {
        return @()
    }

    $lock = Get-Content -Raw $packageLockPath | ConvertFrom-Json
    if (-not $lock.packages) {
        return @()
    }

    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($property in $lock.packages.PSObject.Properties) {
        if ($property.Name -eq "") {
            continue
        }
        $entry = $property.Value
        $name = if ($entry.name) {
            [string]$entry.name
        } else {
            Split-Path -Leaf $property.Name
        }

        $rows.Add([pscustomobject][ordered]@{
                name = $name
                version = if ($entry.version) { [string]$entry.version } else { "UNKNOWN" }
                license = if ($entry.license) { [string]$entry.license } else { "UNKNOWN" }
                source = "npm"
            })
    }

    return $rows |
        Sort-Object name, version -Unique
}

function Resolve-NativeLicenseType {
    param(
        [Parameter(Mandatory = $true)][string]$LicenseFilePath
    )

    try {
        $snippet = (Get-Content -Path $LicenseFilePath -TotalCount 80 -ErrorAction Stop) -join " "
    } catch {
        return "SEE LICENSE FILE"
    }

    $normalized = $snippet.ToLowerInvariant()
    if ($normalized -match "mit license") {
        return "MIT"
    }
    if ($normalized -match "bsd") {
        return "BSD-like"
    }
    if ($normalized -match "gnu") {
        return "GNU-family"
    }
    return "SEE LICENSE FILE"
}

function Get-NativeBundledLibraries {
    $knownVendors = @(
        [ordered]@{
            name = "Spout SDK"
            licenseFile = Join-Path $ProjectRoot "agent/native/spout/SPOUTSDK/LICENSE"
            homepage = "https://github.com/leadedge/Spout2"
        },
        [ordered]@{
            name = "Syphon Framework"
            licenseFile = Join-Path $ProjectRoot "agent/native/syphon/Syphon-Framework/License.txt"
            homepage = "https://github.com/Syphon/Syphon-Framework"
        }
    )

    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($vendor in $knownVendors) {
        if (-not (Test-Path $vendor.licenseFile)) {
            continue
        }
        $rows.Add([pscustomobject][ordered]@{
                name = [string]$vendor.name
                license = Resolve-NativeLicenseType -LicenseFilePath $vendor.licenseFile
                licenseFile = Convert-ToProjectRelativePath -AbsolutePath $vendor.licenseFile
                homepage = [string]$vendor.homepage
            })
    }
    return $rows
}

$cargoMetadata = $null
if (Test-Path $AgentManifestPath) {
    $cargoMetadata = Invoke-CargoMetadata -ManifestPath $AgentManifestPath
}

$rustPackages = @()
if ($cargoMetadata) {
    $rustPackages = Get-RustThirdPartyPackages -Metadata $cargoMetadata
}

$npmPackages = Get-ExtensionNpmPackages
$nativeLibraries = Get-NativeBundledLibraries
$rustPackageList = @($rustPackages)
$npmPackageList = @($npmPackages)
$nativeLibraryList = @($nativeLibraries)

$notices = @()
if (-not $cargoMetadata) {
    $notices += "Rust dependency metadata could not be collected automatically (cargo not found or metadata failed)."
}
if ($npmPackageList.Count -eq 0) {
    $notices += "No npm package-lock.json was found for the Chrome extension."
}

$payload = [ordered]@{
    generatedAtUtc = (Get-Date).ToUniversalTime().ToString("o")
    project = [ordered]@{
        name = "browser-port"
        license = "AGPL-3.0-only"
        licenseTextUrl = "https://www.gnu.org/licenses/agpl-3.0.txt"
        licenseFile = "LICENSE"
    }
    rustCrates = $rustPackageList
    extensionNpmPackages = $npmPackageList
    nativeBundledLibraries = $nativeLibraryList
    notices = @($notices)
}

New-Item -ItemType Directory -Path (Split-Path -Parent $OutputPath) -Force | Out-Null
$payload |
    ConvertTo-Json -Depth 8 |
    Set-Content -Path $OutputPath -Encoding UTF8

Write-Host "[BrowserPort] Generated third-party license data: $OutputPath"
