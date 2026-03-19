# Syphon Debug Playbook

This file is a deeper checklist for debugging Syphon output issues.

## 1. Build Once

```bash
cd agent
cargo build --release --bin browser-port --bin syphon_selftest --bin syphon_probe
```

## 2. Validate Bridge Locally (No WebSocket / No Extension)

```bash
cd agent
target/release/syphon_selftest codex-syphon-selftest 10
```

If this fails, prioritize fixes in:
- `agent/native/syphon/syphon_bridge.mm`
- `agent/src/bin/syphon_selftest.rs`

## 3. Validate Cross-Process Receive

Terminal A:

```bash
cd agent
target/release/syphon_selftest codex-syphon-selftest 10
```

Terminal B:

```bash
cd agent
target/release/syphon_probe codex-syphon-selftest 3
```

If this fails but step 2 passes, check client connection path in:
- `agent/native/syphon/syphon_bridge.mm`
- `agent/src/bin/syphon_probe.rs`

## 4. Validate App Sender Registration

```bash
cd agent
target/release/browser-port output-helper --mode syphon --ws ws://127.0.0.1:9
```

Probe static senders:

```bash
cd agent
target/release/syphon_probe browser-port-syphon-1 3
target/release/syphon_probe browser-port-syphon-2 3
target/release/syphon_probe browser-port-syphon-3 3
target/release/syphon_probe browser-port-syphon-4 3
```

## 5. Validate Decode-to-Syphon Path with Real Stream

Watch logs for:
- `decoder backend selected`
- `first decoded frame`
- `decoder stalled ... waiting keyframe`
- `fallback to openh264`

Interpretation:
- `first decoded frame backend=videotoolbox` appears quickly:
  - VT path is healthy.
- Repeated stall then OpenH264 fallback:
  - Stream framing/timing does not satisfy VT consistently.

## 6. Minimize Test Contamination

Stop prior instances before each run:

```bash
cd agent
pkill -f "browser-port output-helper --mode syphon"
```

## 7. Reporting Template

When sharing results, include:
- Command run (exact)
- First 30-60s logs from `output-helper`
- `syphon_probe` output for one affected sender
- Whether selftest/probe standalone passed
