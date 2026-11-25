use std::mem::size_of;

use desklite_core::Rect;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Factory,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ValidateRect};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, GetWindowLongPtrW, HWND_BOTTOM, KillTimer, PostQuitMessage, RegisterClassExW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WINDOWPOS, WM_CLOSE, WM_CREATE,
    WM_DESTROY, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_TIMER, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, Interface, w};

use crate::Win32Error;
use crate::desktop_listview::probe_desktop_window_tree;
use crate::win_err;

const REORDER_TIMER_ID: usize = 2;

#[derive(Debug, Clone)]
pub struct FencePocConfig {
    pub title: String,
    pub rect: Rect,
}

impl Default for FencePocConfig {
    fn default() -> Self {
        Self {
            title: "DeskLite PoC Fence (Esc/Close to exit)".to_string(),
            rect: Rect::new(80, 80, 420, 320),
        }
    }
}

struct PocWindowState {
    title: String,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    text_format: IDWriteTextFormat,
    render_target: Option<ID2D1HwndRenderTarget>,
}

impl PocWindowState {
    fn new(title: String) -> Result<Self, Win32Error> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|e| win_err("D2D1CreateFactory", e))?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|e| win_err("DWriteCreateFactory", e))?;
        let text_format = unsafe {
            dwrite_factory.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.0,
                w!("zh-CN"),
            )
        }
        .map_err(|e| win_err("CreateTextFormat", e))?;

        Ok(Self {
            title,
            d2d_factory,
            dwrite_factory,
            text_format,
            render_target: None,
        })
    }

    fn ensure_target(&mut self, hwnd: HWND) -> Result<&ID2D1HwndRenderTarget, Win32Error> {
        if self.render_target.is_none() {
            let mut rc = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let width = (rc.right - rc.left).max(1) as u32;
            let height = (rc.bottom - rc.top).max(1) as u32;
            let props = D2D1_RENDER_TARGET_PROPERTIES::default();
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U { width, height },
                ..Default::default()
            };
            let rt = unsafe { self.d2d_factory.CreateHwndRenderTarget(&props, &hwnd_props) }
                .map_err(|e| win_err("CreateHwndRenderTarget", e))?;
            self.render_target = Some(rt);
        }
        Ok(self.render_target.as_ref().expect("render target initialized"))
    }

    fn paint(&mut self, hwnd: HWND) {
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        let w = (rc.right - rc.left).max(1) as f32;
        let h = (rc.bottom - rc.top).max(1) as f32;
        let title_utf16: Vec<u16> = self.title.encode_utf16().collect();

        let _ = self.dwrite_factory.as_raw();
        let text_format = self.text_format.clone();
        let Ok(rt) = self.ensure_target(hwnd).cloned() else {
            return;
        };

        unsafe {
            rt.BeginDraw();
            // Transparent background so DWM Acrylic backdrop shows through cleanly
            rt.Clear(Some(&D2D1_COLOR_F {
                r: 0.08,
                g: 0.09,
                b: 0.12,
                a: 0.45,
            }));

            if let Ok(header_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.14,
                    g: 0.16,
                    b: 0.22,
                    a: 0.72,
                },
                None,
            ) {
                let header_rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 1.0,
                        top: 1.0,
                        right: (w - 1.0).max(2.0),
                        bottom: 34.0_f32.min(h),
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                };
                rt.FillRoundedRectangle(&header_rr, &header_brush);
            }

            if let Ok(border_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 0.22,
                },
                None,
            ) {
                let border_rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 1.0,
                        top: 1.0,
                        right: (w - 1.0).max(2.0),
                        bottom: (h - 1.0).max(2.0),
                    },
                    radiusX: 10.0,
                    radiusY: 10.0,
                };
                rt.DrawRoundedRectangle(&border_rr, &border_brush, 1.0, None);
            }

            if let Ok(text_brush) = rt.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.95,
                    g: 0.97,
                    b: 1.0,
                    a: 0.96,
                },
                None,
            ) {
                let title_rect = D2D_RECT_F {
                    left: 12.0,
                    top: 7.0,
                    right: (w - 12.0).max(16.0),
                    bottom: 30.0,
                };
                rt.DrawText(
                    &title_utf16,
                    &text_format,
                    &title_rect,
                    &text_brush,
                    windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            if rt.EndDraw(None, None).is_err() {
                self.render_target = None;
            }
        }
    }
}

unsafe extern "system" fn poc_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA;

    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        unsafe { SetTimer(Some(hwnd), REORDER_TIMER_ID, 250, None) };
        return LRESULT(0);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut PocWindowState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_WINDOWPOSCHANGING => {
            let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
            if (wp.flags & SWP_NOZORDER).0 == 0 {
                wp.hwndInsertAfter = HWND_BOTTOM;
            }
            wp.flags |= SWP_NOACTIVATE;
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == REORDER_TIMER_ID => {
            // Keep bottom-most on desktop without stealing focus or responding to Win+D minimize.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_BOTTOM),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(rt) = &state.render_target {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                let _ = unsafe {
                    rt.Resize(&windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: w.max(1),
                        height: h.max(1),
                    })
                };
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let _ = BeginPaint(hwnd, &mut ps);
            }
            state.paint(hwnd);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), REORDER_TIMER_ID);
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_NCDESTROY | WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn enable_win11_acrylic_and_rounded_corners(hwnd: HWND) {
    unsafe {
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        let dark: BOOL = true.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            size_of::<BOOL>() as u32,
        );

        let backdrop = DWMSBT_TRANSIENTWINDOW;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const backdrop).cast(),
            size_of::<i32>() as u32,
        );

        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corner).cast(),
            size_of::<i32>() as u32,
        );
    }
}

/// Launches a transparent Direct2D + Win11 DWM Acrylic bottom-most PoC fence window.
/// Returns after the user closes the PoC window or `Ctrl+C`.
pub fn run_bottom_fence_poc(cfg: FencePocConfig) -> Result<(), Win32Error> {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(|e| win_err("GetModuleHandleW", e))?;

    let class_name = w!("DeskLitePocFenceWnd");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(poc_wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassExW(&wc) };

    let mut state = Box::new(PocWindowState::new(cfg.title.clone())?);
    let title_w = HSTRING::from(&cfg.title);

    // Ownership/parenting test: keep desktop Progman as owner so Win+D does not hide the fence.
    let desktop_owner = probe_desktop_window_tree().map(|t| t.progman);

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            &title_w,
            WS_POPUP,
            cfg.rect.left,
            cfg.rect.top,
            cfg.rect.width(),
            cfg.rect.height(),
            desktop_owner,
            None,
            Some(hinstance.into()),
            Some((&raw mut *state).cast()),
        )
    }
    .map_err(|e| win_err("CreateWindowExW(PocFence)", e))?;

    enable_win11_acrylic_and_rounded_corners(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }

    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        CoUninitialize();
    }
    Ok(())
}
