# LANRemoteControl 黑屏问题修复

**日期**: 2026-07-31  
**问题**: 客户端连接成功后显示黑屏，无远程画面  

## 根因

**UDP Socket 接收缓冲区溢出导致第一帧（全帧）丢失。**

### 详细分析

1. Host 发送第一帧（全帧）时，2560×1440 BGRA → LZ4 压缩后约 700KB，拆分成 **544 个 UDP chunk**
2. Windows 默认 UDP 接收缓冲区仅 **8KB**，瞬间涌入的 544 个包大量丢失
3. 客户端只收到 544 个 chunk 中的极少数，全帧永远无法组装完成
4. `persistent_bgra` 保持 `None`（从未初始化）
5. 后续所有 delta 帧因 `persistent_bgra == None` 被跳过 → **永久黑屏**

### 诊断日志证据

**修复前**:
```
msg_id=1, chunks=417, size=549509, type=full  ← 全帧开始
（无 "Frame assembled" 日志）                    ← chunk 丢失，组装失败
msg_id=2, chunks=44, size=56996, type=delta   ← delta 帧到达
Delta frame received but no persistent buffer — skipping  ← 因无全帧基础，跳过
```

**修复后**:
```
msg_id=1, chunks=544, size=716857, type=full
Frame assembled: 2560x1440 (716857 bytes), type=full
LZ4 full-frame decompress OK: 2560×1440 -> 14745600 bytes  ← 全帧成功！
msg_id=2, chunks=199, size=262331, type=delta
Delta decompress OK: 208 regions
Delta applied: 208 regions -> 2560x1440 buffer              ← delta 应用成功！
```

## 修复方案

### 1. 增大 UDP Socket 缓冲区至 4MB

**Host** (`host/src/net.rs` `UdpListener::bind`):
```rust
#[cfg(windows)]
{
    use std::os::windows::io::AsRawSocket;
    const SOL_SOCKET: i32 = 0xFFFF;
    const SO_RCVBUF: i32 = 0x1002;
    const SO_SNDBUF: i32 = 0x1001;
    let buf_size: i32 = 4 * 1024 * 1024;
    unsafe {
        let raw = socket.as_raw_socket() as usize;
        #[link(name = "ws2_32")]
        extern "system" {
            fn setsockopt(s: usize, level: i32, optname: i32, optval: *const i32, optlen: i32) -> i32;
        }
        setsockopt(raw, SOL_SOCKET, SO_RCVBUF, &buf_size, 4);
        setsockopt(raw, SOL_SOCKET, SO_SNDBUF, &buf_size, 4);
    }
}
```

**Client** (`client/src/net.rs` `UdpClient::connect`): 同样的 setsockopt 调用。

### 2. Host 发送 chunk 时每 64 个 yield 一次

```rust
if i > 0 && i % 64 == 0 {
    std::thread::yield_now();
}
```

避免瞬间 burst 淹没 NIC/OS 缓冲区。

### 3. Host 每 60 帧强制发送全帧（keyframe）

```rust
let force_full = frame_seq > 0 && frame_seq % 60 == 0;
```

确保客户端即使丢失全帧也能在 60 帧后恢复。

### 4. 客户端 debug log 写入固定路径

`%APPDATA%/lanremotecontrol/lrc_client_debug.log`（而非当前工作目录），方便定位问题。

## 验证结果

- 75 个测试全部通过
- 客户端成功接收全帧 + delta 帧
- `[LRC-GUI] Received frame: 2560x1440 (14745600 bytes)` 确认帧到达 GUI 渲染层
- Delta 帧压缩比：全帧 14.7MB → delta ~10-300KB（97-99.9% 压缩率）

## 修改文件

| 文件 | 修改内容 |
|------|----------|
| `host/src/net.rs` | UDP 缓冲区 4MB + chunk yield |
| `client/src/net.rs` | UDP 缓冲区 4MB + debug log 路径 |
| `host/src/main.rs` | 每 60 帧 keyframe + 重构全帧/delta 判断逻辑 |
| `client/src/ui.rs` | 清理测试用 auto_connect |
