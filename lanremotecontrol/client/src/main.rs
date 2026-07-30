//! LANRemoteControl Client Application
//!
//! 极简的远程控制 GUI。运行在控制机上，通过 UDP 协议连接主机服务，
//! 实现低延迟、画质无损的远程屏幕查看和键盘鼠标控制。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod net;
mod ui;

fn main() {
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([400.0, 300.0])
            .with_title("LANRemoteControl"),
        ..Default::default()
    };

    eframe::run_native(
        "LANRemoteControl",
        native_options,
        Box::new(|_cc| Box::new(ui::RemoteControlApp::default())),
    )
    .expect("eframe 启动失败");
}
