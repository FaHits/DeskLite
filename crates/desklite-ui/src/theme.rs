use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

pub fn fence_bg_color(alpha: u8) -> D2D1_COLOR_F {
    let a = (f32::from(alpha) / 255.0).clamp(0.15, 0.92);
    rgba(0.08, 0.09, 0.12, a)
}

pub const HEADER_BG: D2D1_COLOR_F = rgba(0.14, 0.16, 0.22, 0.78);
pub const BORDER_NORMAL: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.20);
pub const BORDER_PORTAL: D2D1_COLOR_F = rgba(0.42, 0.72, 1.0, 0.40);
pub const ITEM_HOVER_BG: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.14);
pub const ITEM_SELECTED_BG: D2D1_COLOR_F = rgba(0.35, 0.62, 0.98, 0.28);
pub const TEXT_PRIMARY: D2D1_COLOR_F = rgba(0.96, 0.97, 1.0, 0.96);
pub const TEXT_MUTED: D2D1_COLOR_F = rgba(0.74, 0.78, 0.86, 0.80);
pub const SCROLLBAR_THUMB: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.30);
