use std::fs;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Loads Microsoft YaHei (`msyh.ttc`) from Windows Fonts so CJK filenames and UI labels
/// render crisply in `egui` without falling back to tofu boxes.
pub fn install_chinese_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for candidate in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ] {
        if let Ok(bytes) = fs::read(candidate) {
            fonts.font_data.insert(
                "cjk_primary".to_owned(),
                Arc::new(FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk_primary".to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("cjk_primary".to_owned());
            break;
        }
    }
    ctx.set_fonts(fonts);
}
