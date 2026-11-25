use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::mem::size_of;
use std::rc::Rc;

use desklite_core::{
    FenceItem, FenceKind, FenceSpec, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, MonitorDescriptor, Point,
    Rect, clamp_rect_to_monitors,
};
use desklite_win32::{IconCache, Win32Error, probe_desktop_window_tree};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Bitmap,
    ID2D1Factory, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient,
    ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_MENU,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect, GWLP_USERDATA,
    HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, HWND_BOTTOM, MA_NOACTIVATE, MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW,
    RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
    TrackPopupMenuEx, WINDOWPOS, WM_APP, WM_CREATE, WM_DROPFILES, WM_ERASEBKGND, WM_EXITSIZEMOVE,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_MOVING, WM_NCLBUTTONDBLCLK, WM_NCRBUTTONUP, WM_NCHITTEST, WM_PAINT,
    WM_RBUTTONUP, WM_SIZE, WM_SIZING, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, w};

use crate::hit_test::{
    HitRegion, LIST_ROW_HEIGHT, ResizeEdge, TITLE_BAR_HEIGHT, content_height, hit_test_point,
    item_rect, max_scroll_offset,
};
use crate::snap::snap_move_rect;
use crate::theme::{
    BORDER_NORMAL, BORDER_PORTAL, HEADER_BG, ITEM_HOVER_BG, ITEM_SELECTED_BG, SCROLLBAR_THUMB,
    TEXT_MUTED, TEXT_PRIMARY, fence_bg_color,
};

pub const WM_APP_PROCESS_ACTIONS: u32 = WM_APP + 10;

const CMD_OPEN_ITEM: u32 = 101;
const CMD_REVEAL_ITEM: u32 = 102;
const CMD_REMOVE_ITEM: u32 = 103;
const CMD_TOGGLE_COLLAPSE: u32 = 201;
const CMD_RENAME_FENCE: u32 = 202;
const CMD_TOGGLE_VIEW: u32 = 203;
const CMD_BIND_PORTAL: u32 = 204;
const CMD_UNBIND_PORTAL: u32 = 205;
const CMD_NEW_BLANK_FENCE: u32 = 206;
const CMD_NEW_PORTAL_FENCE: u32 = 207;
const CMD_DELETE_FENCE: u32 = 208;

#[derive(Debug, Clone)]
pub enum FenceAction {
    MovedOrResized {
        fence_id: i32,
        rect: Rect,
    },
    ToggleCollapse {
        fence_id: i32,
    },
    ToggleViewKind {
        fence_id: i32,
    },
    RenameFence {
        fence_id: i32,
    },
    DeleteFence {
        fence_id: i32,
    },
    CreateBlankFence,
    CreatePortalFence,
    BindPortalDir {
        fence_id: i32,
    },
    UnbindPortalDir {
        fence_id: i32,
    },
    DropFiles {
        fence_id: i32,
        paths: Vec<String>,
    },
    ReorderOrTransferItem {
        from_fence_id: i32,
        item_index: usize,
        screen_pt: Point,
    },
    RemoveItem {
        fence_id: i32,
        item_index: usize,
    },
    LaunchItem {
        path: String,
    },
    RevealItem {
        path: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct LayoutQueryContext {
    pub monitors: Vec<MonitorDescriptor>,
    pub fence_rects: HashMap<i32, Rect>,
    pub snap_threshold_px: i32,
    pub cell_w: i32,
    pub cell_h: i32,
}

pub struct SharedUiResources {
    pub d2d_factory: ID2D1Factory,
    pub dwrite_factory: IDWriteFactory,
    pub title_format: IDWriteTextFormat,
    pub item_grid_format: IDWriteTextFormat,
    pub item_list_format: IDWriteTextFormat,
    pub icon_cache: IconCache,
    pub controller_hwnd: HWND,
    pub pending_actions: Rc<RefCell<VecDeque<FenceAction>>>,
    pub query_ctx: Rc<RefCell<LayoutQueryContext>>,
}

impl SharedUiResources {
    pub fn new(
        controller_hwnd: HWND,
        pending_actions: Rc<RefCell<VecDeque<FenceAction>>>,
        query_ctx: Rc<RefCell<LayoutQueryContext>>,
    ) -> Result<Self, Win32Error> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|e| Win32Error::Win {
                    op: "D2D1CreateFactory",
                    source: e,
                })?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map_err(|e| {
                Win32Error::Win {
                    op: "DWriteCreateFactory",
                    source: e,
                }
            })?;

        let title_format = unsafe {
            let fmt = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.5,
                    w!("zh-CN"),
                )
                .map_err(|e| Win32Error::Win {
                    op: "CreateTextFormat(title)",
                    source: e,
                })?;
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            if let Ok(ellipsis) = dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let _ = fmt.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    Some(&ellipsis),
                );
            }
            fmt
        };

        let item_grid_format = unsafe {
            let fmt = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    11.5,
                    w!("zh-CN"),
                )
                .map_err(|e| Win32Error::Win {
                    op: "CreateTextFormat(grid)",
                    source: e,
                })?;
            let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            if let Ok(ellipsis) = dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let _ = fmt.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    Some(&ellipsis),
                );
            }
            fmt
        };

        let item_list_format = unsafe {
            let fmt = dwrite_factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    12.5,
                    w!("zh-CN"),
                )
                .map_err(|e| Win32Error::Win {
                    op: "CreateTextFormat(list)",
                    source: e,
                })?;
            let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if let Ok(ellipsis) = dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let _ = fmt.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    Some(&ellipsis),
                );
            }
            fmt
        };

        let icon_cache = IconCache::new()?;
        Ok(Self {
            d2d_factory,
            dwrite_factory,
            title_format,
            item_grid_format,
            item_list_format,
            icon_cache,
            controller_hwnd,
            pending_actions,
            query_ctx,
        })
    }

    fn push_action(&self, action: FenceAction) {
        self.pending_actions.borrow_mut().push_back(action);
        unsafe {
            let _ = PostMessageW(
                Some(self.controller_hwnd),
                WM_APP_PROCESS_ACTIONS,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    item_index: usize,
    start_x: i32,
    start_y: i32,
    dragging: bool,
}

struct FenceWindowInner {
    spec: FenceSpec,
    shared: Rc<RefCell<SharedUiResources>>,
    render_target: Option<ID2D1HwndRenderTarget>,
    bitmap_cache: HashMap<String, ID2D1Bitmap>,
    hover_index: Option<usize>,
    selected_index: Option<usize>,
    scroll_y: i32,
    tracking_leave: bool,
    drag_state: Option<DragState>,
}

pub struct FenceWindow {
    hwnd: HWND,
    state: *mut FenceWindowInner,
}

impl FenceWindow {
    pub fn register_class() -> Result<(), Win32Error> {
        let hinst = unsafe { GetModuleHandleW(None) }.map_err(|e| Win32Error::Win {
            op: "GetModuleHandleW",
            source: e,
        })?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: windows::Win32::UI::WindowsAndMessaging::CS_DBLCLKS,
            lpfnWndProc: Some(fence_wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("DeskLiteFenceWnd"),
            ..Default::default()
        };
        let _ = unsafe { RegisterClassExW(&wc) };
        Ok(())
    }

    pub fn create(
        spec: FenceSpec,
        shared: Rc<RefCell<SharedUiResources>>,
    ) -> Result<Self, Win32Error> {
        let hinst = unsafe { GetModuleHandleW(None) }.map_err(|e| Win32Error::Win {
            op: "GetModuleHandleW",
            source: e,
        })?;

        let display_rect = effective_window_rect(&spec);
        let desktop_owner = probe_desktop_window_tree().map(|t| t.worker_w.unwrap_or(t.progman));

        let inner = Box::new(FenceWindowInner {
            spec: spec.clone(),
            shared,
            render_target: None,
            bitmap_cache: HashMap::new(),
            hover_index: None,
            selected_index: None,
            scroll_y: 0,
            tracking_leave: false,
            drag_state: None,
        });
        let raw_ptr = Box::into_raw(inner);
        let title_w = HSTRING::from(&spec.name);

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("DeskLiteFenceWnd"),
                &title_w,
                WS_POPUP,
                display_rect.left,
                display_rect.top,
                display_rect.width().max(MIN_FENCE_WIDTH),
                display_rect.height().max(TITLE_BAR_HEIGHT),
                desktop_owner,
                None,
                Some(hinst.into()),
                Some(raw_ptr.cast()),
            )
        }
        .map_err(|e| {
            unsafe {
                drop(Box::from_raw(raw_ptr));
            }
            Win32Error::Win {
                op: "CreateWindowExW(Fence)",
                source: e,
            }
        })?;

        apply_win11_backdrop(hwnd);
        unsafe {
            DragAcceptFiles(hwnd, true);
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

        Ok(Self {
            hwnd,
            state: raw_ptr,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn update_spec(&mut self, spec: FenceSpec) {
        let inner = unsafe { &mut *self.state };
        let old_items_len = inner.spec.items.len();
        inner.spec = spec;
        if inner.spec.items.len() != old_items_len {
            inner.bitmap_cache.clear();
        }
        let target = effective_window_rect(&inner.spec);
        let mut cur = RECT::default();
        let _ = unsafe { GetWindowRect(self.hwnd, &mut cur) };
        if cur.left != target.left
            || cur.top != target.top
            || cur.right != target.right
            || cur.bottom != target.bottom
        {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_BOTTOM),
                    target.left,
                    target.top,
                    target.width().max(MIN_FENCE_WIDTH),
                    target.height().max(TITLE_BAR_HEIGHT),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }
        self.clamp_scroll();
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn clamp_scroll(&mut self) {
        let inner = unsafe { &mut *self.state };
        inner.clamp_scroll_for_hwnd(self.hwnd);
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn item_at_screen_point(&self, screen_pt: Point) -> Option<usize> {
        let mut pt = POINT {
            x: screen_pt.x,
            y: screen_pt.y,
        };
        if !unsafe { ScreenToClient(self.hwnd, &mut pt) }.as_bool() {
            return None;
        }
        let inner = unsafe { &*self.state };
        match inner.hit_test_client(self.hwnd, pt.x, pt.y) {
            HitRegion::Item(idx) => Some(idx),
            _ => None,
        }
    }
}

impl Drop for FenceWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state));
        }
    }
}

fn effective_window_rect(spec: &FenceSpec) -> Rect {
    if spec.collapsed {
        Rect::new(
            spec.rect.left,
            spec.rect.top,
            spec.rect.right,
            spec.rect.top + TITLE_BAR_HEIGHT,
        )
    } else {
        spec.rect
    }
}

fn apply_win11_backdrop(hwnd: HWND) {
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

impl FenceWindowInner {
    fn client_size(&self, hwnd: HWND) -> (i32, i32) {
        let mut rc = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        ((rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1))
    }

    fn clamp_scroll_for_hwnd(&mut self, hwnd: HWND) {
        let (w, h) = self.client_size(hwnd);
        let (cell_w, cell_h) = {
            let shared = self.shared.borrow();
            let q = shared.query_ctx.borrow();
            (q.cell_w, q.cell_h)
        };
        let ch = content_height(self.spec.kind, w, self.spec.items.len(), cell_w, cell_h);
        let max_s = max_scroll_offset(ch, h);
        self.scroll_y = self.scroll_y.clamp(0, max_s);
    }

    fn hit_test_client(&self, hwnd: HWND, x: i32, y: i32) -> HitRegion {
        let (w, h) = self.client_size(hwnd);
        let (cell_w, cell_h) = {
            let shared = self.shared.borrow();
            let q = shared.query_ctx.borrow();
            (q.cell_w, q.cell_h)
        };
        hit_test_point(
            w,
            h,
            x,
            y,
            self.spec.collapsed,
            self.spec.kind,
            self.spec.items.len(),
            cell_w,
            cell_h,
            self.scroll_y,
        )
    }

    fn ensure_target(&mut self, hwnd: HWND) -> Option<ID2D1HwndRenderTarget> {
        if self.render_target.is_none() {
            let (w, h) = self.client_size(hwnd);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                ..Default::default()
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U {
                    width: w as u32,
                    height: h as u32,
                },
                ..Default::default()
            };
            let rt = {
                let shared = self.shared.borrow();
                unsafe { shared.d2d_factory.CreateHwndRenderTarget(&props, &hwnd_props) }.ok()?
            };
            self.render_target = Some(rt);
        }
        self.render_target.clone()
    }

    fn get_or_upload_bitmap(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        item: &FenceItem,
    ) -> Option<ID2D1Bitmap> {
        let key = item.path.to_ascii_lowercase();
        if let Some(bmp) = self.bitmap_cache.get(&key) {
            return Some(bmp.clone());
        }
        let mut shared = self.shared.borrow_mut();
        let decoded = shared.icon_cache.get_or_load(&item.path)?.clone();
        drop(shared);

        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bmp = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U {
                    width: decoded.width,
                    height: decoded.height,
                },
                Some(decoded.pbgra.as_ptr().cast()),
                decoded.width * 4,
                &props,
            )
        }
        .ok()?;
        self.bitmap_cache.insert(key, bmp.clone());
        Some(bmp)
    }

    fn paint(&mut self, hwnd: HWND) {
        let (w_i, h_i) = self.client_size(hwnd);
        let w = w_i as f32;
        let h = h_i as f32;

        let (title_fmt, grid_fmt, list_fmt, cell_w, cell_h) = {
            let shared = self.shared.borrow();
            let q = shared.query_ctx.borrow();
            (
                shared.title_format.clone(),
                shared.item_grid_format.clone(),
                shared.item_list_format.clone(),
                q.cell_w,
                q.cell_h,
            )
        };

        let Some(rt) = self.ensure_target(hwnd) else {
            return;
        };

        // Pre-warm bitmaps before BeginDraw
        let items_snapshot = self.spec.items.clone();
        let mut bitmaps = Vec::with_capacity(items_snapshot.len());
        if !self.spec.collapsed {
            for it in &items_snapshot {
                bitmaps.push(self.get_or_upload_bitmap(&rt, it));
            }
        }

        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&fence_bg_color(self.spec.backdrop_alpha)));

            if let Ok(header_brush) = rt.CreateSolidColorBrush(&HEADER_BG, None) {
                let header_rr = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 1.0,
                        top: 1.0,
                        right: (w - 1.0).max(2.0),
                        bottom: (TITLE_BAR_HEIGHT as f32).min(h),
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                };
                rt.FillRoundedRectangle(&header_rr, &header_brush);
            }

            let border_color = if self.spec.portal_dir.is_some() {
                BORDER_PORTAL
            } else {
                BORDER_NORMAL
            };
            if let Ok(border_brush) = rt.CreateSolidColorBrush(&border_color, None) {
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

            let text_brush = rt.CreateSolidColorBrush(&TEXT_PRIMARY, None).ok();
            let muted_brush = rt.CreateSolidColorBrush(&TEXT_MUTED, None).ok();
            let hover_brush = rt.CreateSolidColorBrush(&ITEM_HOVER_BG, None).ok();
            let sel_brush = rt.CreateSolidColorBrush(&ITEM_SELECTED_BG, None).ok();

            if let Some(ref tb) = text_brush {
                let title = if self.spec.name.is_empty() {
                    "未命名格子".to_string()
                } else if self.spec.portal_dir.is_some() {
                    format!("{} [文件夹]", self.spec.name)
                } else {
                    self.spec.name.clone()
                };
                let title_u16: Vec<u16> = title.encode_utf16().collect();
                let title_rect = D2D_RECT_F {
                    left: 12.0,
                    top: 2.0,
                    right: (w - 36.0).max(20.0),
                    bottom: TITLE_BAR_HEIGHT as f32,
                };
                rt.DrawText(
                    &title_u16,
                    &title_fmt,
                    &title_rect,
                    tb,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );

                if let Some(ref mb) = muted_brush {
                    let badge = if self.spec.collapsed {
                        "▸".to_string()
                    } else {
                        format!("{}", items_snapshot.len())
                    };
                    let badge_u16: Vec<u16> = badge.encode_utf16().collect();
                    let badge_rect = D2D_RECT_F {
                        left: (w - 38.0).max(12.0),
                        top: 2.0,
                        right: (w - 10.0).max(20.0),
                        bottom: TITLE_BAR_HEIGHT as f32,
                    };
                    rt.DrawText(
                        &badge_u16,
                        &title_fmt,
                        &badge_rect,
                        mb,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }

            if !self.spec.collapsed {
                let body_clip = D2D_RECT_F {
                    left: 2.0,
                    top: TITLE_BAR_HEIGHT as f32,
                    right: (w - 2.0).max(4.0),
                    bottom: (h - 2.0).max(TITLE_BAR_HEIGHT as f32 + 2.0),
                };
                rt.PushAxisAlignedClip(
                    &body_clip,
                    windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                );

                if items_snapshot.is_empty()
                    && let Some(ref mb) = muted_brush
                {
                    let hint: Vec<u16> = "拖入文件或快捷方式到这里".encode_utf16().collect();
                    let hint_rc = D2D_RECT_F {
                        left: 16.0,
                        top: (TITLE_BAR_HEIGHT + 24) as f32,
                        right: (w - 16.0).max(24.0),
                        bottom: (h - 12.0).max(60.0),
                    };
                    rt.DrawText(
                        &hint,
                        &grid_fmt,
                        &hint_rc,
                        mb,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }

                for (i, item) in items_snapshot.iter().enumerate() {
                    let (il, it, ir, ib) =
                        item_rect(i, self.spec.kind, w_i, cell_w, cell_h, self.scroll_y);
                    if ib < TITLE_BAR_HEIGHT || it > h_i {
                        continue;
                    }

                    let bg = if self.selected_index == Some(i) {
                        sel_brush.as_ref()
                    } else if self.hover_index == Some(i) {
                        hover_brush.as_ref()
                    } else {
                        None
                    };
                    if let Some(brush) = bg {
                        let rr = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: (il + 2) as f32,
                                top: (it + 2) as f32,
                                right: (ir - 2) as f32,
                                bottom: (ib - 2) as f32,
                            },
                            radiusX: 6.0,
                            radiusY: 6.0,
                        };
                        rt.FillRoundedRectangle(&rr, brush);
                    }

                    match self.spec.kind {
                        FenceKind::FileList => {
                            let icon_sz = 20.0_f32;
                            let icon_x = (il + 8) as f32;
                            let icon_y = it as f32 + (LIST_ROW_HEIGHT as f32 - icon_sz) / 2.0;
                            if let Some(Some(bmp)) = bitmaps.get(i) {
                                let dst = D2D_RECT_F {
                                    left: icon_x,
                                    top: icon_y,
                                    right: icon_x + icon_sz,
                                    bottom: icon_y + icon_sz,
                                };
                                rt.DrawBitmap(
                                    bmp,
                                    Some(&dst),
                                    1.0,
                                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                    None,
                                );
                            }
                            if let Some(ref tb) = text_brush {
                                let name_u16: Vec<u16> = item.display_name.encode_utf16().collect();
                                let tr = D2D_RECT_F {
                                    left: icon_x + icon_sz + 8.0,
                                    top: it as f32,
                                    right: (ir - 8) as f32,
                                    bottom: ib as f32,
                                };
                                rt.DrawText(
                                    &name_u16,
                                    &list_fmt,
                                    &tr,
                                    tb,
                                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                    DWRITE_MEASURING_MODE_NATURAL,
                                );
                            }
                        }
                        FenceKind::IconGrid | FenceKind::Unknown(_) => {
                            let icon_sz = 42.0_f32;
                            let icon_x = (il + ir) as f32 / 2.0 - icon_sz / 2.0;
                            let icon_y = (it + 8) as f32;
                            if let Some(Some(bmp)) = bitmaps.get(i) {
                                let dst = D2D_RECT_F {
                                    left: icon_x,
                                    top: icon_y,
                                    right: icon_x + icon_sz,
                                    bottom: icon_y + icon_sz,
                                };
                                rt.DrawBitmap(
                                    bmp,
                                    Some(&dst),
                                    1.0,
                                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                    None,
                                );
                            }
                            if let Some(ref tb) = text_brush {
                                let name_u16: Vec<u16> = item.display_name.encode_utf16().collect();
                                let tr = D2D_RECT_F {
                                    left: (il + 4) as f32,
                                    top: icon_y + icon_sz + 4.0,
                                    right: (ir - 4) as f32,
                                    bottom: (ib - 2) as f32,
                                };
                                rt.DrawText(
                                    &name_u16,
                                    &grid_fmt,
                                    &tr,
                                    tb,
                                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                    DWRITE_MEASURING_MODE_NATURAL,
                                );
                            }
                        }
                    }
                }

                // Draw slim scrollbar thumb when scrollable
                let ch = content_height(self.spec.kind, w_i, items_snapshot.len(), cell_w, cell_h);
                let max_s = max_scroll_offset(ch, h_i);
                if max_s > 0
                    && let Ok(sb_brush) = rt.CreateSolidColorBrush(&SCROLLBAR_THUMB, None)
                {
                    let body_h = (h_i - TITLE_BAR_HEIGHT).max(1) as f32;
                    let ratio = (body_h / ch.max(1) as f32).clamp(0.12, 1.0);
                    let thumb_h = (body_h * ratio).max(20.0);
                    let progress = self.scroll_y as f32 / max_s as f32;
                    let thumb_top =
                        TITLE_BAR_HEIGHT as f32 + progress * (body_h - thumb_h - 4.0) + 2.0;
                    let thumb_rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: w - 6.0,
                            top: thumb_top,
                            right: w - 2.5,
                            bottom: thumb_top + thumb_h,
                        },
                        radiusX: 2.0,
                        radiusY: 2.0,
                    };
                    rt.FillRoundedRectangle(&thumb_rr, &sb_brush);
                }

                rt.PopAxisAlignedClip();
            }

            if rt.EndDraw(None, None).is_err() {
                self.render_target = None;
                self.bitmap_cache.clear();
            }
        }
    }

    fn show_context_menu(&self, hwnd: HWND, item_index: Option<usize>) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };

        unsafe {
            if let Some(idx) = item_index
                && idx < self.spec.items.len()
            {
                let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_ITEM as usize, w!("打开"));
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    CMD_REVEAL_ITEM as usize,
                    w!("打开所在位置"),
                );
                if self.spec.portal_dir.is_none() {
                    let _ = AppendMenuW(
                        menu,
                        MF_STRING,
                        CMD_REMOVE_ITEM as usize,
                        w!("从格子移除"),
                    );
                } else {
                    let _ = AppendMenuW(
                        menu,
                        MF_STRING | MF_GRAYED,
                        CMD_REMOVE_ITEM as usize,
                        w!("文件夹映射条目由磁盘同步"),
                    );
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            }

            let collapse_lbl = if self.spec.collapsed {
                w!("展开格子")
            } else {
                w!("折叠格子")
            };
            let view_lbl = match self.spec.kind {
                FenceKind::FileList => w!("切换为图标网格视图"),
                _ => w!("切换为紧凑列表视图"),
            };

            let _ = AppendMenuW(menu, MF_STRING, CMD_TOGGLE_COLLAPSE as usize, collapse_lbl);
            let _ = AppendMenuW(menu, MF_STRING, CMD_RENAME_FENCE as usize, w!("重命名格子"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_TOGGLE_VIEW as usize, view_lbl);
            if self.spec.portal_dir.is_some() {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    CMD_UNBIND_PORTAL as usize,
                    w!("取消文件夹映射"),
                );
            } else {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    CMD_BIND_PORTAL as usize,
                    w!("绑定为文件夹映射..."),
                );
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_NEW_BLANK_FENCE as usize,
                w!("新建空白格子"),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_NEW_PORTAL_FENCE as usize,
                w!("新建文件夹映射格子..."),
            );
            let _ = AppendMenuW(menu, MF_STRING, CMD_DELETE_FENCE as usize, w!("删除此格子"));
        }

        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let cmd = unsafe {
            TrackPopupMenuEx(
                menu,
                (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0,
                pt.x,
                pt.y,
                hwnd,
                None,
            )
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }

        let cmd_id = cmd.0 as u32;
        if cmd_id == 0 {
            return;
        }
        let fid = self.spec.id;
        let shared = self.shared.borrow();
        match cmd_id {
            CMD_OPEN_ITEM => {
                if let Some(idx) = item_index
                    && let Some(it) = self.spec.items.get(idx)
                {
                    shared.push_action(FenceAction::LaunchItem {
                        path: it.path.clone(),
                    });
                }
            }
            CMD_REVEAL_ITEM => {
                if let Some(idx) = item_index
                    && let Some(it) = self.spec.items.get(idx)
                {
                    shared.push_action(FenceAction::RevealItem {
                        path: it.path.clone(),
                    });
                }
            }
            CMD_REMOVE_ITEM => {
                if let Some(idx) = item_index {
                    shared.push_action(FenceAction::RemoveItem {
                        fence_id: fid,
                        item_index: idx,
                    });
                }
            }
            CMD_TOGGLE_COLLAPSE => shared.push_action(FenceAction::ToggleCollapse { fence_id: fid }),
            CMD_RENAME_FENCE => shared.push_action(FenceAction::RenameFence { fence_id: fid }),
            CMD_TOGGLE_VIEW => shared.push_action(FenceAction::ToggleViewKind { fence_id: fid }),
            CMD_BIND_PORTAL => shared.push_action(FenceAction::BindPortalDir { fence_id: fid }),
            CMD_UNBIND_PORTAL => shared.push_action(FenceAction::UnbindPortalDir { fence_id: fid }),
            CMD_NEW_BLANK_FENCE => shared.push_action(FenceAction::CreateBlankFence),
            CMD_NEW_PORTAL_FENCE => shared.push_action(FenceAction::CreatePortalFence),
            CMD_DELETE_FENCE => shared.push_action(FenceAction::DeleteFence { fence_id: fid }),
            _ => {}
        }
    }
}

unsafe extern "system" fn fence_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        return LRESULT(0);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut FenceWindowInner;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let inner = unsafe { &mut *ptr };

    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_WINDOWPOSCHANGING => {
            let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
            if (wp.flags & SWP_NOZORDER).0 == 0 {
                wp.hwndInsertAfter = HWND_BOTTOM;
            }
            wp.flags |= SWP_NOACTIVATE;
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let sx = (lparam.0 & 0xffff) as i16 as i32;
            let sy = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let mut pt = POINT { x: sx, y: sy };
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            let hit = inner.hit_test_client(hwnd, pt.x, pt.y);
            let code = match hit {
                HitRegion::Resize(ResizeEdge::Left) => HTLEFT,
                HitRegion::Resize(ResizeEdge::Right) => HTRIGHT,
                HitRegion::Resize(ResizeEdge::Top) => HTTOP,
                HitRegion::Resize(ResizeEdge::Bottom) => HTBOTTOM,
                HitRegion::Resize(ResizeEdge::TopLeft) => HTTOPLEFT,
                HitRegion::Resize(ResizeEdge::TopRight) => HTTOPRIGHT,
                HitRegion::Resize(ResizeEdge::BottomLeft) => HTBOTTOMLEFT,
                HitRegion::Resize(ResizeEdge::BottomRight) => HTBOTTOMRIGHT,
                HitRegion::TitleBar => HTCAPTION,
                HitRegion::Item(_) | HitRegion::EmptyBody => HTCLIENT,
            };
            LRESULT(code as isize)
        }
        WM_NCLBUTTONDBLCLK => {
            if wparam.0 as u32 == HTCAPTION {
                let fid = inner.spec.id;
                inner
                    .shared
                    .borrow()
                    .push_action(FenceAction::ToggleCollapse { fence_id: fid });
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCRBUTTONUP => {
            if wparam.0 as u32 == HTCAPTION {
                inner.show_context_menu(hwnd, None);
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOVING => {
            let rc = unsafe { &mut *(lparam.0 as *mut RECT) };
            let proposed = Rect::new(rc.left, rc.top, rc.right, rc.bottom);
            let alt_held = unsafe { GetKeyState(i32::from(VK_MENU.0)) } < 0;
            let snapped = {
                let shared = inner.shared.borrow();
                let q = shared.query_ctx.borrow();
                let siblings: Vec<Rect> = q
                    .fence_rects
                    .iter()
                    .filter(|&(&id, _)| id != inner.spec.id)
                    .map(|(_, &r)| r)
                    .collect();
                snap_move_rect(
                    proposed,
                    &siblings,
                    &q.monitors,
                    q.snap_threshold_px,
                    alt_held,
                )
            };
            rc.left = snapped.left;
            rc.top = snapped.top;
            rc.right = snapped.right;
            rc.bottom = snapped.bottom;
            LRESULT(1)
        }
        WM_SIZING => {
            let rc = unsafe { &mut *(lparam.0 as *mut RECT) };
            let min_h = if inner.spec.collapsed {
                TITLE_BAR_HEIGHT
            } else {
                MIN_FENCE_HEIGHT
            };
            if rc.right - rc.left < MIN_FENCE_WIDTH {
                rc.right = rc.left + MIN_FENCE_WIDTH;
            }
            if rc.bottom - rc.top < min_h {
                rc.bottom = rc.top + min_h;
            }
            let proposed = Rect::new(rc.left, rc.top, rc.right, rc.bottom);
            let monitors = inner.shared.borrow().query_ctx.borrow().monitors.clone();
            if !monitors.is_empty()
                && let Ok((clamped, _)) = clamp_rect_to_monitors(proposed, &monitors)
            {
                rc.left = clamped.left;
                rc.top = clamped.top;
                rc.right = clamped.right;
                rc.bottom = clamped.bottom;
            }
            LRESULT(1)
        }
        WM_EXITSIZEMOVE => {
            let mut wr = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut wr) }.is_ok() {
                let rect = if inner.spec.collapsed {
                    Rect::new(
                        wr.left,
                        wr.top,
                        wr.right,
                        wr.top + inner.spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    Rect::new(wr.left, wr.top, wr.right, wr.bottom)
                };
                inner.spec.rect = rect;
                inner.clamp_scroll_for_hwnd(hwnd);
                let fid = inner.spec.id;
                inner
                    .shared
                    .borrow()
                    .push_action(FenceAction::MovedOrResized { fence_id: fid, rect });
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if !inner.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
                    inner.tracking_leave = true;
                }
            }
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(ref mut ds) = inner.drag_state
                && !ds.dragging
                && ((x - ds.start_x).abs() > 6 || (y - ds.start_y).abs() > 6)
            {
                ds.dragging = true;
            }
            let new_hover = match inner.hit_test_client(hwnd, x, y) {
                HitRegion::Item(idx) => Some(idx),
                _ => None,
            };
            if new_hover != inner.hover_index {
                inner.hover_index = new_hover;
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            inner.tracking_leave = false;
            if inner.hover_index.is_some() {
                inner.hover_index = None;
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let hit = inner.hit_test_client(hwnd, x, y);
            let new_sel = match hit {
                HitRegion::Item(idx) => {
                    if inner.spec.portal_dir.is_none() {
                        inner.drag_state = Some(DragState {
                            item_index: idx,
                            start_x: x,
                            start_y: y,
                            dragging: false,
                        });
                        unsafe {
                            SetCapture(hwnd);
                        }
                    }
                    Some(idx)
                }
                _ => None,
            };
            if new_sel != inner.selected_index {
                inner.selected_index = new_sel;
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(ds) = inner.drag_state.take() {
                unsafe {
                    let _ = ReleaseCapture();
                }
                if ds.dragging {
                    let x = (lparam.0 & 0xffff) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                    let mut pt = POINT { x, y };
                    let _ = unsafe { ClientToScreen(hwnd, &mut pt) };
                    let fid = inner.spec.id;
                    inner.shared.borrow().push_action(
                        FenceAction::ReorderOrTransferItem {
                            from_fence_id: fid,
                            item_index: ds.item_index,
                            screen_pt: Point { x: pt.x, y: pt.y },
                        },
                    );
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let HitRegion::Item(idx) = inner.hit_test_client(hwnd, x, y)
                && let Some(it) = inner.spec.items.get(idx)
            {
                inner.shared.borrow().push_action(FenceAction::LaunchItem {
                    path: it.path.clone(),
                });
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            let hit_item = match inner.hit_test_client(hwnd, x, y) {
                HitRegion::Item(idx) => {
                    inner.selected_index = Some(idx);
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    Some(idx)
                }
                _ => None,
            };
            inner.show_context_menu(hwnd, hit_item);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if !inner.spec.collapsed {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                let step = -(delta * 42) / 120;
                inner.scroll_y = inner.scroll_y.saturating_add(step);
                inner.clamp_scroll_for_hwnd(hwnd);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            let hdrop = HDROP(wparam.0 as *mut _);
            let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let len = unsafe { DragQueryFileW(hdrop, i, None) };
                if len > 0 {
                    let mut buf = vec![0u16; len as usize + 1];
                    let got = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
                    if got > 0 {
                        paths.push(String::from_utf16_lossy(&buf[..got as usize]));
                    }
                }
            }
            unsafe {
                DragFinish(hdrop);
            }
            if !paths.is_empty() {
                let fid = inner.spec.id;
                inner
                    .shared
                    .borrow()
                    .push_action(FenceAction::DropFiles { fence_id: fid, paths });
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(rt) = &inner.render_target {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                let _ = unsafe {
                    rt.Resize(&D2D_SIZE_U {
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
            inner.paint(hwnd);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
