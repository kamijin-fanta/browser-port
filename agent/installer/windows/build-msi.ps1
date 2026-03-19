param(
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"

Write-Host "[BrowserPort] Building release binary for $Target"
cargo build --release --target $Target

if (-not (Get-Command cargo-wix -ErrorAction SilentlyContinue)) {
    Write-Host "[BrowserPort] Installing cargo-wix"
    cargo install cargo-wix
}

Write-Host "[BrowserPort] Building MSI (unsigned)"
cargo wix --target $Target --nocapture
