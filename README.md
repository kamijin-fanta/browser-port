# BrowserPort

This directory is the standalone BrowserPort repo.

## Layout

- `agent/`: Rust WebSocket relay and output helper
- `extention/`: Chrome extension frontend
- `agent/native/spout/SPOUTSDK`: `leadedge/Spout2` submodule
- `agent/native/spout/SPOUTSDK/SPOUTSDK/SpoutGL`: Spout SDK source used by the Rust bridge
- `agent/native/syphon/Syphon-Framework`: `Syphon/Syphon-Framework` submodule (vendor reference)
- `agent/native/syphon/syphon_bridge.mm`: Syphon sender/client bridge used on macOS

## Run the relay

```powershell
cd agent
cargo run
```

On macOS, build/embed `Syphon.framework` once before running self-check helpers:

```bash
./scripts/embed_syphon_framework.sh
```

To build a distributable macOS DMG with the menu bar app bundle:

```bash
./installer/macos/build-dmg.sh
```

Optional bind override:

```powershell
$env:BROWSER_PORT_AGENT_BIND = "127.0.0.1:1844"
cargo run
```

## Load the extension

Use Chrome's unpacked extension flow and point it at `extention/`.
