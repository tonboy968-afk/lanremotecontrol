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
        Box::new(|cc| {
            // ── 内嵌中文字体（最优先加载） ──────────────────────────────
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "noto-sans-sc".to_owned(),
                egui::FontData::from_static(
                    include_bytes!("../fonts/NotoSansSC.ttf"),
                ),
            );
            // 插入位置 0（最优先），让 Noto Sans SC 处理所有字形（含拉丁 + CJK）
            if let Some(proportional) =
                fonts.families.get_mut(&egui::FontFamily::Proportional)
            {
                proportional.insert(0, "noto-sans-sc".to_owned());
            }
            if let Some(monospace) =
                fonts.families.get_mut(&egui::FontFamily::Monospace)
            {
                monospace.insert(0, "noto-sans-sc".to_owned());
            }
            cc.egui_ctx.set_fonts(fonts);

            Box::new(ui::RemoteControlApp::default())
        }),
    )
    .expect("eframe 启动失败");
}
