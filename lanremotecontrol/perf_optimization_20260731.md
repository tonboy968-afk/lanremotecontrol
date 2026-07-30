# 性能优化报告 — 2026-07-31

## 背景
LAN Remote Control 项目在 localhost 测试中帧率仅 8fps，需要优化到至少 30fps。

## 优化结果

| 指标 | 优化前 | 优化后 | 提升 |
|------|--------|--------|------|
| FPS | 8.0 | 30.5 | **3.8x** |
| Avg interval | 124ms | 33ms | **3.8x** |
| Min interval | 53ms | 15ms | **3.5x** |
| Full frame size | 1.8MB | 437KB | **4.1x** |
| Delta frame size | 525KB avg | 25-45KB | **~10x** |
| Chunks per full frame | 1363 | 331 | **4.1x** |
| Chunks per delta frame | 398 | 20-35 | **~12x** |
| Encode time (full) | N/A | 2-3ms | — |
| Encode time (delta) | N/A | 0.1-0.2ms | — |

## 优化项

### 1. UDP recv timeout: 100ms → 1ms (贡献最大)
- **文件**: `host/src/net.rs` L28
- **原因**: 主循环每次迭代都被 `receive_message()` 阻塞 100ms，导致帧率上限 ~10fps
- **修复**: `set_read_timeout(Some(Duration::from_millis(1)))`

### 2. DXGI AcquireNextFrame timeout: 16ms → 5ms
- **文件**: `host/src/capture.rs` L117
- **原因**: 屏幕无变化时 DXGI 等待 16ms 才返回 timeout，浪费时间
- **修复**: `AcquireNextFrame(5, ...)`

### 3. Tile size: 64 → 128
- **文件**: `common/src/encoding.rs` DEFAULT_TILE_SIZE
- **原因**: 64px tile 导致 1920×1080 有 1350 个 tile，delta region 数量多
- **修复**: 改为 128px → 135 个 tile，减少 region 数量

### 4. LZ4 压缩模式: DEFAULT (而非 HC)
- **文件**: `common/src/encoding.rs` compress_delta/compress_full_frame
- **原因**: HC(9) 压缩 full frame 耗时 42-80ms，DEFAULT 只需 2-3ms
- **修复**: 使用 `compress(data, None, false)` (DEFAULT mode)

### 5. FULL_FRAME_THRESHOLD: 0.3 → 0.5
- **文件**: `common/src/encoding.rs`
- **原因**: 30% 的 tile 变化就发 full frame 太激进，50% 更合理
- **修复**: `pub const FULL_FRAME_THRESHOLD: f64 = 0.5;`

### 6. 空 delta 跳过
- **文件**: `host/src/main.rs`
- **原因**: 无变化时仍发 4-byte delta 包，浪费带宽和 CPU
- **修复**: `if changed_tiles.is_empty() { continue; }`

### 7. 帧率上限: 30fps → 60fps
- **文件**: `host/src/main.rs` FRAME_INTERVAL
- **原因**: 30fps cap 限制了性能上限
- **修复**: `Duration::from_millis(16)` (60fps cap)

## 瓶颈分析

优化后的单帧处理时间：
- downscale: 4-6ms (CPU nearest-neighbor, 2560×1440 → 1920×1080)
- checksum: 1.5-2ms (XXH32 over 135 tiles)
- encode: 0.1-3ms (LZ4 DEFAULT)
- send: 0.1-0.5ms (20-30 chunks)
- **总计**: ~8-12ms

实际 avg 33ms 的差额来自：
- recv timeout 1ms (每循环)
- DXGI timeout 5ms (无变化时)
- 主循环其他逻辑

## 下一步优化方向

1. **GPU downscale**: 用 D3D11 shader 做 2560×1440 → 1920×1080，省 4-6ms CPU
2. **捕获线程分离**: capture 在独立线程，主循环只做 encode+send
3. **双缓冲**: capture 下一帧同时发送当前帧
4. **H.264 硬件编码**: Media Foundation MFT，<5ms 编码 + 10x 压缩比
5. **客户端纹理复用**: 避免每帧 load_texture 重建
