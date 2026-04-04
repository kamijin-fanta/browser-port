# BrowserPort

BrowserPort is a desktop relay + Chrome extension pair for streaming browser media from Chrome tabs to local outputs.

- Agent (`agent/`): Rust WebSocket relay and output helper manager
- Chrome extension (`extention/`): capture/control UI and relay client

The default local endpoint is `ws://127.0.0.1:1844`.

## For Users

### 1. Download build artifacts

Every commit triggers a cross-platform build on GitHub Actions:

- Workflow: `.github/workflows/build-artifacts.yml`
- Artifacts:
  - `browser-port-windows`
  - `browser-port-linux`
  - `browser-port-macos`

Each artifact includes:

- Installer package (OS-specific)
- Standalone executable/binary
- Chrome extension zip

### 2. Install or run BrowserPort agent

#### Windows

- Installer: `browser-port-<version>-x86_64-pc-windows-msvc-unsigned.msi`
- Standalone: `browser-port-<version>-x86_64-pc-windows-msvc.exe`

#### macOS

- Installer: `browser-port-<version>-aarch64-apple-darwin-unsigned.dmg`
- Standalone: `browser-port-<version>-aarch64-apple-darwin`

#### Linux

- Installer tarball: `browser-port-<version>-x86_64-unknown-linux-gnu-linux-installer.tar.gz`
- Standalone: `browser-port-<version>-x86_64-unknown-linux-gnu`

Install from tarball:

```bash
tar -xzf browser-port-<version>-x86_64-unknown-linux-gnu-linux-installer.tar.gz
sudo ./install.sh
```

### 3. Load Chrome extension

1. Open `chrome://extensions`
2. Enable Developer mode
3. Choose one:
   - Load unpacked: select `extention/`
   - Packed artifact: unzip `browser-port-chrome-extension-<version>.zip`, then load unpacked from the extracted directory

### 4. Start the agent

- Windows/macOS: app starts as tray/menu-bar style background app
- Linux: run from terminal

Default bind address:

- `ws://127.0.0.1:1844`

## For Developers

### Prerequisites

- Rust stable toolchain
- Git submodules
- Platform dependencies:
  - Windows MSI build: WiX Toolset v3 (`candle.exe`, `light.exe`)
  - macOS installer build: `hdiutil`, `sips`, `iconutil`

### Clone

```bash
git clone --recurse-submodules <repo-url>
cd browser-port
```

If already cloned:

```bash
git submodule update --init --recursive
```

### Run locally

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

### Windows (MSI + standalone)

```powershell
cd agent
.\installer\windows\build-msi.ps1 -Target x86_64-pc-windows-msvc -OutputDir "$PWD\target\dist"
```

### macOS (DMG + standalone)

```bash
cd agent
OUTPUT_DIR="$PWD/target/dist" TARGET_TRIPLE="$(rustc -vV | awk '/host:/ {print $2}')" ./installer/macos/build-dmg.sh
```

### Linux (installer tar + standalone)

```bash
cd agent
OUTPUT_DIR="$PWD/target/dist" TARGET_TRIPLE="$(rustc -vV | awk '/host:/ {print $2}')" ./installer/linux/build-tar.sh
```

### Chrome extension zip

```powershell
./extention/package-extension.ps1 -OutputDir "$PWD/extention/target"
```

## CI Workflow

`build-artifacts.yml` runs on `push`, `pull_request`, and manual dispatch.

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
