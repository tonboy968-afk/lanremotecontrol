# LANRemoteControl Implementation Guide

## Overview
This document provides a step-by-step implementation guide for the LANRemoteControl software. It compiles all design documents into sequential development phases with performance tuning guidelines for latency and quality.

---

## Development Phases

### Phase 1: Network & Capture Foundation
**Objective**: Establish the core network communication layer and screen capture mechanism.

**Tasks**:
1. **Network Protocol Implementation**:
   - Implement custom UDP-based network protocol with message types (control commands, screen frames, ACKs, heartbeats, connection management)
   - Implement sequence number and ACK mechanism for reliability
   - Configure TOS/QoS settings for low delay (DSCP markings, socket options)
   - Implement connection establishment flow (discovery, negotiation, session initialization)

2. **Screen Capture Module**:
   - Implement OS-specific screen capture APIs:
     - Windows: DXGI Desktop Duplication API
     - macOS: ScreenCaptureKit
     - Linux: X11 Xshm / Wayland xdg-desktop-portal or PipeWire
   - Implement tile-based delta frame detection with hash-based change detection
   - Implement threshold-based full frame fallback (>30% tiles changed)

**Deliverables**:
- `src-host/network/udp_listener.rs` (or equivalent in chosen language)
- `src-host/capture/screen_capture.rs`
- `docs/NETWORK_PROTOCOL.md`, `docs/SCREEN_CAPTURE_AND_ENCODING.md`

---

### Phase 2: Encoding & Input
**Objective**: Implement screen encoding/compression and input forwarding mechanisms.

**Tasks**:
1. **Encoding Module**:
   - Implement Raw Delta + LZ4 Compression for minimal latency
   - Implement Low-Delay H.264/AV1 with hardware acceleration (NVENC, AMF, QuickSync, VideoToolbox)
   - Implement adaptive switching logic based on network RTT, packet loss, CPU/GPU utilization

2. **Input Forwarding Mechanism**:
   - Implement client-side input event capture:
     - Windows: `WH_KEYBOARD_LL`, `WH_MOUSE_LL`
     - macOS: Core Graphics event taps
     - Linux: evdev / X11 XQueryKeymap
   - Implement host-side low-level input injection:
     - Windows: `SendInput()`
     - macOS: `CGEventPost()`
     - Linux: XTest extension or `/dev/input/event*`
   - Implement timestamping and ordering rules with jitter compensation (2-5ms buffer)

**Deliverables**:
- `src-host/encoding/compressor.rs`
- `src-client/input/input_capture.rs`
- `src-host/input/input_injector.rs`
- `docs/INPUT_FORWARDING.md`

---

### Phase 3: UI Integration
**Objective**: Implement the minimalist user interface for connection and remote control.

**Tasks**:
1. **Connection Panel UI**:
   - Implement IP address input field with validation
   - Implement Connect button with loading state
   - Implement status indicator (gray=inactive, yellow=connecting, green=connected, red=error)

2. **Remote Control Window UI**:
   - Implement frame display area with aspect ratio preservation
   - Implement fullscreen toggle button
   - Implement disconnect button
   - Implement temporary control bar for fullscreen mode (auto-hide after inactivity)

3. **Interaction Flow Integration**:
   - Connect UI components to network and session management logic
   - Implement state transitions (Idle -> Connecting -> Connected -> Disconnecting -> Disconnected)

**Deliverables**:
- `src-client/ui/connection_panel.rs`
- `src-client/ui/remote_control_window.rs`
- `docs/UI_UX_DESIGN.md`

---

### Phase 4: Testing & Optimization
**Objective**: Validate functionality, performance, and reliability; optimize for latency and quality.

**Tasks**:
1. **Functional Testing**:
   - Test connection establishment and teardown flows
   - Test screen frame transmission and rendering
   - Test input event capture and injection
   - Test edge cases (network drops, resolution changes, fullscreen transitions)

2. **Performance Testing**:
   - Measure end-to-end latency (capture -> encode -> network -> decode -> render -> input feedback)
   - Test under various network conditions (LAN 1Gbps, WiFi 5/6 with interference)
   - Validate CPU/GPU utilization for encoding and decoding

3. **Optimization**:
   - Tune LZ4 compression parameters for speed vs size
   - Adjust H.264/AV1 encoder presets for lowest latency
   - Optimize network buffer sizes and UDP socket options
   - Profile and optimize UI rendering pipeline for minimal frame delay

**Deliverables**:
- Test suite for network, capture, encoding, input, and UI components
- Performance benchmarking reports
- `docs/HOST_CLIENT_FLOW.md`

---

## Performance Tuning Guidelines

### Latency Optimization
1. **Network Layer**:
   - Use UDP for all screen frame and input event transmission
   - Set TOS/DSCP values for low-latency traffic prioritization (EF or AF41 for control commands)
   - Minimize application-level buffering; send packets immediately upon capture

2. **Encoding Layer**:
   - Prefer LZ4 raw delta compression for static or text-heavy content
   - Use low-delay H.264/AV1 profiles with zero B-frames and keyframe interval of 1-2 seconds
   - Enable hardware acceleration (NVENC, AMF, QuickSync, VideoToolbox)

3. **Input Forwarding**:
   - Coalesce rapid mouse movements and keyboard events to reduce network overhead
   - Use high-resolution timestamps on client side; apply jitter buffer (2-5ms) on host side for reordering

### Quality Optimization
1. **Screen Fidelity**:
   - Maintain 32-bit BGRA/RGBA pixel format to preserve color accuracy
   - Use lossless LZ4 compression for delta regions when bandwidth permits
   - Switch to H.264/AV1 with CRF 0-8 (visually lossless range) for high-motion content

2. **Adaptive Strategies**:
   - Monitor network RTT and packet loss; adjust encoding strategy dynamically
   - Detect content type (text vs video vs graphics) and select optimal compression method
   - Fallback to full keyframes when delta detection indicates >30% screen change

---

## Integration Notes

- **Project Structure**:
  ```
  lanremotecontrol/
  ├── docs/
  │   ├── ARCHITECTURE.md
  │   ├── NETWORK_PROTOCOL.md
  │   ├── SCREEN_CAPTURE_AND_ENCODING.md
  │   ├── INPUT_FORWARDING.md
  │   ├── UI_UX_DESIGN.md
  │   ├── HOST_CLIENT_FLOW.md
  │   └── IMPLEMENTATION_GUIDE.md
  ├── src-host/
  │   ├── network/
  │   ├── capture/
  │   ├── encoding/
  │   └── input/
  └── src-client/
      ├── network/
      ├── ui/
      └── input/
  ```

- **Language/Framework Recommendations**:
  - Host service: Rust, C++, or Go for low-level system access and performance
  - Client UI: Qt, Flutter, or Tauri (Rust + web frontend) for cross-platform support with minimal overhead
  - Encoding: Leverage native OS APIs (Media Foundation, VideoToolbox, VAAPI) via language bindings

- **Testing Environment**:
  - Set up local LAN test network with multiple machines
  - Use packet capture tools (Wireshark) to validate network protocol behavior
  - Measure latency using high-resolution timestamps and frame rendering metrics
