# 转化分析：从 LZ4 全帧压缩迁移到硬件 H.264 低延迟通路

> 背景：当前远控"页面刷新像 PPT 一样卡"。本文件在通读 `lanremotecontrol/{common,host,client}` 三个 crate 后，给出根因诊断 + 正确转化方案。
> 结论先行：**卡顿不是 LZ4 本身的问题，而是"全帧 + CPU 回读 + UDP 双倍重发 + 客户端每帧重建纹理"的架构问题**；`hevc.rs` 那个"转化"方向是错的（每帧 spawn ffmpeg 子进程 + 像素无损 = 既慢又占带宽）。

---

## 0. 项目现状全景（通读结论）

真实可运行代码在 `lanremotecontrol/{common,host,client}`（顶层 `src-host`/`src-client` 是空目录，已废弃）。

| 模块 | 文件 | 状态 | 说明 |
|------|------|------|------|
| 协议 | `common/src/lib.rs` | ✅ 可用 | 二进制 UDP 协议，6 种 MessageType，分片重组、ACK、心跳 |
| 捕获 | `common/src/capture.rs` | ⚠️ 有瓶颈 | DXGI Desktop Duplication，**逐行 Map 回读 CPU** |
| 编码 | `common/src/encoding.rs` | ⚠️ 热路径只用了一半 | `compress_full_frame`（全帧 LZ4）被用；tile-delta 机器（`detect_delta_tiles`/`compress_delta`）写好但**热路径没调用** |
| 编码(新) | `common/src/hevc.rs` | ❌ 错误方向 | 每帧 `Command::new("ffmpeg")` spawn + x265 像素无损 |
| 主机循环 | `host/src/main.rs` | ⚠️ 卡顿根源 | capture→`compress_full_frame`→`send_fragmented` 背靠背、无帧率控制 |
| 主机网络 | `host/src/net.rs` | ⚠️ 双倍重发 | `send_fragmented` 每个 chunk 发 **2 遍** |
| 客户端收 | `client/src/net.rs` | ⚠️ 每帧重建纹理 | `decompress`→`bgra_to_rgba`(8MB 拷贝)→`frame_buffer` |
| 客户端渲染 | `client/src/ui.rs` | ⚠️ 每帧 `load_texture` | 每帧新建 egui 纹理 + 全量上传 GPU |
| 客户端解码 | 无 | ❌ 缺失 | 没有视频解码器，只做 LZ4 解压 |

**关键事实**：当前运行链路用的是**全帧 LZ4**（`compress_full_frame`），不是 tile-delta。encoding.rs 里那套完善的增量编码是死代码（有测试但没接进 `main.rs`）。

---

## 1. 为什么"像 PPT 一样卡"——根因（带代码定位）

### 根因 A：DXGI 捕获的 CPU 回读瓶颈
`common/src/capture.rs` `copy_to_cpu()`：
- `CreateTexture2D(STAGING)` → `CopyResource` → `Map` → **逐行 `extend_from_slice`** 拼成 `Vec<u8>`。
- 1080p = 8MB，经 Map（通常为 write-combined/uncached 内存）串行搬移，CPU+GPU 双向阻塞。
- 没有把 D3D11 纹理直接交给编码器（GPU→GPU），一切走内存。

### 根因 B：每帧发"整屏"而不是"增量"
`host/src/main.rs` 帧广播块：`encoding::compress_full_frame(&frame.data)`。
- 即便桌面基本静止，也每帧 LZ4 整屏 8MB。LZ4 高压缩比下仍 ~1–3MB。
- 按 `SCREEN_FRAME_CHUNK_DATA_SIZE=1320` → **约 1000–2000 个 chunk/帧**。

### 根因 C：每个 chunk 发 2 遍
`host/src/net.rs` `send_fragmented`：`for pass in 0..2 { for wire in &chunk_wires { send_to } }`。
- datagram 数翻倍 → **约 2000–4000 个 UDP 包/帧**。
- 干净 LAN 不需要 2×；应改 NACK/ARQ 或 FEC。

### 根因 D：没有帧率/流控
`host/src/main.rs`：`AcquireNextFrame(16,...)` 后直接 compress→send，循环周期被"压缩+发 4000 个包"主导（几十~几百 ms）→ **实际 2–8 fps，而非 60**。

### 根因 E：客户端每帧重建纹理 + 8MB 格式转换
- `client/src/net.rs` `run_frame_receiver`：`decompress_full_frame` → `bgra_to_rgba`（8MB 逐像素拷贝）。
- `client/src/ui.rs` `render_screen`：`ctx.load_texture(...)` **每帧新建纹理**并全量上传 GPU。应复用 `egui::TextureHandle` 的 `.set(...)`。

### 根因 F：egui 不适合高帧率视频
立即模式 GUI + 每帧全量纹理重建，渲染侧也吃掉一截延迟。

> 合计：1080p 下每个"画面"要经 8MB Map 回读 + 8MB LZ4 + ~4000 个 UDP 包（局域网内核发送缓冲大量丢包→帧不完整→PPT 节奏）+ 8MB BGRA→RGBA + 每帧纹理重建。这就是卡顿的全部来源。

---

## 2. 关于"转化/不用 LZ4"：`hevc.rs` 为什么是错误方向

`common/src/hevc.rs`：
- `encode_frame()` 每次 `Command::new("ffmpeg").spawn()` → 等子进程退出，**~30–60 ms/帧**（进程创建 + x265 启动开销）。
- 用 `libx265` **像素无损**（`lossless=1`）→ 1080p 桌面仍数百 KB~几 MB/帧 → 还是几千个 chunk。
- 既慢（进程 spawn）又占带宽（无损）→ 接进去只会**更卡**。
- 文档里 `// ... rest of the module stays the same ...` 占位也说明这是草稿。

结论：**删除 `hevc.rs`**。若必须坚持"无损"，也应走"常驻会话的 HEVC"而非"每帧子进程"，但 HEVC 解码在弱客户端更重、带宽仍大。对"极低延迟"目标，HEVC 无损是反方向。

---

## 3. 正确的转化方案：硬件 H.264 低延迟（GPU 直通）

**目标延迟**：捕获+编码+解码 < 10ms（GPU 内）；LAN 网络 +1–2ms；端到端 < 16–20ms（≈1 帧）。1080p60 @ 5–30 Mbps。

**核心转变**：用真正的视频编解码器（帧间预测），而不是"每帧独立压缩器"。

### 3.1 捕获（host）：不做 CPU 回读
保留 DXGI Desktop Duplication，但**不 Map 到 CPU**。`DxgiCapture` 暴露 `ID3D11Texture2D`，转成 NV12 后直接喂编码器（经 `IMFDXGIDeviceManager`）。消除 8MB 回读。

### 3.2 编码（host）：常驻硬件 H.264 会话（二选一）
- **(推荐) Media Foundation H.264 Encoder MFT**（`d3d11`/`dxva` 硬件变换，`IMFTransform`）：
  - `CODECAPI_AVLowLatencyMode = 1`、`CODECAPI_AVEncMPVGOPSize = 0`（无 B 帧）、`keyint` 1–2s、`MF_MT_FRAME_RATE = 60`。
  - 输入 D3D11 NV12 表面；输出 Annex-B H.264 NAL → 包进 `ScreenFrame`。
  - 硬件（NVENC/QSV/AMF），实时，<5ms。**无需 ffmpeg，无子进程。**
- **(次选，落地更快) `rust-ffmpeg`/`ffmpeg-next` 常驻 `AVCodecContext`**：`h264_nvenc`/`h264_qsv`/`libx264`（`-preset ultrafast -tune zerolatency -g 120`）。⚠️ 若仍走 CPU 喂数据则需回读；短期可接受，长期要 GPU 喂帧。

### 3.3 码率模式：有损"视觉无损"，不要像素无损（关键）
- 用 **QP/CRF ~18–22 或 CBR 8–30 Mbps**（YUV 4:2:0，必要时 4:4:4）。
- 这是带宽层面最大的单一胜利，也是让"每帧只有几个~几十个 chunk"的前提。
- 对 PRD 的"无损画质"：**真·逐像素无损 + 低延迟 + 低带宽三者不可兼得**（高动态视频下）。应解读为**视觉无损**（H.264 CRF 0–8 / 4:4:4）。这点需与产品方确认。

### 3.4 传输：keyframe + P 帧，去掉 2× 重发
- 大多数帧是 P 帧（帧间差）→ 1–50 chunk。
- 去掉 `send_fragmented` 的双倍发；改 **NACK/ARQ**（接收方在短窗口内请求缺失 chunk）或接受偶发丢包 + 周期 keyframe 刷新（keyint 1–2s）。
- LAN 丢包≈0，简单 keyframe 周期刷新即可。

### 3.5 解码 + 渲染（client）
- 硬件 H.264 解码（MF `H264 Decoder MFT` / DXVA2）→ D3D11 纹理。
- **复用持久 `egui::TextureHandle`**（`.set(...)`）替代每帧 `load_texture`；去掉 BGRA→RGBA 的 Rust 拷贝（直接上传或 shader 内换序）。

---

## 4. 分阶段执行清单（远离 LZ4）

### Phase 0 — 先止血（不引入新编解码器，纯利用已有代码）
1. 去掉 `send_fragmented` 的 2× 重发（或改条件重发/NACK）。省 50% 带宽。
2. 主机 `main.rs` 改走**已写好的 tile-delta 路径**（`tile_checksums`→`detect_delta_tiles`→`build_delta_regions`→`compress_delta`）。静止桌面每帧≈0 字节，**办公类远控的卡顿大概率立刻消失**。
3. 客户端 `ui.rs`：复用持久 `TextureHandle`（`.set`）；尽量去掉每帧 `bgra_to_rgba` 全量拷贝。

### Phase 1 — 硬件 H.264 编码（真正的"转化"）
4. 新增 `common/src/h264.rs`：MF H.264 Encoder MFT（或 rust-ffmpeg 常驻会话），D3D11 NV12 输入、低延迟、视觉无损 QP。
5. 重构 `DxgiCapture` 暴露 D3D11 纹理（而非 CPU BGRA）给编码路径。
6. `host/src/main.rs` 循环：capture→encode(GPU)→send(keyframe/P 帧) 按显示刷新率。
7. 协商已就绪：`EncodingCapabilities.h264_low_delay=true`，客户端选 `"h264"`。

### Phase 2 — 硬件解码 + GPU 渲染（client）
8. 新增 `client/src/h264_decoder.rs`（MF / ffmpeg）→ D3D11 纹理。
9. `run_frame_receiver` 解码进持久缓冲/纹理。
10. `ui.rs` 经持久 `TextureHandle` 渲染。

### Phase 3 — 传输加固
11. NACK/ARQ 或 FEC；帧率对齐显示刷新；基于 RTT 的自适应码率/质量；按需 keyframe。

### Phase 4 — 退役 LZ4
12. 删除 `encoding.rs` 全帧/死代码 tile-delta（或仅保留为"超静止文本"极低带宽回退）；删除 `hevc.rs`。H.264 成为唯一主通路。

---

## 5. 立即可做的验证（确认诊断）

- **同机 loopback 跑 host+client**：若仍 PPT，说明瓶颈在 CPU/编码/渲染而非网络。预期仍卡（8MB 回读 + 全帧 LZ4 + 2× 发 + 每帧纹理重建）。
- **看 `lrc_client_debug.log` / Wireshark**：统计每帧 datagram 数。预期上千个 → 印证带宽/吞吐理论。
- **临时改主机只发 keyframe + 走 delta 路径**：看办公类卡顿是否消失（即 Phase 0 收益）。

---

## 6. 决策待确认
1. "无损画质"是否接受**视觉无损**（CRF 18–22 / 4:2:0）？还是必须逐像素无损（则只能走 LZ4-delta 或 HEVC 持久会话，且带宽高）。
2. 编码器选型：**Media Foundation H.264 MFT（推荐，硬件、无 ffmpeg 依赖）** vs **rust-ffmpeg 常驻会话（落地快，但有 ffmpeg 依赖）**。
3. 是否先执行 **Phase 0 快速止血**（低风险、立即见效），再上 Phase 1 硬件 H.264。
