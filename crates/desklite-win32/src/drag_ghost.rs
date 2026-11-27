use std::mem::size_of;
use std::path::Path;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP,
    SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST, IDC_ARROW, LoadCursorW,
    RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::icons::extract_file_icon_pbgra;

const GHOST_SIZE: i32 = 104;
static GHOST_CLASS_REG: OnceLock<bool> = OnceLock::new();

/// Computes the top-left screen origin for `DragGhost` so the rounded preview plate sits
/// horizontally centered and above the cursor tip (matching Explorer's drag preview).
pub(crate) fn ghost_window_origin(cursor_x: i32, cursor_y: i32) -> (i32, i32) {
    (
        cursor_x - GHOST_SIZE / 2,
        cursor_y - (GHOST_SIZE as f32 * 0.86).round() as i32,
    )
}

unsafe extern "system" fn ghost_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn ensure_ghost_window_class() -> bool {
    *GHOST_CLASS_REG.get_or_init(|| unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else {
            return false;
        };
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(ghost_wnd_proc),
            hInstance: hinst.into(),
            hCursor: cursor,
            lpszClassName: w!("DeskLiteDragGhostWnd"),
            ..Default::default()
        };
        RegisterClassExW(&wc) != 0
    })
}

/// Renders a premultiplied 32bpp top-down DIB (`HBITMAP`) showing `paths[0]`'s icon at ~85%
/// opacity and, when `paths.len() > 1`, a bottom-right count badge.
pub(crate) fn render_drag_preview_dib(paths: &[String], size_px: i32) -> Option<HBITMAP> {
    if paths.is_empty() || size_px <= 0 {
        return None;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size_px,
            biHeight: -size_px, // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mem_dc = unsafe { CreateCompatibleDC(None) };
    if mem_dc.is_invalid() {
        return None;
    }
    let hbmp = unsafe { CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) }
        .ok()?;
    if hbmp.is_invalid() || bits_ptr.is_null() {
        unsafe {
            let _ = DeleteDC(mem_dc);
        }
        return None;
    }

    let old_obj = unsafe { SelectObject(mem_dc, hbmp.into()) };
    let rendered = render_into_dc(mem_dc, paths, size_px);
    unsafe {
        let _ = SelectObject(mem_dc, old_obj);
        let _ = DeleteDC(mem_dc);
    }

    if rendered {
        Some(hbmp)
    } else {
        unsafe {
            let _ = DeleteObject(hbmp.into());
        }
        None
    }
}

fn render_into_dc(mem_dc: windows::Win32::Graphics::Gdi::HDC, paths: &[String], size_px: i32) -> bool {
    let Ok(d2d_factory) =
        (unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) })
    else {
        return false;
    };
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        ..Default::default()
    };
    let Ok(rt): Result<ID2D1DCRenderTarget, _> = (unsafe { d2d_factory.CreateDCRenderTarget(&props) })
    else {
        return false;
    };
    let rc = RECT {
        left: 0,
        top: 0,
        right: size_px,
        bottom: size_px,
    };
    if unsafe { rt.BindDC(mem_dc, &rc) }.is_err() {
        return false;
    }

    let decoded_icon = extract_file_icon_pbgra(Path::new(&paths[0]));
    let s = size_px as f32;

    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));

        // Subtle translucent backing plate so bright/dark icons both stay legible over any desktop/fence.
        if let Ok(bg_brush) = rt.CreateSolidColorBrush(
            &D2D1_COLOR_F {
                r: 0.10,
                g: 0.12,
                b: 0.17,
                a: 0.62,
            },
            None,
        ) {
            let plate = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: s * 0.12,
                    top: s * 0.08,
                    right: s * 0.88,
                    bottom: s * 0.84,
                },
                radiusX: 12.0,
                radiusY: 12.0,
            };
            rt.FillRoundedRectangle(&plate, &bg_brush);
            if let Ok(border_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.24,
                },
                None,
            ) {
                rt.DrawRoundedRectangle(&plate, &border_brush, 1.0, None);
            }
        }

        if let Some(icon) = decoded_icon {
            let bmp_props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let sz = D2D_SIZE_U {
                width: icon.width,
                height: icon.height,
            };
            if let Ok(bmp) = rt.CreateBitmap(
                sz,
                Some(icon.pbgra.as_ptr().cast()),
                icon.width * 4,
                &bmp_props,
            ) {
                let dst = D2D_RECT_F {
                    left: s * 0.19,
                    top: s * 0.15,
                    right: s * 0.81,
                    bottom: s * 0.77,
                };
                rt.DrawBitmap(
                    &bmp,
                    Some(&dst),
                    0.85,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }

        if paths.len() > 1 {
            let count_text = if paths.len() > 99 {
                "99+".to_string()
            } else {
                paths.len().to_string()
            };
            let badge_w = if count_text.len() >= 3 {
                s * 0.36
            } else if count_text.len() == 2 {
                s * 0.28
            } else {
                s * 0.24
            };
            let badge_h = s * 0.24;
            let badge_rect = D2D_RECT_F {
                left: s * 0.94 - badge_w,
                top: s * 0.92 - badge_h,
                right: s * 0.94,
                bottom: s * 0.92,
            };
            let badge_rr = D2D1_ROUNDED_RECT {
                rect: badge_rect,
                radiusX: badge_h * 0.5,
                radiusY: badge_h * 0.5,
            };
            if let Ok(badge_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.23,
                    g: 0.51,
                    b: 0.96,
                    a: 0.96,
                },
                None,
            ) {
                rt.FillRoundedRectangle(&badge_rr, &badge_brush);
            }
            if let Ok(ring_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.92,
                },
                None,
            ) {
                rt.DrawRoundedRectangle(&badge_rr, &ring_brush, 1.5, None);
            }
            if let Ok(dwrite) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)
                && let Ok(fmt) = dwrite.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    (s * 0.13).clamp(11.0, 16.0),
                    w!("en-US"),
                )
            {
                let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                if let Ok(txt_brush) = rt.CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    None,
                ) {
                    let utf16: Vec<u16> = count_text.encode_utf16().collect();
                    rt.DrawText(
                        &utf16,
                        &fmt,
                        &badge_rect,
                        &txt_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        }

        rt.EndDraw(None, None).is_ok()
    }
}

/// Lightweight click-through layered preview window following the pointer during internal
/// DeskLite file drags (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE`).
pub struct DragGhost {
    hwnd: HWND,
}

impl DragGhost {
    pub fn show(paths: &[String], screen_x: i32, screen_y: i32) -> Option<Self> {
        if paths.is_empty() || !ensure_ghost_window_class() {
            return None;
        }
        let hbmp = render_drag_preview_dib(paths, GHOST_SIZE)?;
        let Ok(hinst) = (unsafe { GetModuleHandleW(None) }) else {
            unsafe {
                let _ = DeleteObject(hbmp.into());
            }
            return None;
        };

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE,
                w!("DeskLiteDragGhostWnd"),
                w!("DeskLiteDragGhost"),
                WS_POPUP,
                screen_x,
                screen_y,
                GHOST_SIZE,
                GHOST_SIZE,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        };
        let Ok(hwnd) = hwnd else {
            unsafe {
                let _ = DeleteObject(hbmp.into());
            }
            return None;
        };

        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DeleteObject(hbmp.into());
            }
            return None;
        }
        let old_obj = unsafe { SelectObject(mem_dc, hbmp.into()) };
        let pt_dst = POINT {
            x: screen_x,
            y: screen_y,
        };
        let sz = SIZE {
            cx: GHOST_SIZE,
            cy: GHOST_SIZE,
        };
        let pt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ulw_ok = unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pt_dst),
                Some(&sz),
                Some(mem_dc),
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
        .is_ok();

        unsafe {
            let _ = SelectObject(mem_dc, old_obj);
            let _ = DeleteDC(mem_dc);
            let _ = DeleteObject(hbmp.into());
        }

        if !ulw_ok {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return None;
        }

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Some(Self { hwnd })
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd.0 as isize
    }

    pub fn move_to(&self, screen_x: i32, screen_y: i32) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    screen_x,
                    screen_y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
    }
}

impl Drop for DragGhost {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_window_origin_centers_horizontally_and_hovers_above_cursor() {
        let (x, y) = ghost_window_origin(1000, 500);
        assert_eq!(x, 1000 - GHOST_SIZE / 2);
        assert_eq!(y, 500 - (GHOST_SIZE as f32 * 0.86).round() as i32);
        assert!(y + GHOST_SIZE > 500 && y < 500);
    }
}
