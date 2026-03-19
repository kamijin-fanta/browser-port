# BrowserPort

This directory is the standalone BrowserPort repo.

## Layout

- `agent/`: Rust WebSocket relay and output helper
- `extention/`: Chrome extension frontend
- `agent/native/spout/SPOUTSDK`: `leadedge/Spout2` submodule
- `agent/native/spout/SPOUTSDK/SPOUTSDK/SpoutGL`: Spout SDK source used by the Rust bridge

## Run the relay

```powershell
cd agent
cargo run
```

Optional bind override:

```powershell
$env:BROWSER_PORT_AGENT_BIND = "127.0.0.1:9876"
cargo run
```

## Load the extension

Use Chrome's unpacked extension flow and point it at `extention/`.
