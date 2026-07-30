<div align="center">

<img src="https://img.shields.io/badge/version-v0.1.0-blue.svg" alt="Version">
<img src="https://img.shields.io/badge/license-MIT-green.svg" alt="License">
<img src="https://img.shields.io/badge/language-Rust-DEA584.svg" alt="Rust">
<img src="https://img.shields.io/badge/UI-egui%20%7C%20eframe-FF6921.svg" alt="egui">

# LANRemoteControl

**纯局域网 PC 远程控制软件 · 极低延迟 · 无损画质 · 极简界面**

*专为局域网环境设计的高性能远程控制方案，不依赖任何公网基础设施*

[架构文档](#-架构设计) · [性能基准](#-性能基准) · [快速开始](#-快速开始) · [贡献指南](#-贡献指南)

</div>

---

## 📖 项目简介

LANRemoteControl 是一款**纯局域网** PC 远程控制软件，设计目标是 **极低延迟**（端到端 ~3–10ms）、**无损画质** 与 **极简用户界面**。所有通信均在本地网络内完成，不依赖任何公网服务器或中继节点，天然规避数据外泄风险。

**设计理念：**

- 🚀 **极低延迟** - 基于自定义 UDP 协议 + 增量帧检测，端到端延迟低至 3–10ms
- 🖼️ **无损画质** - 仅传输变化的屏幕区域（Delta Frame），采用 LZ4 无损压缩
- 🏠 **纯局域网** - 不依赖公网，使用本地 IP 与可配置端口，数据不出内网
- 🎯 **极简 UI** - 专注核心功能，连接面板 + 远程控制窗口，零干扰

**技术亮点：**

| 模块 | 技术实现 |
|------|----------|
| 屏幕采集 | Windows DXGI / Linux X11·Wayland / macOS Quartz |
| 编码压缩 | LZ4 块压缩 + XXH32 分块校验 + 增量帧检测 |
| 网络传输 | 自定义 UDP 协议，应用层 Sequence/ACK + DSCP QoS |
| 输入注入 | 底层 API（Windows `SendInput`） |
| 渲染界面 | egui / eframe 原生 GUI |

---

## ✨ 核心特性

| 特性 | 描述 |
|------|------|
| 🌐 **纯局域网** | 无公网依赖，所有通信在本地网络内完成 |
| ⚡ **超低延迟** | UDP + 增量帧，端到端 ~3–10ms（1Gbps LAN） |
| 🖼️ **无损画质** | 增量帧检测，仅传变化区域，LZ4 无损压缩 |
| 🔌 **多协议支持** | 控制指令、屏幕帧、ACK、心跳、连接管理五类消息 |
| 📡 **可靠传输** | 应用层重传 + ACK 确认 + 心跳保活 |
| 🎨 **极简界面** | 连接面板（IP 输入、连接按钮、状态指示）+ 远程控制窗口 |

---

## 🏗️ 架构设计

```
┌─────────────────────────────────────────────────────────────────┐
│                        Client (控制端)                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ UI Layer    │  │ Input Capture│  │ Decoding & Rendering    │  │
│  │ (egui)      │  │ (键鼠采集)   │  │ (解码 + 渲染)           │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
                          │  UDP (LAN)
                          ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Host (被控端)                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ Screen      │  │ Encoding     │  │ Input Injection         │  │
│  │ Capture     │→│ (LZ4+Delta)  │  │ (SendInput)             │  │
│  │ (DXGI)      │  │              │  │                         │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 项目结构

```
lanremotecontrol/
├── common/                 # 公共模块（协议、序列化、压缩）
├── host/                   # 被控端服务（屏幕采集 + 编码 + 输入注入）
├── client/                 # 控制端应用（egui 界面 + 输入采集 + 解码渲染）
└── Cargo.toml              # Rust Workspace 配置

docs/                       # 设计文档
├── ARCHITECTURE.md         # 架构概览
├── NETWORK_PROTOCOL.md     # 网络协议设计
├── SCREEN_CAPTURE_AND_ENCODING.md  # 屏幕采集与编码
├── INPUT_FORWARDING.md     # 输入转发
├── HOST_CLIENT_FLOW.md     # 主从流程
├── UI_UX_DESIGN.md         # UI/UX 设计
├── BENCHMARK_RESULTS.md    # 性能基准测试
└── IMPLEMENTATION_GUIDE.md # 实现指南
```

---

## 🚀 快速开始

### 环境要求

- Rust 1.70+（推荐 1.97+）
- Windows 10+（当前完整支持 DXGI 采集）
- 1 Gbps 局域网（推荐）

### 构建

```bash
# 克隆仓库
git clone https://github.com/tonboy968-afk/lanremotecontrol.git
cd lanremotecontrol/lanremotecontrol

# 构建（Release 模式推荐用于生产环境）
cargo build --release

# 运行被控端（Host）
cargo run --release -p lanremotecontrol-host

# 运行控制端（Client）
cargo run --release -p lanremotecontrol-client
```

### 使用流程

1. 在被控电脑上启动 **Host** 服务
2. 在控制电脑上启动 **Client**，输入被控端 IP
3. 点击连接，等待能力协商完成
4. 开始远程控制

---

## 📊 性能基准

> 测试环境：AMD64 / Windows 10 / 16GB RAM / Rust 1.97.1 / LZ4 1.28

| 指标 | 数值 |
|------|------|
| 端到端延迟（1Gbps LAN） | **~3–10 ms** |
| 增量帧检测（720p） | < 1 ms |
| LZ4 压缩/解压（720p） | < 1 ms |
| 屏幕采集（DXGI） | ~1–3 ms |
| 带宽占用（1080p@60fps, Δ50KB） | ~24 Mbps |
| 单元测试 | 57 passed / 0 failed |

**压缩表现：**

| 内容类型 | 分辨率 | 压缩比 |
|----------|--------|--------|
| 纯色画面 | 1920×1080 | > 200× |
| 棋盘格 | 3840×2160 | > 10× |
| 渐变图案 | 1280×720 | ~1.5× |

---

## 🔧 技术栈

| 类别 | 技术 |
|------|------|
| 语言 | Rust 2021 Edition |
| 构建 | Cargo Workspace |
| 序列化 | bincode + serde |
| 压缩 | LZ4 1.28 |
| 哈希 | XXH32 (xxhash-rust) |
| 界面 | egui 0.27 / eframe 0.27 |
| 屏幕采集 | Windows DXGI |
| 输入注入 | Windows `SendInput` |

---

## 🤝 贡献指南

我们欢迎所有形式的贡献！

### 需要帮助的方向

| 领域 | 任务 | 难度 |
|------|------|------|
| 🐧 **跨平台** | Linux (X11/Wayland) 屏幕采集与输入注入 | ⭐⭐⭐⭐ |
| 🍎 **跨平台** | macOS (Quartz) 屏幕采集与输入注入 | ⭐⭐⭐⭐ |
| 🎥 **编码** | H.264 / AV1 自适应编码支持 | ⭐⭐⭐ |
| 🔐 **安全** | 预共享密钥 / PIN 认证 | ⭐⭐⭐ |
| 🧪 **测试** | 跨平台 E2E 测试 | ⭐⭐ |
| 📝 **文档** | 部署指南、API 文档 | ⭐ |

### 贡献流程

1. Fork 本仓库
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交改动 (`git commit -m 'feat: add amazing feature'`)
4. 推送分支 (`git push origin feature/amazing-feature`)
5. 提交 Pull Request

---

## 📄 协议

[MIT License](./LICENSE) - Copyright © 2026 LANRemoteControl Team

---

<div align="center">

**如果这个项目对你有帮助，请给一个 ⭐ Star 支持一下！**

[报告 Bug](https://github.com/tonboy968-afk/lanremotecontrol/issues) · [功能建议](https://github.com/tonboy968-afk/lanremotecontrol/issues)

</div>
