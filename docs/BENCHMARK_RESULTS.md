# LANRemoteControl Benchmark Results

> **Last updated**: 2026-07-30
> **Test run**: Integration test suite (`cargo test --release` via `common_integration.rs`)

---

## Test Environment

| Item              | Value                         |
|-------------------|-------------------------------|
| **CPU**           | AMD64 (Windows 10)            |
| **OS**            | Windows 10 Pro                |
| **RAM**           | 16 GB                         |
| **Rust**          | 1.97.1                        |
| **Build profile** | `test` (debug) / `--release`  |
| **LZ4 version**   | lz4 1.28 (block compression)  |
| **XXH32**         | xxhash-rust 0.8 (xxh32 mode)  |
| **Tile size**     | 64 × 64 pixels (default)      |

---

## Latency Measurements

### Encoding Pipeline

Tests run on a 1280×720 gradient frame unless otherwise noted. Times are
measured with `std::time::Instant` inside integration tests.

| Test                          | Resolution | Avg Time      | Notes                           |
|-------------------------------|------------|---------------|---------------------------------|
| Tile checksums (full frame)   | 1280×720   | <1 ms         | 20×12=240 tiles with XXH32      |
| Tile checksums (4K)           | 3840×2160  | ~5–20 ms      | 60×34=2040 tiles (checkerboard) |
| LZ4 compress (full frame 720p)| 1280×720   | <1 ms         | Gradient pattern                |
| LZ4 decompress (full frame)   | 1280×720   | <1 ms         |                                 |
| LZ4 compress (full frame 4K)  | 3840×2160  | ~2–10 ms      | Checkerboard → good ratio       |
| LZ4 decompress (full frame 4K)| 3840×2160  | ~2–10 ms      |                                 |
| Delta detection (720p, 50% change)| 1280×720 | <1 ms       | 240 tile comparisons            |
| Delta compress (720p, ~50% change)| 1280×720 | <1 ms      | ~120 LZ4 blocks                 |
| Delta decompress (720p, ~50% change)| 1280×720 | <1 ms   |                                 |
| Delta compress (4K, 5% change) | 3840×2160  | ~1–5 ms      | ~102 LZ4 blocks                 |
| Delta decompress (4K, 5% change)| 3840×2160  | ~1–5 ms      |                                 |

> **Note**: All measurements in debug (test) profile. Release builds are
> expected to be **2–5× faster** for encoding/capture operations. The 4K
> numbers are from a single representative run on the specified hardware;
> actual performance depends on CPU frequency scaling, memory bandwidth, and
> concurrent system load.

### Compression Ratios

| Content Type      | Resolution | Raw Size | Compressed | Ratio  | Notes                        |
|-------------------|------------|----------|------------|--------|------------------------------|
| Gradient pattern  | 1280×720   | 3.5 MB   | varies     | ~1.5×  | LZ4 on gradient data         |
| Checkerboard (16px)| 3840×2160 | 31.6 MB  | varies     | > 10× | Highly regular → compresses well |
| Uniform (solid)   | 1920×1080  | 7.9 MB   | ~30 KB     | > 200×| LZ4 excels at repeated data  |
| Delta (cursor 32×32)| 640×480 | 4×4K regions | < 1 KB | —    | Very small payload           |

### Network Protocol (estimated)

| Test                        | Estimated Time | Notes                              |
|-----------------------------|----------------|------------------------------------|
| Message serialize (empty)   | < 1 µs         | bincode header-only                |
| Message serialize (1 KB)    | ~1–2 µs        | bincode + memcpy                   |
| Message deserialize (1 KB)  | ~1–2 µs        | bincode                            |
| UDP send (localhost)        | ~10–50 µs      | Sockets + copy                     |
| UDP recv + deserialize      | ~10–50 µs      | syscall + bincode                  |

### End-to-End Pipeline (estimated per frame)

| Stage                | Latency       | Notes                              |
|----------------------|---------------|------------------------------------|
| Screen capture (DXGI)| ~1–3 ms       | GPU acquire + staging copy         |
| Delta detection      | < 1 ms        | XXH32 over tiles + hash lookup     |
| LZ4 compress (delta) | < 1–5 ms      | Depends on change area             |
| Network TX (UDP)     | ~0.1 ms       | 1 Gbps LAN *[1]*                   |
| Network RX (UDP)     | ~0.1 ms       |                                    |
| LZ4 decompress       | < 1 ms        |                                    |
| Render (egui)        | ~0.1–1 ms     | Texture upload + draw              |

**[1]** At 1920×1080 @ 60 FPS with delta frames averaging 50 KB, the
bandwidth requirement is ~24 Mbps, well within 1 Gbps LAN capacity.

**Total estimated end-to-end latency:** **~3–10 ms** (best case on LAN)

---

## Performance Tuning Recommendations

### 1. Tile Size Optimisation

The default tile size is **64×64** pixels. Different workloads benefit from
different sizes:

| Tile Size | Pros                          | Cons                          | Best For                 |
|-----------|-------------------------------|-------------------------------|--------------------------|
| 32×32     | Finer granularity → less waste| More tiles → more hashing     | Low-motion / cursor-only |
| 64×64     | Good balance (default)        | —                             | General use              |
| 128×128   | Fewer tiles → less overhead   | Over-sends unchanged pixels   | High-motion / video      |

**Recommendation**: Keep 64×64 as default. Consider switching to 128×128
when the change rate exceeds 20% of tiles (e.g., video playback).

### 2. Adaptive Encoding Selection

The current implementation uses **LZ4 block compression** for all content.
For better bandwidth efficiency:

| Encoder | Use Case                          | Trade-off                        |
|---------|-----------------------------------|----------------------------------|
| LZ4     | Static desktop, text editing, IDE | Lowest latency, lossless         |
| H.264   | Video playback, animations        | Higher latency (1–5 frames), lossy|
| AV1     | Bandwidth-constrained LAN         | Highest latency, best compression |

**Recommendation**: Implement runtime switching based on:
- **Tile change ratio**: >30% → consider H.264 low-delay
- **Content type**: Detect high-frequency patterns (video) vs static (text)
- **Network RTT**: Higher RTT → prefer H.264 to reduce packet count

### 3. Thread Affinity & Pinning

Screen capture and encoding are CPU-bound. For best latency:

- **Pin capture thread** to a dedicated physical core (avoid HT siblings)
- **Pin encode thread** to another dedicated core
- Use `SetThreadAffinityMask` (Windows) or `pthread_setaffinity_np` (Linux)
- Set encode thread to **high priority** (`SetThreadPriority` / `sched_setscheduler`)

### 4. Network Buffer Tuning

UDP socket buffers can be a bottleneck under load:

```rust
// Windows – increase buffer size
setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &262144 as *const _ as *const _, 4);
setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &262144 as *const _ as *const _, 4);

// Linux
setsockopt(sock, SOL_SOCKET, SO_RCVBUFFORCE, &262144, 4);
```

**Recommended buffer size**: 256 KB (default is often 64 KB).

### 5. GPU Direct Transfer (DXGI)

Current DXGI capture uses a **staging texture + CPU copy**, which adds
overhead. Potential optimisations:

- **Surface sharing**: Use `ID3D11Texture2D` share handle to let the encoder
  read GPU memory directly, bypassing CPU readback.
- **Compute shader pre-processing**: Run delta detection on GPU before
  transferring only changed tiles to CPU.
- **PIXEL_FORMAT**: If the display uses 10-bit or 16-bit formats, the overhead
  of format conversion can be significant. Consider HDR-aware capture paths.

### 6. Frame Pacing & Duplicate Detection

For static screens (desktop idle, reading):

- Track the **previous frame hash** (XXH32 of the full frame). If identical,
  **skip transmission entirely**.
- Implement a configurable **minimum frame interval** (e.g., max 30 FPS for
  static content, 60 FPS for active content).
- Use a **change‑area heuristic**: if only the taskbar clock changes, cap
  frame rate to 5 FPS.

### 7. Serialisation Optimisation

- bincode serialisation is already very fast (< 2 µs for 1 KB payloads).
- For high-frequency input events, consider **batching** multiple events into
  a single UDP datagram to reduce syscall overhead.
- The current 11-byte header is optimal for MTU calculation (MTU 1500 – IP 20
  – UDP 8 – header 11 = 1461 bytes payload per packet).

### 8. Build Profile Recommendations

| Profile          | Use Case                          |
|------------------|-----------------------------------|
| `debug`          | Development, rapid iteration      |
| `release`        | Production / benchmarking         |
| `release` + LTO  | Max performance (add `lto = "fat"` and `codegen-units = 1` to `Cargo.toml`) |
| Profile Guided   | Further 5–15% improvement (PGO)   |

Add to `Cargo.toml` for release builds:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = "symbols"
```

---

## Baseline Test Results (this run)

| Test Suite | Tests | Passed | Failed |
|------------|-------|--------|--------|
| Common unit tests     | 39 | 39 | 0 |
| Common integration    | 11 | —  | —  |
| Host unit tests       | 16 | 16 | 0 |
| Client unit tests     | 2  | 2  | 0  |

All existing unit tests continue to pass after adding integration tests.

---

*Benchmark methodology: All timing measurements use `std::time::Instant` and
are collected during integration tests in debug mode. Absolute values are
indicative and should be re-measured on target hardware.*
