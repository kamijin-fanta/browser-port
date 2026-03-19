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

Optional bind override:

```powershell
$env:BROWSER_PORT_AGENT_BIND = "127.0.0.1:9876"
cargo run
```

## Output helper control

`browser-port-control` messages can toggle helper processes:

- `set-output` (`output=spout|syphon|ndi`, `enabled=true|false`)
- `toggle-output`

BrowserPort launches the same executable as:

```powershell
browser-port output-helper --mode <spout|syphon|ndi> --ws ws://127.0.0.1:9876
```

Override command per output:

- `BROWSER_PORT_SPOUT_HELPER_CMD`
- `BROWSER_PORT_SYPHON_HELPER_CMD`
- `BROWSER_PORT_NDI_HELPER_CMD`

Runtime dependency notes:

- `spout` (Windows): built from the `leadedge/Spout2` submodule in `agent/native/spout/SPOUTSDK`
  (SDK sources are under `SPOUTSDK/SpoutGL` inside the submodule)
- `syphon` (macOS): links to Syphon framework (`agent/native/syphon/syphon_bridge.mm`)
- `ndi`: uses Rust `ndi` crate and requires NDI Runtime installed on host

## Installer scaffolding

- Windows MSI script: `installer/windows/build-msi.ps1`
- macOS PKG script: `installer/macos/build-pkg.sh`

These scripts are unsigned defaults for local/internal distribution.
