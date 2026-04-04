# Contributing

## Prerequisites

- Rust stable toolchain
- Git submodules
- Platform dependencies:
  - Windows MSI build: WiX Toolset v3 (`candle.exe`, `light.exe`)
  - macOS installer build: `hdiutil`, `sips`, `iconutil`

## Clone

```bash
git clone --recurse-submodules <repo-url>
cd browser-port
```

If already cloned:

```bash
git submodule update --init --recursive
```

## Run locally

```bash
cd agent
cargo run --bin browser-port
```

Optional bind override:

```bash
BROWSER_PORT_AGENT_BIND=127.0.0.1:1844 cargo run --bin browser-port
```

PowerShell:

```powershell
$env:BROWSER_PORT_AGENT_BIND = "127.0.0.1:1844"
cargo run --bin browser-port
```

## Build Artifacts Locally

Resolve versions from git state first:

```bash
./scripts/versioning/resolve-version
```

### Windows (MSI + standalone)

```powershell
cd agent
.\installer\windows\build-msi.ps1 `
  -Target x86_64-pc-windows-msvc `
  -OutputDir "$PWD\target\dist" `
  -Version "<full-version>" `
  -ManifestVersion "<release-version>"
```

### macOS (DMG + standalone)

```bash
cd agent
OUTPUT_DIR="$PWD/target/dist" \
TARGET_TRIPLE="$(rustc -vV | awk '/host:/ {print $2}')" \
VERSION="<full-version>" \
MANIFEST_VERSION="<release-version>" \
APP_VERSION="<release-version>" \
./installer/macos/build-dmg.sh
```

### Linux (installer tar + standalone)

```bash
cd agent
OUTPUT_DIR="$PWD/target/dist" \
TARGET_TRIPLE="$(rustc -vV | awk '/host:/ {print $2}')" \
VERSION="<full-version>" \
MANIFEST_VERSION="<release-version>" \
./installer/linux/build-tar.sh
```

### Chrome extension zip

```powershell
./extention/package-extension.ps1 `
  -OutputDir "$PWD/extention/target" `
  -Version "<full-version>" `
  -ManifestVersion "<release-version>" `
  -VersionName "<full-version for dev only>"
```

## Versioning and Releases

- Single source of truth is `git tag` in `vMAJOR.MINOR.PATCH` format.
- Release tag on `HEAD`:
  - `RELEASE_VERSION = X.Y.Z`
  - `FULL_VERSION = X.Y.Z`
- Non-tag commit:
  - nearest release tag is used as base
  - `FULL_VERSION = X.Y.Z-dev.<N>+g<sha>`
- If no release tag exists, baseline `v0.1.0` is used.

Resolver script:

```bash
./scripts/versioning/resolve-version
```

Outputs fixed keys:

- `RELEASE_VERSION`
- `FULL_VERSION`
- `IS_TAG_RELEASE`
- `TAG_NAME`
- `COMMITS_SINCE_TAG`
- `SHORT_SHA`

Chrome extension packaging policy:

- `manifest.json` in repository is not edited.
- Staged extension manifest always sets `version=<release-version>`.
- Dev build only: staged manifest adds `version_name=<full-version>`.

Release operation:

1. Create and push tag: `git tag vX.Y.Z && git push origin vX.Y.Z`
2. CI validates tag format (`^v[0-9]+\.[0-9]+\.[0-9]+$`)
3. CI uploads matrix artifacts and publishes GitHub Release for that tag

## CI Workflow

`build-artifacts.yml` runs on `push`, `pull_request`, and manual dispatch.
`v*` tag push additionally triggers GitHub Release publishing.

Matrix targets:

- `windows-latest` (`x86_64-pc-windows-msvc`)
- `ubuntu-latest` (`x86_64-unknown-linux-gnu`)
- `macos-14` (`aarch64-apple-darwin`)

Per job outputs uploaded as an artifact:

- installer package
- standalone binary
- `browser-port-chrome-extension-<version>.zip`

## Repository Layout

- `agent/`: Rust relay/output helper
- `extention/`: Chrome extension sources (directory name kept as `extention`)
- `agent/installer/windows/`: Windows installer build script
- `agent/installer/macos/`: macOS installer build scripts
- `agent/installer/linux/`: Linux installer tarball build script
- `scripts/versioning/`: git tag driven version resolution scripts
- `.github/workflows/`: CI workflows

## Runtime Notes

- Windows launches as tray app by default.
- macOS launches as menu-bar app by default.
- macOS bundled app registers itself as a login item via `SMAppService` on launch.
- Set `BROWSER_PORT_HEADLESS=true` to disable tray/menu-bar startup mode.
- Set `BROWSER_PORT_TRAY=true` to force tray/menu-bar mode when supported.

## Troubleshooting

- Tray icon present but no process:
  - This is usually a stale shell icon cache entry.
  - Restart Explorer (Windows) or relaunch the app.
- Second launch does not open another tray icon on Windows:
  - Expected behavior (single-instance guard enabled).
- Extension shows disconnected:
  - Verify agent is running.
  - Verify WebSocket URL in extension settings (`ws://127.0.0.1:1844` by default).
