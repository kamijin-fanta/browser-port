# BrowserPort

Rust WebSocket relay for BrowserPort v1.

## Features

- Single-port WebSocket relay for `role=browser-port-extension` and `role=client`
- Handshake and protocol version validation (`protocolVersion=1`)
- Player-aware routing for control/search commands
- Binary chunk relay (`video-chunk`, `audio-chunk`)
- Output helper manager for `spout`, `syphon`, `ndi` (start/stop via `browser-port-control`)
- Output helper is built into the same Rust binary (`output-helper` subcommand)

## Run

```powershell
cd agent
cargo run
```

On macOS, launching the bundled `.app` opens BrowserPort as a menu bar app.
The tray shows player count, Syphon client count, WS address, and a quit action.

Optional bind override:

```powershell
$env:BROWSER_PORT_AGENT_BIND = "127.0.0.1:1844"
cargo run
```

## Output helper control

`browser-port-control` messages can toggle helper processes:

- `set-output` (`output=spout|syphon|ndi`, `enabled=true|false`)
- `toggle-output`

On macOS, `spout` is not supported. Use `output=syphon`.

BrowserPort launches the same executable as:

```powershell
browser-port output-helper --mode <spout|syphon|ndi> --ws ws://127.0.0.1:1844 [--parent-pid <pid>]
```

Override command per output:

- `BROWSER_PORT_SPOUT_HELPER_CMD`
- `BROWSER_PORT_SYPHON_HELPER_CMD`
- `BROWSER_PORT_NDI_HELPER_CMD`

Runtime dependency notes:

- `spout` (Windows): built from the `leadedge/Spout2` submodule in `agent/native/spout/SPOUTSDK`
  (SDK sources are under `SPOUTSDK/SpoutGL` inside the submodule)
- `syphon` (macOS): uses dynamic Syphon runtime classes via `agent/native/syphon/syphon_bridge.mm`
  (runtime still requires Syphon framework to be installed)
- `ndi`: uses Rust `ndi` crate and requires NDI Runtime installed on host
  - On macOS, BrowserPort dynamically loads `libndi` at runtime.
  - If runtime is missing, NDI output is reported as unavailable and cannot be enabled.
  - Optional override: `BROWSER_PORT_NDI_LIBRARY_PATH=/absolute/path/to/libndi.dylib`

### Embed Syphon.framework (macOS)

Build and embed the framework from submodule:

```bash
cd agent
./scripts/embed_syphon_framework.sh
```

The script copies `Syphon.framework` to:

- `agent/target/debug/Frameworks/Syphon.framework`
- `agent/target/release/Frameworks/Syphon.framework`

Runtime load override:

- `BROWSER_PORT_SYPHON_FRAMEWORK_PATH=/absolute/path/to/Syphon.framework`

## Output self-check helpers

Platform helper binaries are available for autonomous validation:

- Windows:
  - `cargo run --bin spout_selftest`
  - `cargo run --bin spout_probe -- <sender_name> <timeout_sec> <output_path>`
- macOS:
  - `cargo run --bin syphon_selftest`
  - `cargo run --bin syphon_probe -- <server_name> <timeout_sec> <output_path>`

Unified entrypoint:

- `cargo run --bin output_selfcheck`

`output_selfcheck` exits non-zero on failure, so it is suitable for coding-agent or CI smoke checks.

## Installer scaffolding

- Windows MSI script: `installer/windows/build-msi.ps1`
- macOS PKG script: `installer/macos/build-pkg.sh`
- macOS DMG script: `installer/macos/build-dmg.sh`

These scripts are unsigned defaults for local/internal distribution.
