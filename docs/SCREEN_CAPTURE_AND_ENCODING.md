# Screen Capture and Encoding Design

## 1. OS-Specific Screen Capture APIs

To achieve extremely low latency and high fidelity screen capture, we will use native OS-level APIs for each platform:

### Windows
- **Primary API**: DXGI (DirectX Graphics Infrastructure) Desktop Duplication API
  - Provides frame-by-frame access to the desktop without rendering overhead
  - Captures at the compositor level, ensuring what the user sees is exactly what is captured
  - Supports hardware-accelerated encoding via Media Foundation or NVENC/AMD AMF when available
- **Fallback API**: GDI/DWM BitBlt (for older systems or specific virtual environments)

### macOS
- **Primary API**: ScreenCaptureKit (introduced in macOS 12 Monterey)
  - Modern, efficient screen capture API with low CPU overhead
  - Supports per-display and per-window capture
  - Integrates well with VideoToolbox for hardware encoding
- **Fallback API**: CGWindowListCreateCGImages / NSBitmapImageRep

### Linux
- **Primary API**: X11 Xshm / XCB + `xdg-desktop-portal` (for Wayland)
  - For X11: `XShmGetImage` with Shared Memory extension for minimal copy overhead
  - For Wayland: `xdg-desktop-portal-screen-cast` or PipeWire source capture
- **Fallback API**: `/dev/fb0` (framebuffer) for minimal headless environments

---

## 2. Delta Frame Detection Algorithm

To minimize bandwidth and ensure ultra-low latency, we will only transmit changed portions of the screen (delta frames). The delta detection algorithm operates as follows:

### 2.1 Frame Comparison Strategy
1. **Tile-based Partitioning**: Divide the screen into fixed-size tiles (e.g., 64x64 or 128x128 pixels). This allows transmitting only modified regions rather than full frames.
2. **Hash-based Change Detection**: For each tile, compute a fast checksum (e.g., XXH32 or a simple pixel stride sum) on the current capture and compare it with the previous frame's checksum for that tile.
3. **Threshold-based Full Frame Fallback**: If >30% of tiles are marked as changed, switch to transmitting a full frame (or use a more aggressive compression strategy) to avoid overhead from sending too many small delta patches.

### 2.2 Sequence and Synchronization
- Each captured frame or delta patch is assigned a monotonically increasing sequence number.
- The client acknowledges receipt of frames via ACK messages containing the last successfully received sequence number.
- If a gap in sequence numbers is detected, the host will send a full keyframe (I-frame) to resynchronize the decoding state.

---

## 3. Compression Strategy

To balance **lossless/ultra-low-latency** requirements with bandwidth constraints, we implement a multi-tier compression strategy:

### 3.1 Raw Delta + LZ4 Compression (Primary for Minimal Latency)
- **Use Case**: Fast-moving content, text-heavy UIs, or when the network is extremely reliable (e.g., 1Gbps+ LAN).
- **Method**: 
  - After delta detection identifies changed tiles, raw pixel data (typically 32-bit BGRA or RGBA) for those tiles is compressed using **LZ4** (or `lz4hc` for higher compression at slight CPU cost).
  - LZ4 provides extremely fast decompression (< 1ms typical), preserving the end-to-end low latency requirement.
- **Advantages**: Near-zero encoding/decoding delay, preserves exact pixel fidelity (lossless for the delta region).

### 3.2 Low-Delay H.264 / AV1 with Intra-Frame Focus (Secondary for High Motion/Bandwidth Constraints)
- **Use Case**: Video playback, high-motion graphics, or when LAN bandwidth is constrained (e.g., WiFi 5/6 with interference).
- **Method**:
  - Use hardware-accelerated encoders: NVENC (NVIDIA), AMF (AMD), QuickSync (Intel), or VideoToolbox (macOS).
  - Configure encoder for **low-latency mode** (e.g., NVENC `low_latency` or `ll_hq` preset, zero B-frames, keyframe interval set to 1-2 seconds).
  - For truly "lossless-like" visual quality under motion, use H.264 Profile High at Level 5.1 with CRF 0-8 (visually lossless range) or AV1 in real-time encoding mode if hardware supports it.
- **Advantages**: Significantly reduces bandwidth for complex scenes while maintaining low enough latency (< 50ms encode + network + decode) to feel responsive on a LAN.

### 3.3 Adaptive Switching Logic
The host encoder will dynamically switch between Raw+LZ4 and Low-Delay H.264/AV1 based on:
- **Network RTT and Packet Loss**: Measured via UDP heartbeat/ACK round-trip times. High loss or high RTT triggers more aggressive compression (H.264/AV1).
- **CPU/GPU Utilization**: If host CPU is saturated, fall back to hardware-accelerated H.264/AV1 encoding.
- **Content Type Detection**: Simple heuristics (e.g., high edge density or text regions favor LZ4 delta; high color gradient/motion favors H.264/AV1).

---

## 4. Integration with Network Protocol

- Screen frames (delta patches or encoded frames) are segmented into network packets matching the MTU (typically 1500 bytes for Ethernet, adjusted for UDP header size).
- Each packet includes:
  - Frame sequence number
  - Packet index (for reassembly if split)
  - Payload type indicator (raw delta, LZ4-compressed delta, or encoded video frame)
- The client reassembles packets, decompresses/decodes, and presents the frame to the rendering pipeline with minimal queuing delay.
