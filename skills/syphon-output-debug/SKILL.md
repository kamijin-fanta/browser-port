---
name: syphon-output-debug
description: Diagnose and validate Syphon output on macOS for this repository, especially when Sender is visible but frames are black or delayed. Use when running selftest/probe binaries, checking output-helper logs, verifying VideoToolbox/OpenH264 behavior, and reproducing sender/frame delivery issues.
---

# Syphon Output Debug

Use this skill to verify and debug Syphon output end-to-end in this repository.

## Quick Start

1. Build debug tools and app binaries.
2. Confirm standalone Syphon bridge health with `syphon_selftest`.
3. Confirm cross-process receive with `syphon_probe`.
4. Run the app and inspect `output-helper` decode/send logs.
5. Decide whether the issue is decoder stall, publish path, or sender naming/routing.

## Commands (From Repo Root)

```bash
cd agent
cargo build --release --bin browser-port --bin syphon_selftest --bin syphon_probe
```

### 1) Standalone Sender/Receiver Sanity

```bash
cd agent
target/release/syphon_selftest codex-syphon-selftest 10
```

Expected:
- `send_ok=true`
- `receive_ok[...] = true`
- `non_black_ratio` close to `1.0000`

### 2) Cross-Process Probe Against Selftest Sender

Run selftest in one terminal:

```bash
cd agent
target/release/syphon_selftest codex-syphon-selftest 10
```

Probe from another terminal:

```bash
cd agent
target/release/syphon_probe codex-syphon-selftest 3
```

Expected:
- `client_size` becomes non-zero.
- `frame=1 ... non_black_ratio` is non-zero.

### 3) App Path (output-helper / Syphon mode)

```bash
cd agent
target/release/browser-port output-helper --mode syphon --ws ws://127.0.0.1:9
```

Then probe static senders:

```bash
cd agent
target/release/syphon_probe browser-port-syphon-1 3
target/release/syphon_probe browser-port-syphon-2 3
target/release/syphon_probe browser-port-syphon-3 3
target/release/syphon_probe browser-port-syphon-4 3
```

Note:
- Primed static senders can be black until real frames are decoded/sent.
- Visibility in Syphon client/OBS without non-black frames usually indicates decode/send path issues, not discovery issues.

## Log Patterns to Check

Healthy decode start:
- `output-helper: decoder backend selected backend=videotoolbox`
- `output-helper: first decoded frame backend=videotoolbox size=...`

Fallback indicator:
- `output-helper: videotoolbox stalled for ... chunks; fallback to openh264`
- `output-helper: decoder backend selected backend=openh264`

Potential stream metadata issue:
- `output-helper: keyframe flag missing; treating packet as keyframe via H264 NAL detection`

## Triage Matrix

- Sender visible, selftest/probe non-black OK, app black:
  - Focus on `output-helper` decode logs and keyframe/stall behavior.
- Sender visible, app eventually starts after fallback:
  - VideoToolbox input framing or stream cadence issue likely.
- Sender not visible:
  - Focus on Syphon bridge runtime loading and sender creation.

## Cleanup / Process Control

Stop lingering helper processes before retest:

```bash
cd agent
pkill -f "browser-port output-helper --mode syphon"
```

Re-run tests from a clean state to avoid multi-process interference.

## Detailed Playbook

For a fuller troubleshooting sequence and checklist, read:
- `skills/syphon-output-debug/references/playbook.md`
