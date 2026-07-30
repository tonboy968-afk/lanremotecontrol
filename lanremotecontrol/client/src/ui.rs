//! LANRemoteControl 极简 GUI 界面
//!
//! 包含连接面板和远程控制窗口两个核心界面组件。

use eframe::egui;
use egui::ColorImage;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use lanremotecontrol_common::*;
use crate::net::{
    bgra_to_rgba, run_frame_receiver, FrameBuffer, HandshakeClient, UdpClient,
};

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
    /// UDP 客户端（连接成功后创建，Arc 以便跨线程共享）
    client: Option<Arc<UdpClient>>,
    /// 主机能力信息
    caps: Option<CapabilitiesResponse>,
    /// 是否全屏模式
    fullscreen: bool,
    /// 状态消息（显示在界面上）
    status_message: String,
    /// 握手结果共享通道（跨线程）
    handshake_result: Arc<Mutex<Option<Result<CapabilitiesResponse, String>>>>,
    /// 接收到的远程屏幕帧缓冲 (BGRA, width, height)
    frame_buffer: FrameBuffer,
    /// egui 纹理句柄，用于绘制远程画面
    remote_texture: Option<egui::TextureHandle>,
    /// 已接收帧计数
    frame_count: u64,
    /// 远程画面宽度 (原生像素)
    remote_width: u32,
    /// 远程画面高度 (原生像素)
    remote_height: u32,
    /// 输入消息序列号
    input_seq: u32,
    /// 上一次发送给远端的鼠标坐标 (用于移动事件去重)
    last_mouse_remote_pos: Option<(f32, f32)>,
    /// 上一帧的修饰键状态 (用于检测 Shift/Ctrl/Alt 变化)
    last_modifiers: egui::Modifiers,
    /// 自动连接标志 (测试用)
    auto_connect: bool,
}

impl Default for RemoteControlApp {
    fn default() -> Self {
        Self {
            state: AppState::Disconnected,
            host_ip: String::from("127.0.0.1"),
            port_text: DEFAULT_PORT.to_string(),
            host_port: DEFAULT_PORT,
            client: None,
            caps: None,
            fullscreen: false,
            status_message: String::new(),
            handshake_result: Arc::new(Mutex::new(None)),
            frame_buffer: Arc::new(Mutex::new(None)),
            remote_texture: None,
            frame_count: 0,
            remote_width: 0,
            remote_height: 0,
            input_seq: 1,
            last_mouse_remote_pos: None,
            last_modifiers: egui::Modifiers::default(),
            auto_connect: true,
        }
    }
}

impl RemoteControlApp {
    /// 开始连接主机
    fn start_connect(&mut self) {
        let host_ip = self.host_ip.clone();
        let port = self.host_port;
        let result = Arc::clone(&self.handshake_result);

        // 先创建 UDP 客户端（连接时分配本地端口）
        match UdpClient::connect(&host_ip, port) {
            Ok(client) => {
                let client = Arc::new(client);
                let client_for_thread = Arc::clone(&client);
                self.client = Some(client);

                self.state = AppState::Connecting;
                self.status_message = format!("正在连接到 {}:{} ...", host_ip, port);

                thread::spawn(move || {
                    match HandshakeClient::perform_handshake(&client_for_thread, "", 1) {
                        Ok(caps) => {
                            *result.lock().unwrap() = Some(Ok(caps));
                        }
                        Err(e) => {
                            *result.lock().unwrap() = Some(Err(format!("握手失败: {}", e)));
                        }
                    }
                });
            }
            Err(e) => {
                self.state = AppState::Error(format!("连接失败: {}", e));
            }
        }
    }

    /// 断开连接
    fn disconnect(&mut self) {
        self.state = AppState::Disconnected;
        self.client = None;
        self.caps = None;
        self.remote_texture = None;
        self.frame_count = 0;
        self.status_message = "已断开连接".to_string();
        // 清空残留的握手结果与帧缓冲
        if let Ok(mut guard) = self.handshake_result.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.frame_buffer.lock() {
            *guard = None;
        }
    }

    /// 检查并消费新收到的远程画面帧，更新纹理
    fn update_remote_texture(&mut self, ctx: &egui::Context) {
        if let Ok(mut guard) = self.frame_buffer.lock() {
            if let Some((bgra, w, h)) = guard.take() {
                // 保存远程分辨率用于鼠标坐标映射
                self.remote_width = w;
                self.remote_height = h;
                // 远程分辨率变化时重置鼠标位置跟踪
                self.last_mouse_remote_pos = None;

                // BGRA → RGBA 转换，供 egui ColorImage 使用
                let rgba = bgra_to_rgba(&bgra);
                let image = ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    &rgba,
                );

                // 复用已有纹理句柄，避免每帧重建
                if let Some(tex) = &mut self.remote_texture {
                    tex.set(
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                } else {
                    self.remote_texture =
                        Some(ctx.load_texture("remote-screen", image, Default::default()));
                }
                self.frame_count += 1;
            }
        }
    }

    // ========================================================================
    // UI 绘制方法
    // ========================================================================

    /// 绘制连接面板
    fn draw_connection_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading("LANRemoteControl");
                ui.label("🎮 控制端 - 连接至被控主机");
                ui.add_space(4.0);
                ui.label(egui::RichText::new("💡 被控端请运行 lanremotecontrol-host.exe").size(12.0).color(egui::Color32::LIGHT_BLUE));
                ui.add_space(20.0);

                let form_width = 320.0;
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                    .show(ui, |ui| {
                        ui.set_min_width(form_width);

                        ui.label(egui::RichText::new("输入被控主机的局域网 IP 后点击连接").size(12.0).color(egui::Color32::GRAY));
                        ui.add_space(12.0);

                        ui.horizontal(|ui| {
                            ui.label("主机 IP:");
                            ui.add_sized(
                                [200.0, 24.0],
                                egui::TextEdit::singleline(&mut self.host_ip)
                                    .hint_text("例如 192.168.1.100"),
                            );
                        });

                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.label("端口:    ");
                            ui.add_sized(
                                [200.0, 24.0],
                                egui::TextEdit::singleline(&mut self.port_text)
                                    .hint_text("50000"),
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.add_space(48.0);
                            ui.label(egui::RichText::new("默认端口: 50000（与 host 一致）").size(11.0).color(egui::Color32::GRAY));
                        });

                        ui.add_space(16.0);

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

    /// 绘制远程控制窗口（含远程画面显示）
    fn draw_remote_window(
        &mut self,
        ctx: &egui::Context,
        _encoding: &str,
        _width: u32,
        _height: u32,
    ) {
        // 检查新帧并更新纹理
        self.update_remote_texture(ctx);

        // 保存图像区域信息，供绘图闭包之后使用（避免借用冲突）
        let mut image_info: Option<(egui::Rect, u32, u32)> = None;

        egui::CentralPanel::default().show(ctx, |ui| {
            let screen_rect = ui.max_rect();

            // ── 如果有远程画面，绘制它 ──────────────────────────────────
            if let Some(tex) = &self.remote_texture {
                // 保持宽高比缩放至窗口大小
                let img_size = tex.size_vec2();
                let available = screen_rect.size();
                let scale = (available / img_size).min_elem().min(1.0);
                let scaled_size = img_size * scale;
                let pos = egui::pos2(
                    screen_rect.left() + (screen_rect.width() - scaled_size.x) / 2.0,
                    screen_rect.top() + (screen_rect.height() - scaled_size.y) / 2.0,
                );
                let image_rect = egui::Rect::from_min_size(pos, scaled_size);
                ui.painter().image(tex.id(), image_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                // 保存，供闭包外输入捕获使用
                image_info = Some((image_rect, self.remote_width, self.remote_height));

                // 底部信息（FPS、帧计数）
                let info_text = format!(
                    "已接收: {} 帧 | 远程桌面 | 纯局域网",
                    self.frame_count
                );
                let info_font = egui::FontId::proportional(14.0);
                let info_galley = ui.fonts(|f| {
                    f.layout_no_wrap(info_text, info_font, egui::Color32::GRAY)
                });
                let info_pos = egui::pos2(
                    screen_rect.right() - info_galley.size().x - 16.0,
                    screen_rect.bottom() - info_galley.size().y - 16.0,
                );
                ui.painter()
                    .galley(info_pos, info_galley, egui::Color32::GRAY);

                // ── 右上角按钮 ──────────────────────────────────────────
                let button_size = egui::Vec2::new(36.0, 36.0);
                let button_spacing = 8.0;
                let top_right = egui::pos2(
                    screen_rect.right() - button_size.x - 12.0,
                    screen_rect.top() + 12.0,
                );

                // 全屏切换
                let fs_response = ui.put(
                    egui::Rect::from_min_size(top_right, button_size),
                    egui::Button::new(egui::RichText::new("⛶").size(20.0))
                        .fill(egui::Color32::from_black_alpha(80)),
                );
                if fs_response.clicked() {
                    self.fullscreen = !self.fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                }

                // 断开
                let disc_response = ui.put(
                    egui::Rect::from_min_size(
                        egui::pos2(top_right.x, top_right.y + button_size.y + button_spacing),
                        button_size,
                    ),
                    egui::Button::new(egui::RichText::new("✕").size(20.0))
                        .fill(egui::Color32::from_rgba_premultiplied(200, 40, 40, 80)),
                );
                if disc_response.clicked() {
                    self.disconnect();
                }
            } else {
                // ── 无画面时显示提示 ────────────────────────────────────
                let painter = ui.painter();
                painter.rect_filled(screen_rect, 0.0, egui::Color32::from_rgb(16, 16, 22));

                let text = format!("已连接\n正在等待远程画面...\n\n已接收 {} 帧", self.frame_count);
                let font_id = egui::FontId::proportional(18.0);
                let galley =
                    ui.fonts(|f| f.layout_no_wrap(text, font_id, egui::Color32::WHITE));
                let text_pos = egui::pos2(
                    screen_rect.center().x - galley.size().x / 2.0,
                    screen_rect.center().y - galley.size().y / 2.0 - 40.0,
                );
                painter.galley(text_pos, galley, egui::Color32::WHITE);

                // 右上角断开按钮（无画面时也需要）
                let top_right = egui::pos2(screen_rect.right() - 48.0, screen_rect.top() + 12.0);
                let disc_response = ui.put(
                    egui::Rect::from_min_size(top_right, egui::Vec2::new(36.0, 36.0)),
                    egui::Button::new(egui::RichText::new("✕").size(20.0))
                        .fill(egui::Color32::from_rgba_premultiplied(200, 40, 40, 80)),
                );
                if disc_response.clicked() {
                    self.disconnect();
                }
            }
        });

        // ── 输入事件捕获与发送 ──────────────────────────────────────────
        if let Some((img_rect, rw, rh)) = image_info {
            if rw > 0 && rh > 0 {
                self.capture_and_send_input(ctx, img_rect, rw, rh);
            }
        }
    }

    /// 捕获并发送键盘/鼠标事件到被控主机
    fn capture_and_send_input(
        &mut self,
        ctx: &egui::Context,
        img_rect: egui::Rect,
        remote_w: u32,
        remote_h: u32,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => return,
        };

        // ── 检测修饰键 (Shift/Ctrl/Alt) 状态变化 ───────────────────────
        // egui 不会为修饰键生成 Event::Key，必须通过 Modifiers 结构检测变化
        let cur_mod = ctx.input(|i| i.modifiers);
        if self.last_modifiers.shift != cur_mod.shift {
            self.send_modifier_key(&client, 0x10, cur_mod.shift);
        }
        if self.last_modifiers.ctrl != cur_mod.ctrl {
            self.send_modifier_key(&client, 0x11, cur_mod.ctrl);
        }
        if self.last_modifiers.alt != cur_mod.alt {
            self.send_modifier_key(&client, 0x12, cur_mod.alt);
        }
        self.last_modifiers = cur_mod;

        let events = ctx.input(|i| i.events.clone());
        for event in &events {
            match event {
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } => {
                    if *repeat {
                        continue;
                    }
                    if let Some(vk_code) = egui_key_to_vk(key) {
                        let mod_bits: u8 = (if modifiers.alt { 0b100 } else { 0 })
                            | (if modifiers.ctrl { 0b010 } else { 0 })
                            | (if modifiers.shift { 0b001 } else { 0 });
                        let payload =
                            ControlCommandPayload::Key(KeyEvent {
                                key_code: vk_code,
                                pressed: *pressed,
                                modifiers: mod_bits,
                                timestamp_us: current_timestamp_us(),
                            });
                        self.send_control_command(&client, payload);
                    }
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let (rx, ry) = map_pos_to_remote(*pos, img_rect, remote_w, remote_h);
                    let btn_idx: u8 = match button {
                        egui::PointerButton::Primary => 0,
                        egui::PointerButton::Secondary => 1,
                        egui::PointerButton::Middle => 2,
                        _ => continue,
                    };
                    let nx = crate::net::normalize_abs_coord(rx, remote_w);
                    let ny = crate::net::normalize_abs_coord(ry, remote_h);
                    let payload =
                        ControlCommandPayload::MouseButton(MouseButtonEvent {
                            button: btn_idx,
                            pressed: *pressed,
                            x: nx,
                            y: ny,
                            timestamp_us: current_timestamp_us(),
                        });
                    self.send_control_command(&client, payload);
                }
                egui::Event::PointerMoved(pos) => {
                    let (rx, ry) = map_pos_to_remote(*pos, img_rect, remote_w, remote_h);
                    let threshold = 0.5;
                    if self.last_mouse_remote_pos.map_or(true, |(lx, ly)| {
                        (lx - rx).abs() > threshold || (ly - ry).abs() > threshold
                    }) {
                        self.last_mouse_remote_pos = Some((rx, ry));
                        // 归一化到 Windows 绝对坐标范围 0..65535
                        let dx = crate::net::normalize_abs_coord(rx, remote_w);
                        let dy = crate::net::normalize_abs_coord(ry, remote_h);
                        let payload =
                            ControlCommandPayload::MouseMove(MouseMoveEvent {
                                dx,
                                dy,
                                abs_coords: true,
                                timestamp_us: current_timestamp_us(),
                            });
                        self.send_control_command(&client, payload);
                    }
                }
                egui::Event::Scroll(delta) => {
                    // 将 egui 滚动量 (points) 转换为 Windows WHEEL_DELTA 当量
                    let sx = (delta.x * 2.0).round() as i32;
                    let sy = (delta.y * 2.0).round() as i32;
                    if sx != 0 || sy != 0 {
                        let payload =
                            ControlCommandPayload::Scroll(ScrollEvent {
                                delta_x: sx,
                                delta_y: sy,
                                timestamp_us: current_timestamp_us(),
                            });
                        self.send_control_command(&client, payload);
                    }
                }
                _ => {}
            }
        }
    }

    /// 序列化并发送一条控制命令到被控主机
    fn send_control_command(
        &mut self,
        client: &UdpClient,
        payload: ControlCommandPayload,
    ) {
        if let Ok(bytes) = bincode::serialize(&payload) {
            let msg = Message::new(
                MessageType::ControlCommand,
                self.input_seq,
                bytes,
            );
            self.input_seq = self.input_seq.wrapping_add(1);
            let _ = client.send(&msg);
        }
    }

    /// 发送修饰键 (Shift/Ctrl/Alt) 的按下或释放事件
    fn send_modifier_key(&mut self, client: &UdpClient, vk_code: u32, pressed: bool) {
        let payload = ControlCommandPayload::Key(KeyEvent {
            key_code: vk_code,
            pressed,
            modifiers: 0,
            timestamp_us: current_timestamp_us(),
        });
        self.send_control_command(client, payload);
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
        // 自动连接 (测试模式)
        if self.auto_connect && self.state == AppState::Disconnected {
            self.auto_connect = false;
            self.start_connect();
        }

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

                        // ── 启动后台帧接收线程 ──────────────────────────
                        if let Some(client) = self.client.clone() {
                            let fb = Arc::clone(&self.frame_buffer);
                            thread::spawn(move || {
                                run_frame_receiver(client, fb);
                            });
                        }
                    }
                    Err(e) => {
                        self.state = AppState::Error(e);
                        self.client = None;
                    }
                }
            }
        }

        // 根据当前状态绘制对应界面
        let current_state = self.state.clone();
        match current_state {
            AppState::Disconnected | AppState::Connecting => {
                self.draw_connection_panel(ctx);
            }
            AppState::Connected { .. } => {
                self.draw_remote_window(ctx, "", 0, 0);
            }
            AppState::Error(msg) => {
                self.draw_error_panel(ctx, &msg);
            }
        }

        // 连接中或已连接时持续刷新界面
        if self.state == AppState::Connecting || matches!(self.state, AppState::Connected { .. }) {
            ctx.request_repaint();
        }
    }
}

// ============================================================================
// 辅助函数：键盘映射、坐标转换、时间戳
// ============================================================================

/// 将 egui::Key 映射为 Windows 虚拟键码 (VK_*)
fn egui_key_to_vk(key: &egui::Key) -> Option<u32> {
    use egui::Key;
    Some(match key {
        // ── 字母 A-Z ──
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,

        // ── 数字行 0-9 (egui 不区分主键盘/小键盘数字) ──
        Key::Num0 => 0x30,
        Key::Num1 => 0x31,
        Key::Num2 => 0x32,
        Key::Num3 => 0x33,
        Key::Num4 => 0x34,
        Key::Num5 => 0x35,
        Key::Num6 => 0x36,
        Key::Num7 => 0x37,
        Key::Num8 => 0x38,
        Key::Num9 => 0x39,

        // ── 功能键 F1-F20 ──
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::F13 => 0x7C,
        Key::F14 => 0x7D,
        Key::F15 => 0x7E,
        Key::F16 => 0x7F,
        Key::F17 => 0x80,
        Key::F18 => 0x81,
        Key::F19 => 0x82,
        Key::F20 => 0x83,

        // ── 方向键 ──
        Key::ArrowDown => 0x28,
        Key::ArrowLeft => 0x25,
        Key::ArrowRight => 0x27,
        Key::ArrowUp => 0x26,

        // ── 导航/编辑键 ──
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Enter => 0x0D,
        Key::Escape => 0x1B,
        Key::Space => 0x20,

        // ── 剪贴板快捷键 (作为虚拟键处理) ──
        Key::Copy => return None,   // 不适合直接映射 VK
        Key::Cut => return None,
        Key::Paste => return None,

        // ── 符号键 ──
        Key::Minus => 0xBD,       // -
        Key::Plus => 0xBB,        // +
        Key::Equals => 0xBB,      // =
        Key::Comma => 0xBC,       // ,
        Key::Period => 0xBE,      // .
        Key::Semicolon => 0xBA,   // ;
        Key::Colon => 0xBA,       // : (same VK as ; — VK_OEM_1)
        Key::Backslash => 0xDC,   // \
        Key::Slash => 0xBF,       // /
        Key::OpenBracket => 0xDB, // [
        Key::CloseBracket => 0xDD,// ]
        Key::Backtick => 0xC0,    // `
        Key::Pipe => 0xDC,        // | (same VK as \ — VK_OEM_5)
        Key::Questionmark => 0xBF,// ? (same VK as / — VK_OEM_2)

        // 未映射的键 (Copy, Cut, Paste, F21-F35 等)
        _ => return None,
    })
}

/// 将本地鼠标位置映射到远程屏幕坐标
///
/// `img_rect` 是远程桌面在本地窗口中的绘制矩形，
/// `remote_w/h` 是远程屏幕的原生分辨率。
fn map_pos_to_remote(
    pos: egui::Pos2,
    img_rect: egui::Rect,
    remote_w: u32,
    remote_h: u32,
) -> (f32, f32) {
    // 防御：避免除以零
    if img_rect.width() <= 0.0 || img_rect.height() <= 0.0 {
        return (0.0, 0.0);
    }
    let local_x = pos.x - img_rect.left();
    let local_y = pos.y - img_rect.top();
    let scale_x = remote_w as f32 / img_rect.width();
    let scale_y = remote_h as f32 / img_rect.height();
    let rx = (local_x * scale_x).clamp(0.0, remote_w as f32);
    let ry = (local_y * scale_y).clamp(0.0, remote_h as f32);
    (rx, ry)
}

/// 当前微秒时间戳
fn current_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
