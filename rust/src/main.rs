#![cfg_attr(windows, windows_subsystem = "windows")]
//! MC Server Manager — управление серверами Minecraft.

mod app;
mod backup;
mod config;
mod launch;
mod lists;
mod mods;
mod ping;
mod props;
mod server;
mod tunnel;
mod ui;

use eframe::egui;

fn load_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // На Windows берём системные шрифты: Segoe UI для текста и Consolas для консоли.
    let candidates: &[(&str, &[&str], bool)] = &[
        ("ui", &[r"C:\Windows\Fonts\segoeui.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"], false),
        ("mono", &[r"C:\Windows\Fonts\consola.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"], true),
        ("symbols", &[r"C:\Windows\Fonts\seguisym.ttf"], false),
    ];
    for (name, paths, mono) in candidates {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert(name.to_string(), egui::FontData::from_owned(bytes).into());
            let fam = if *mono { egui::FontFamily::Monospace } else { egui::FontFamily::Proportional };
            if *name == "symbols" {
                fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.to_string());
            } else {
                fonts.families.get_mut(&fam).unwrap().insert(0, name.to_string());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let icon = ui::app_icon();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MC Server Manager")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "MC Server Manager",
        options,
        Box::new(|cc| {
            load_fonts(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)))
        }),
    )
}
