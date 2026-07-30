//! LANRemoteControl 极简 GUI 界面
//!
//! 包含连接面板和远程控制窗口两个核心界面组件。

use eframe::egui;
use std::sync::{Arc, Mutex};
use std::thread;

use lanremotecontrol_common::*;
use crate::net::{HandshakeClient, UdpClient};

// ============================================================================
// 应用状态
// ============================================================================

/// 应用连接状态
#[derive(Clone, PartialEq)]
enum AppState {
    /// 未连接
    Disconnected,
    /// 正在连接中
    Connecting,
    /// 已连接，包含编码和分辨率信息
    Connected {
        encoding: String,
        width: u32,
        height: u32,
    },
    /// 发生错误
    Error(String),
}

// ============================================================================
// 主应用结构体
// ============================================================================

/// LANRemoteControl 主 GUI 应用
pub struct RemoteControlApp {
    /// 当前连接状态
    state: AppState,
    /// 目标主机 IP 地址
    host_ip: String,
    /// 端口输入文本（字符串形式以便编辑）
    port_text: String,
    /// 目标端口
    host_port: u16,
    /// UDP 客户端（连接成功后创建）
    client: Option<UdpClient>,
    /// 主机能力信息
    caps: Option<CapabilitiesResponse>,
    /// 是否全屏模式
    fullscreen: bool,
    /// 状态消息（显示在界面上）
    status_message: String,
    /// 握手结果共享通道（跨线程）
    handshake_result: Arc<Mutex<Option<Result<CapabilitiesResponse, String>>>>,
}

impl Default for RemoteControlApp {
    fn default() -> Self {
        Self {
            state: AppState::Disconnected,
            host_ip: "127.0.0.1".to_string(),
            port_text: DEFAULT_PORT.to_string(),
            host_port: DEFAULT_PORT,
            client: None,
            caps: None,
            fullscreen: false,
            status_message: String::new(),
            handshake_result: Arc::new(Mutex::new(None)),
        }
    }
}

impl RemoteControlApp {
    /// 开始连接主机
    fn start_connect(&mut self) {
        let host_ip = self.host_ip.clone();
        let port = self.host_port;
        let result = Arc::clone(&self.handshake_result);

        self.state = AppState::Connecting;
        self.status_message = format!("正在连接到 {}:{} ...", host_ip, port);

        thread::spawn(move || {
            match UdpClient::connect(&host_ip, port) {
                Ok(client) => match HandshakeClient::perform_handshake(&client, "", 1) {
                    Ok(caps) => {
                        *result.lock().unwrap() = Some(Ok(caps));
                    }
                    Err(e) => {
                        *result.lock().unwrap() = Some(Err(format!("握手失败: {}", e)));
                    }
                },
                Err(e) => {
                    *result.lock().unwrap() = Some(Err(format!("连接失败: {}", e)));
                }
            }
        });
    }

    /// 断开连接
    fn disconnect(&mut self) {
        self.state = AppState::Disconnected;
        self.client = None;
        self.caps = None;
        self.status_message = "已断开连接".to_string();
        // 清空残留的握手结果
        if let Ok(mut guard) = self.handshake_result.lock() {
            *guard = None;
        }
    }

    // ========================================================================
    // UI 绘制方法
    // ========================================================================

    /// 绘制连接面板
    fn draw_connection_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // 垂直居中布局
            ui.vertical_centered(|ui| {
                // 标题区域
                ui.add_space(80.0);
                ui.heading("LANRemoteControl");
                ui.label("纯局域网 PC 远程控制");
                ui.add_space(30.0);

                // 连接表单（在固定宽度区域内左对齐）
                let form_width = 320.0;
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                    .show(ui, |ui| {
                        ui.set_min_width(form_width);

                        // IP 输入
                        ui.horizontal(|ui| {
                            ui.label("主机 IP:");
                            ui.add_sized(
                                [200.0, 24.0],
                                egui::TextEdit::singleline(&mut self.host_ip)
                                    .hint_text("例如 192.168.1.100"),
                            );
                        });

                        ui.add_space(8.0);

                        // 端口输入
                        ui.horizontal(|ui| {
                            ui.label("端口:    ");
                            ui.add_sized(
                                [200.0, 24.0],
                                egui::TextEdit::singleline(&mut self.port_text)
                                    .hint_text("50000"),
                            );
                        });

                        ui.add_space(16.0);

                        // 状态指示器
                        ui.horizontal(|ui| {
                            let (color, text) = match &self.state {
                                AppState::Disconnected => (egui::Color32::GRAY, "● 未连接"),
                                AppState::Connecting => (egui::Color32::YELLOW, "● 正在连接..."),
                                AppState::Connected { .. } => (egui::Color32::GREEN, "● 已连接"),
                                AppState::Error(_) => (egui::Color32::RED, "● 错误"),
                            };
                            ui.colored_label(color, text);
                        });

                        ui.add_space(12.0);

                        // 连接 / 断开按钮
                        let can_connect = matches!(self.state, AppState::Disconnected);
                        let is_connecting = matches!(self.state, AppState::Connecting);

                        if is_connecting {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("正在连接...");
                            });
                        } else {
                            let button_label = if can_connect { "连接" } else { "断开" };
                            if ui
                                .add_sized(
                                    [form_width, 36.0],
                                    egui::Button::new(button_label),
                                )
                                .clicked()
                            {
                                if can_connect {
                                    // 解析端口
                                    let port: u16 = self
                                        .port_text
                                        .parse()
                                        .unwrap_or(DEFAULT_PORT);
                                    self.host_port = port;
                                    self.start_connect();
                                } else {
                                    self.disconnect();
                                }
                            }
                        }
                    });

                ui.add_space(12.0);

                // 状态 / 错误消息
                if !self.status_message.is_empty() {
                    let is_error = matches!(self.state, AppState::Error(_));
                    if is_error {
                        ui.colored_label(egui::Color32::RED, &self.status_message);

                        ui.add_space(8.0);
                        if ui.button("重试").clicked() {
                            self.state = AppState::Disconnected;
                            self.status_message = String::new();
                        }
                    } else {
                        ui.colored_label(egui::Color32::LIGHT_BLUE, &self.status_message);
                    }
                }
            });
        });
    }

    /// 绘制远程控制窗口
    fn draw_remote_window(
        &mut self,
        ctx: &egui::Context,
        encoding: &str,
        width: u32,
        height: u32,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let screen_rect = ui.max_rect();

            // ── 远程屏幕显示区域（背景占满窗口） ─────────────────────────
            let painter = ui.painter();
            painter.rect_filled(
                screen_rect,
                0.0,
                egui::Color32::from_rgb(16, 16, 22),
            );

            // 居中显示连接信息
            let text = format!(
                "已连接 - {}\n分辨率: {}x{}\n\n远程屏幕显示区域",
                encoding, width, height
            );
            let font_id = egui::FontId::proportional(18.0);

            // 计算文本边界并居中绘制
            let galley = ui.fonts(|f| f.layout_no_wrap(text, font_id, egui::Color32::WHITE));
            let text_pos = egui::pos2(
                screen_rect.center().x - galley.size().x / 2.0,
                screen_rect.center().y - galley.size().y / 2.0 - 40.0,
            );
            painter.galley(text_pos, galley, egui::Color32::WHITE);

            // 底部信息
            let info_text = "FPS: 60 | 延迟: N/A | 纯局域网";
            let info_font = egui::FontId::proportional(14.0);
            let info_galley =
                ui.fonts(|f| f.layout_no_wrap(info_text.to_string(), info_font, egui::Color32::GRAY));
            let info_pos = egui::pos2(
                screen_rect.right() - info_galley.size().x - 16.0,
                screen_rect.bottom() - info_galley.size().y - 16.0,
            );
            painter.galley(info_pos, info_galley, egui::Color32::GRAY);

            // ── 右上角按钮（全屏 + 断开） ────────────────────────────────
            let button_size = egui::Vec2::new(36.0, 36.0);
            let button_spacing = 8.0;
            let top_right = egui::pos2(
                screen_rect.right() - button_size.x - 12.0,
                screen_rect.top() + 12.0,
            );

            // 全屏切换按钮
            let fullscreen_rect = egui::Rect::from_min_size(top_right, button_size);
            let fs_label = if self.fullscreen { "⛶" } else { "⛶" };
            let fs_response = ui.put(
                fullscreen_rect,
                egui::Button::new(egui::RichText::new(fs_label).size(20.0))
                    .fill(egui::Color32::from_black_alpha(80)),
            );
            if fs_response.clicked() {
                self.fullscreen = !self.fullscreen;
                if self.fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }

            // 断开按钮
            let disconnect_rect = egui::Rect::from_min_size(
                egui::pos2(
                    top_right.x,
                    top_right.y + button_size.y + button_spacing,
                ),
                button_size,
            );
            let disc_response = ui.put(
                disconnect_rect,
                egui::Button::new(egui::RichText::new("✕").size(20.0))
                    .fill(egui::Color32::from_rgba_premultiplied(
                        200, 40, 40, 80,
                    )),
            );
            if disc_response.clicked() {
                self.disconnect();
            }

            // 全屏模式下的提示：鼠标移至边缘显示控制条
            if self.fullscreen {
                let pointer_pos = ctx.input(|i| i.pointer.hover_pos());
                if let Some(pos) = pointer_pos {
                    if pos.y < 50.0 || pos.y > screen_rect.bottom() - 50.0 {
                        ctx.request_repaint();
                    }
                }
            }
        });
    }

    /// 绘制错误面板
    fn draw_error_panel(&mut self, ctx: &egui::Context, msg: &str) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(100.0);
                ui.colored_label(
                    egui::Color32::RED,
                    egui::RichText::new("连接错误").size(24.0),
                );
                ui.add_space(12.0);
                ui.label(msg);
                ui.add_space(24.0);
                if ui
                    .add_sized([200.0, 40.0], egui::Button::new("返回"))
                    .clicked()
                {
                    self.state = AppState::Disconnected;
                    self.status_message = String::new();
                }
            });
        });
    }
}

// ============================================================================
// eFrame App 实现
// ============================================================================

impl eframe::App for RemoteControlApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 处理异步握手结果
        if let Ok(mut guard) = self.handshake_result.lock() {
            if let Some(result) = guard.take() {
                match result {
                    Ok(caps) => {
                        let encoding = if caps.encoding.lz4_delta {
                            "LZ4 无损"
                        } else if caps.encoding.h264_low_delay {
                            "H.264 低延迟"
                        } else if caps.encoding.av1_rt {
                            "AV1 实时"
                        } else {
                            "未知"
                        };
                        self.state = AppState::Connected {
                            encoding: encoding.to_string(),
                            width: caps.encoding.max_width,
                            height: caps.encoding.max_height,
                        };
                        self.caps = Some(caps);
                        self.status_message =
                            format!("已连接到 {}:{}", self.host_ip, self.host_port);
                    }
                    Err(e) => {
                        self.state = AppState::Error(e);
                    }
                }
            }
        }

        // 根据当前状态绘制对应界面（克隆 state 以避免借用冲突）
        let current_state = self.state.clone();
        match current_state {
            AppState::Disconnected | AppState::Connecting => {
                self.draw_connection_panel(ctx);
            }
            AppState::Connected {
                encoding,
                width,
                height,
            } => {
                self.draw_remote_window(ctx, &encoding, width, height);
            }
            AppState::Error(msg) => {
                self.draw_error_panel(ctx, &msg);
            }
        }

        // 连接中时持续刷新
        if self.state == AppState::Connecting {
            ctx.request_repaint();
        }
    }
}
