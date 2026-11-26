//! Native Win32 `WS_EX_LAYERED` + `UpdateLayeredWindow` + Direct2D (`ID2D1DCRenderTarget`)
//! host window for DeskLite's `FreeDesktop` icon layer.
//!
//! Renders desktop icons and marquee selection into a 32bpp top-down DIBSection
//! committed atomically with `ULW_ALPHA`. Blank canvas pixels use `alpha = 1`
//! (`1.0 / 255.0`) so live wallpapers show through cleanly while preserving
//! DWM hit-testing for clicks, marquee selection, double-click hide/show, and
//! Explorer `WM_DROPFILES` drops.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null_mut, slice_from_raw_parts};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use desklite_core::{Point, Rect};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, ClientToScreen, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
    FF_DONTCARE, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT};
use windows::Win32::UI::Controls::{EM_SETSEL, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP,
    RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DLGC_WANTALLKEYS, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOHSCROLL, ES_CENTER, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, PostMessageW, PostQuitMessage, RegisterClassExW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CONTEXTMENU, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETDLGCODE, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT,
    WM_RBUTTONUP, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows::core::{HSTRING, w};

use crate::icons::IconCache;
use crate::window_attach::attach_free_desktop_canvas;
use crate::{Win32Error, win_err};

const FREE_DESKTOP_CLASS_NAME: windows::core::PCWSTR = w!("DeskLiteFreeDesktopWnd");
const FREE_DESKTOP_WINDOW_TITLE: windows::core::PCWSTR = w!("DeskLite_FreeDesktop");

const WM_APP_SYNC_STATE: u32 = WM_APP + 0x201;
const WM_APP_BEGIN_RENAME: u32 = WM_APP + 0x202;
const WM_APP_CLOSE_RENAME: u32 = WM_APP + 0x203;
const WM_APP_SHUTDOWN: u32 = WM_APP + 0x204;

const RENAME_EDIT_SUBCLASS_ID: usize = 0x444C_524E;
const RENAME_EDIT_CTRL_ID: isize = 0x5101;
const DRAG_THRESHOLD_PX: i32 = 4;
const MAX_CACHED_D2D_BITMAPS: usize = 512;

/// Minimum non-zero alpha written to blank pixels so DWM hit-testing captures pointer & drop events
/// while letting the wallpaper show through cleanly.
const HITTEST_ALPHA: f32 = 1.0 / 255.0;

/// Single desktop icon rendered directly on the native `FreeDesktop` layered canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeDesktopRenderItem {
    /// Canonical filesystem path of the item.
    pub path: String,
    /// Display label rendered underneath the icon.
    pub display_name: String,
    /// Top-left cell coordinate in physical screen space.
    pub pos: Point,
    /// Whether the icon is currently selected.
    pub selected: bool,
    /// Whether the icon is marked as cut on the clipboard.
    pub is_cut: bool,
}

/// Complete snapshot pushed from the UI/core controller to the `FreeDesktop` window thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeDesktopSnapshot {
    /// Virtual desktop rectangle in physical screen coordinates.
    pub virtual_bounds: Rect,
    /// Grid cell `(width, height)` in physical pixels.
    pub cell_size: (i32, i32),
    /// When `true`, icons are hidden but the `alpha = 1` hit-test surface stays active.
    pub hide_all_fences: bool,
    /// Icons to draw on the free desktop layer.
    pub icons: Vec<FreeDesktopRenderItem>,
}

impl Default for FreeDesktopSnapshot {
    fn default() -> Self {
        Self {
            virtual_bounds: Rect::new(0, 0, 1, 1),
            cell_size: (84, 96),
            hide_all_fences: false,
            icons: Vec::new(),
        }
    }
}

/// Events emitted by the `FreeDesktop` native Win32 canvas toward the app event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeDesktopEvent {
    IconClicked {
        path: String,
        ctrl: bool,
        shift: bool,
    },
    IconDoubleClicked {
        path: String,
    },
    BlankClicked,
    BlankDoubleClicked,
    MarqueeSelect {
        rect: Rect,
        additive: bool,
    },
    ContextMenuBlank {
        screen_x: i32,
        screen_y: i32,
    },
    ContextMenuIcon {
        path: String,
        screen_x: i32,
        screen_y: i32,
    },
    StartDragIcons {
        start_screen: (i32, i32),
    },
    FilesDroppedFromExplorer {
        paths: Vec<String>,
        screen_x: i32,
        screen_y: i32,
    },
    FocusLost,
    RenameCommitted {
        old_path: String,
        new_name: String,
    },
    RenameCancelled,
}

#[derive(Debug, Clone)]
struct PendingRename {
    path: String,
    current_name: String,
    item_screen_rect: Rect,
}

#[derive(Default)]
struct PendingCommands {
    snapshot: Option<FreeDesktopSnapshot>,
    bounds: Option<Rect>,
    rename: Option<PendingRename>,
}

struct ControllerInner {
    hwnd: Arc<AtomicIsize>,
    pending: Arc<Mutex<PendingCommands>>,
    shutdown_requested: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ControllerInner {
    fn shutdown(&self) {
        if self.shutdown_requested.swap(true, Ordering::AcqRel) {
            // Another caller already requested shutdown; still join below if needed.
        } else {
            let raw = self.hwnd.load(Ordering::Acquire);
            if raw != 0 {
                let hwnd = HWND(raw as *mut c_void);
                let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_SHUTDOWN, WPARAM(0), LPARAM(0)) };
            }
        }

        let handle = {
            let mut guard = lock_unpoisoned(&self.thread);
            guard.take()
        };
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Thread-safe handle controlling the native `FreeDesktop` Win32 layered window thread.
#[derive(Clone)]
pub struct FreeDesktopController {
    inner: Arc<ControllerInner>,
}

impl FreeDesktopController {
    /// Spawns a dedicated Win32 message-loop thread hosting the `DeskLite_FreeDesktop` layered canvas.
    ///
    /// When events are emitted, `PostMessageW(notify_hwnd, notify_msg, 0, 0)` is sent (when both are
    /// non-zero) to wake the host event loop immediately.
    pub fn spawn(
        notify_hwnd: isize,
        notify_msg: u32,
    ) -> Result<
        (
            FreeDesktopController,
            std::sync::mpsc::Receiver<FreeDesktopEvent>,
        ),
        Win32Error,
    > {
        let (event_tx, event_rx) = std::sync::mpsc::channel::<FreeDesktopEvent>();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<isize, Win32Error>>(1);
        let pending = Arc::new(Mutex::new(PendingCommands::default()));
        let hwnd_slot = Arc::new(AtomicIsize::new(0));
        let thread_pending = Arc::clone(&pending);
        let thread_hwnd_slot = Arc::clone(&hwnd_slot);

        let handle = thread::Builder::new()
            .name("desklite-free-desktop".to_string())
            .spawn(move || {
                run_window_thread(
                    notify_hwnd,
                    notify_msg,
                    event_tx,
                    thread_pending,
                    thread_hwnd_slot,
                    ready_tx,
                );
            })
            .map_err(|e| Win32Error::Status {
                op: "spawn free_desktop_wnd thread",
                code: e.raw_os_error().map(|c| c as u32).unwrap_or(u32::MAX),
            })?;

        let raw_hwnd = match ready_rx.recv() {
            Ok(Ok(hwnd)) => hwnd,
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(err);
            }
            Err(_) => {
                let _ = handle.join();
                return Err(Win32Error::Status {
                    op: "free_desktop_wnd startup channel closed",
                    code: u32::MAX,
                });
            }
        };

        hwnd_slot.store(raw_hwnd, Ordering::Release);
        let inner = Arc::new(ControllerInner {
            hwnd: hwnd_slot,
            pending,
            shutdown_requested: AtomicBool::new(false),
            thread: Mutex::new(Some(handle)),
        });
        Ok((Self { inner }, event_rx))
    }

    /// Returns the native `HWND` handle of the `FreeDesktop` layered window (`0` after destroy).
    pub fn hwnd(&self) -> isize {
        self.inner.hwnd.load(Ordering::Acquire)
    }

    /// Queues a new icon/bounds snapshot and triggers an asynchronous D2D redraw + `UpdateLayeredWindow`.
    pub fn update_snapshot(&self, snapshot: FreeDesktopSnapshot) {
        if self.inner.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        {
            let mut guard = lock_unpoisoned(&self.inner.pending);
            guard.snapshot = Some(snapshot);
        }
        self.post_app_msg(WM_APP_SYNC_STATE);
    }

    /// Opens an inline Win32 `EDIT` control over `item_screen_rect` to rename `path`.
    pub fn begin_inline_rename(&self, path: String, current_name: String, item_screen_rect: Rect) {
        if self.inner.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        {
            let mut guard = lock_unpoisoned(&self.inner.pending);
            guard.rename = Some(PendingRename {
                path,
                current_name,
                item_screen_rect,
            });
        }
        self.post_app_msg(WM_APP_BEGIN_RENAME);
    }

    /// Updates the virtual canvas bounds without replacing icon items.
    pub fn set_bounds(&self, rect: Rect) {
        if self.inner.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        {
            let mut guard = lock_unpoisoned(&self.inner.pending);
            guard.bounds = Some(rect);
        }
        self.post_app_msg(WM_APP_SYNC_STATE);
    }

    /// Gracefully destroys the window and joins its Win32 message-loop thread.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    fn post_app_msg(&self, msg: u32) {
        let raw = self.inner.hwnd.load(Ordering::Acquire);
        if raw != 0 {
            let hwnd = HWND(raw as *mut c_void);
            let _ = unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) };
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct LayeredSurface {
    mem_dc: HDC,
    bitmap: HBITMAP,
    prev_obj: HGDIOBJ,
    width: i32,
    height: i32,
}

impl LayeredSurface {
    fn new(width: i32, height: i32) -> Result<Self, Win32Error> {
        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            return Err(Win32Error::Status {
                op: "CreateCompatibleDC",
                code: u32::MAX,
            });
        }
        let mut surface = Self {
            mem_dc,
            bitmap: HBITMAP::default(),
            prev_obj: HGDIOBJ::default(),
            width: 0,
            height: 0,
        };
        surface.resize(width, height)?;
        Ok(surface)
    }

    fn ensure_size(&mut self, width: i32, height: i32) -> Result<(), Win32Error> {
        let target_w = width.max(1);
        let target_h = height.max(1);
        if self.width == target_w && self.height == target_h && !self.bitmap.is_invalid() {
            return Ok(());
        }
        self.resize(target_w, target_h)
    }

    fn resize(&mut self, width: i32, height: i32) -> Result<(), Win32Error> {
        self.release_bitmap();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height selects top-down DIB orientation to match Direct2D coordinates.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let hbitmap = unsafe {
            CreateDIBSection(Some(self.mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        }
        .map_err(|e| win_err("CreateDIBSection", e))?;
        let prev_obj = unsafe { SelectObject(self.mem_dc, HGDIOBJ(hbitmap.0)) };
        self.bitmap = hbitmap;
        self.prev_obj = prev_obj;
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn release_bitmap(&mut self) {
        if !self.bitmap.is_invalid() {
            if !self.prev_obj.is_invalid() {
                unsafe {
                    let _ = SelectObject(self.mem_dc, self.prev_obj);
                }
                self.prev_obj = HGDIOBJ::default();
            }
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            self.bitmap = HBITMAP::default();
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        self.release_bitmap();
        if !self.mem_dc.is_invalid() {
            unsafe {
                let _ = DeleteDC(self.mem_dc);
            }
            self.mem_dc = HDC::default();
        }
    }
}

struct DesktopRenderer {
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    dc_rt: Option<ID2D1DCRenderTarget>,
    text_format: Option<IDWriteTextFormat>,
    cached_font_size: f32,
    icon_cache: IconCache,
    bitmap_cache: HashMap<String, ID2D1Bitmap>,
    surface: Option<LayeredSurface>,
}

impl DesktopRenderer {
    fn new() -> Result<Self, Win32Error> {
        let d2d_factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|e| win_err("D2D1CreateFactory", e))?;
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|e| win_err("DWriteCreateFactory", e))?;
        let icon_cache = IconCache::new()?;

        Ok(Self {
            d2d_factory,
            dwrite_factory,
            dc_rt: None,
            text_format: None,
            cached_font_size: 0.0,
            icon_cache,
            bitmap_cache: HashMap::new(),
            surface: None,
        })
    }

    fn ensure_render_target(&mut self) -> Result<ID2D1DCRenderTarget, Win32Error> {
        if let Some(rt) = &self.dc_rt {
            return Ok(rt.clone());
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            // Keep 1 DIP == 1 physical pixel so multi-monitor virtual_bounds & cell_size align 1:1.
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = unsafe { self.d2d_factory.CreateDCRenderTarget(&props) }
            .map_err(|e| win_err("CreateDCRenderTarget", e))?;
        self.dc_rt = Some(rt.clone());
        Ok(rt)
    }

    fn ensure_text_format(&mut self, font_size: f32) -> Result<IDWriteTextFormat, Win32Error> {
        if let Some(fmt) = &self.text_format
            && (self.cached_font_size - font_size).abs() <= 0.5
        {
            return Ok(fmt.clone());
        }

        let fmt = unsafe {
            self.dwrite_factory.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font_size,
                w!(""),
            )
        }
        .map_err(|e| win_err("CreateTextFormat", e))?;

        unsafe {
            let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            if let Ok(ellipsis) = self.dwrite_factory.CreateEllipsisTrimmingSign(&fmt) {
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let _ = fmt.SetTrimming(&trimming, Some(&ellipsis));
            }
        }

        self.cached_font_size = font_size;
        self.text_format = Some(fmt.clone());
        Ok(fmt)
    }

    fn sync_bitmaps_with_snapshot(&mut self, icons: &[FreeDesktopRenderItem]) {
        if self.bitmap_cache.is_empty() {
            return;
        }
        let active_paths: HashSet<&str> = icons.iter().map(|i| i.path.as_str()).collect();
        self.bitmap_cache
            .retain(|path, _| active_paths.contains(path.as_str()));
        if self.bitmap_cache.len() > MAX_CACHED_D2D_BITMAPS {
            self.bitmap_cache.clear();
        }
    }

    fn get_or_create_bitmap(
        &mut self,
        rt: &ID2D1DCRenderTarget,
        path: &str,
    ) -> Option<ID2D1Bitmap> {
        if let Some(bmp) = self.bitmap_cache.get(path) {
            return Some(bmp.clone());
        }
        let decoded = self.icon_cache.get_or_load(path)?;
        if decoded.width == 0 || decoded.height == 0 {
            return None;
        }
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
                decoded.width.saturating_mul(4),
                &props,
            )
        }
        .ok()?;
        self.bitmap_cache.insert(path.to_string(), bmp.clone());
        Some(bmp)
    }

    fn paint_and_commit(
        &mut self,
        hwnd: HWND,
        snapshot: &FreeDesktopSnapshot,
        hovered_path: Option<&str>,
        marquee: Option<(Point, Point)>,
        active_rename_path: Option<&str>,
    ) {
        let bounds = normalized_bounds(snapshot.virtual_bounds);
        let width = bounds.width().max(1);
        let height = bounds.height().max(1);

        let surface_res = match &mut self.surface {
            Some(surface) => surface.ensure_size(width, height),
            None => match LayeredSurface::new(width, height) {
                Ok(surface) => {
                    self.surface = Some(surface);
                    Ok(())
                }
                Err(err) => Err(err),
            },
        };
        if surface_res.is_err() {
            return;
        }

        let rt = match self.ensure_render_target() {
            Ok(rt) => rt,
            Err(_) => return,
        };

        let (cw, ch) = effective_cell_size(snapshot.cell_size);
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32;
        let font_size = compute_label_font_size(ch, dpi);
        let text_format = match self.ensure_text_format(font_size) {
            Ok(fmt) => fmt,
            Err(_) => return,
        };

        self.sync_bitmaps_with_snapshot(&snapshot.icons);

        let mem_dc = match &self.surface {
            Some(s) => s.mem_dc,
            None => return,
        };

        let bind_rc = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        if unsafe { rt.BindDC(mem_dc, &bind_rc) }.is_err() {
            self.dc_rt = None;
            self.bitmap_cache.clear();
            return;
        }

        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: HITTEST_ALPHA,
            }));
        }

        if !snapshot.hide_all_fences {
            let brushes = RenderBrushes::new(&rt);
            for item in &snapshot.icons {
                let is_hovered = hovered_path == Some(item.path.as_str());
                let hide_label = active_rename_path == Some(item.path.as_str());
                let bmp = self.get_or_create_bitmap(&rt, &item.path);
                draw_icon_cell(
                    &rt,
                    &text_format,
                    &brushes,
                    bounds,
                    (cw, ch),
                    item,
                    bmp.as_ref(),
                    is_hovered,
                    hide_label,
                );
            }

            if let Some((origin, current)) = marquee {
                draw_marquee_box(&rt, &brushes, bounds, origin, current);
            }
        }

        let end_res = unsafe { rt.EndDraw(None, None) };
        if end_res.is_err() {
            self.dc_rt = None;
            self.bitmap_cache.clear();
            return;
        }

        let pt_dst = POINT {
            x: bounds.left,
            y: bounds.top,
        };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let pt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&pt_dst),
                Some(&size),
                Some(mem_dc),
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        };
    }
}

struct RenderBrushes {
    sel_fill: Option<ID2D1SolidColorBrush>,
    sel_border: Option<ID2D1SolidColorBrush>,
    hover_fill: Option<ID2D1SolidColorBrush>,
    hover_border: Option<ID2D1SolidColorBrush>,
    fallback_fill: Option<ID2D1SolidColorBrush>,
    shadow: Option<ID2D1SolidColorBrush>,
    text: Option<ID2D1SolidColorBrush>,
    marquee_fill: Option<ID2D1SolidColorBrush>,
    marquee_border: Option<ID2D1SolidColorBrush>,
}

impl RenderBrushes {
    fn new(rt: &ID2D1DCRenderTarget) -> Self {
        Self {
            sel_fill: make_brush(rt, 0.24, 0.56, 0.94, 0.36),
            sel_border: make_brush(rt, 0.45, 0.75, 1.0, 0.80),
            hover_fill: make_brush(rt, 1.0, 1.0, 1.0, 0.14),
            hover_border: make_brush(rt, 1.0, 1.0, 1.0, 0.26),
            fallback_fill: make_brush(rt, 0.72, 0.78, 0.86, 0.75),
            shadow: make_brush(rt, 0.0, 0.0, 0.0, 0.85),
            text: make_brush(rt, 1.0, 1.0, 1.0, 0.98),
            marquee_fill: make_brush(rt, 0.20, 0.56, 0.98, 0.22),
            marquee_border: make_brush(rt, 0.35, 0.72, 1.0, 0.88),
        }
    }
}

fn make_brush(
    rt: &ID2D1DCRenderTarget,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> Option<ID2D1SolidColorBrush> {
    unsafe { rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None) }.ok()
}

#[allow(clippy::too_many_arguments)]
fn draw_icon_cell(
    rt: &ID2D1DCRenderTarget,
    text_format: &IDWriteTextFormat,
    brushes: &RenderBrushes,
    bounds: Rect,
    cell_size: (i32, i32),
    item: &FreeDesktopRenderItem,
    bitmap: Option<&ID2D1Bitmap>,
    is_hovered: bool,
    hide_label: bool,
) {
    let lx = (item.pos.x - bounds.left) as f32;
    let ly = (item.pos.y - bounds.top) as f32;
    let cw = cell_size.0 as f32;
    let ch = cell_size.1 as f32;

    let cell_rect = D2D_RECT_F {
        left: lx + 2.0,
        top: ly + 2.0,
        right: lx + cw - 2.0,
        bottom: ly + ch - 2.0,
    };

    if item.selected {
        let rr = D2D1_ROUNDED_RECT {
            rect: cell_rect,
            radiusX: 4.0,
            radiusY: 4.0,
        };
        if let Some(fill) = &brushes.sel_fill {
            unsafe { rt.FillRoundedRectangle(&rr, fill) };
        }
        if let Some(border) = &brushes.sel_border {
            unsafe { rt.DrawRoundedRectangle(&rr, border, 1.0, None) };
        }
    } else if is_hovered {
        let rr = D2D1_ROUNDED_RECT {
            rect: cell_rect,
            radiusX: 4.0,
            radiusY: 4.0,
        };
        if let Some(fill) = &brushes.hover_fill {
            unsafe { rt.FillRoundedRectangle(&rr, fill) };
        }
        if let Some(border) = &brushes.hover_border {
            unsafe { rt.DrawRoundedRectangle(&rr, border, 1.0, None) };
        }
    }

    let icon_dim = (cw * 0.50).clamp(28.0, 56.0).min((ch - 32.0).max(24.0));
    let icon_left = lx + (cw - icon_dim) * 0.5;
    let icon_top = ly + 6.0;
    let icon_rect = D2D_RECT_F {
        left: icon_left,
        top: icon_top,
        right: icon_left + icon_dim,
        bottom: icon_top + icon_dim,
    };
    let opacity = if item.is_cut { 0.45 } else { 1.0 };

    if let Some(bmp) = bitmap {
        unsafe {
            rt.DrawBitmap(
                bmp,
                Some(&icon_rect),
                opacity,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    } else if let Some(fallback) = &brushes.fallback_fill {
        let rr = D2D1_ROUNDED_RECT {
            rect: icon_rect,
            radiusX: 6.0,
            radiusY: 6.0,
        };
        unsafe { rt.FillRoundedRectangle(&rr, fallback) };
    }

    if hide_label || item.display_name.is_empty() {
        return;
    }

    let label_top = (icon_top + icon_dim + 4.0).min(ly + ch - 18.0);
    let label_rect = D2D_RECT_F {
        left: lx + 3.0,
        top: label_top,
        right: (lx + cw - 3.0).max(lx + 8.0),
        bottom: (ly + ch - 3.0).max(label_top + 14.0),
    };
    let label_utf16: Vec<u16> = item.display_name.encode_utf16().collect();

    if let Some(shadow) = &brushes.shadow {
        let shadow_rect = D2D_RECT_F {
            left: label_rect.left + 1.0,
            top: label_rect.top + 1.0,
            right: label_rect.right + 1.0,
            bottom: label_rect.bottom + 1.0,
        };
        unsafe {
            rt.DrawText(
                &label_utf16,
                text_format,
                &shadow_rect,
                shadow,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    if let Some(text_brush) = &brushes.text {
        unsafe {
            rt.DrawText(
                &label_utf16,
                text_format,
                &label_rect,
                text_brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}

fn draw_marquee_box(
    rt: &ID2D1DCRenderTarget,
    brushes: &RenderBrushes,
    bounds: Rect,
    origin: Point,
    current: Point,
) {
    let screen_rc = rect_from_points(origin, current);
    let rc = D2D_RECT_F {
        left: (screen_rc.left - bounds.left) as f32,
        top: (screen_rc.top - bounds.top) as f32,
        right: (screen_rc.right - bounds.left) as f32,
        bottom: (screen_rc.bottom - bounds.top) as f32,
    };
    if let Some(fill) = &brushes.marquee_fill {
        unsafe { rt.FillRectangle(&rc, fill) };
    }
    if let Some(border) = &brushes.marquee_border {
        unsafe { rt.DrawRectangle(&rc, border, 1.0, None) };
    }
}

enum MouseDragState {
    None,
    PendingIcon {
        path: String,
        start_screen: Point,
        ctrl: bool,
        shift: bool,
    },
    PendingBlank {
        start_screen: Point,
        additive: bool,
    },
    Marquee {
        origin: Point,
        current: Point,
        additive: bool,
    },
}

struct InlineRenameState {
    edit_hwnd: HWND,
    font: HFONT,
    old_path: String,
}

struct WindowState {
    hwnd: HWND,
    hwnd_slot: Arc<AtomicIsize>,
    notify_hwnd: isize,
    notify_msg: u32,
    event_tx: std::sync::mpsc::Sender<FreeDesktopEvent>,
    pending: Arc<Mutex<PendingCommands>>,
    snapshot: FreeDesktopSnapshot,
    renderer: DesktopRenderer,
    drag_state: MouseDragState,
    hovered_path: Option<String>,
    tracking_mouse_leave: bool,
    suppress_next_lbutton_up: bool,
    inline_rename: Option<InlineRenameState>,
    window_shown: bool,
}

impl WindowState {
    fn emit(&self, event: FreeDesktopEvent) {
        let _ = self.event_tx.send(event);
        if self.notify_hwnd != 0 && self.notify_msg != 0 {
            let target = HWND(self.notify_hwnd as *mut c_void);
            let _ = unsafe { PostMessageW(Some(target), self.notify_msg, WPARAM(0), LPARAM(0)) };
        }
    }

    fn repaint(&mut self) {
        let marquee = match &self.drag_state {
            MouseDragState::Marquee {
                origin, current, ..
            } => Some((*origin, *current)),
            _ => None,
        };
        let rename_path = self.inline_rename.as_ref().map(|r| r.old_path.clone());
        let hovered = self.hovered_path.clone();
        self.renderer.paint_and_commit(
            self.hwnd,
            &self.snapshot,
            hovered.as_deref(),
            marquee,
            rename_path.as_deref(),
        );
    }

    fn sync_pending(&mut self) {
        let (new_snapshot, new_bounds) = {
            let mut guard = lock_unpoisoned(&self.pending);
            (guard.snapshot.take(), guard.bounds.take())
        };

        let prev_bounds = self.snapshot.virtual_bounds;
        if let Some(snap) = new_snapshot {
            self.snapshot = snap;
        }
        if let Some(bounds) = new_bounds {
            self.snapshot.virtual_bounds = bounds;
        }

        let bounds = normalized_bounds(self.snapshot.virtual_bounds);
        if bounds != prev_bounds || !self.window_shown {
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    None,
                    bounds.left,
                    bounds.top,
                    bounds.width().max(1),
                    bounds.height().max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }

        self.repaint();

        if !self.window_shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.window_shown = true;
        }
    }

    fn start_inline_rename_from_pending(&mut self) {
        let pending_rename = {
            let mut guard = lock_unpoisoned(&self.pending);
            guard.rename.take()
        };
        let Some(req) = pending_rename else {
            return;
        };

        // Close any previous edit cleanly without confusing the newly requested rename session.
        self.destroy_inline_rename_silently();

        let client_rect =
            compute_rename_edit_client_rect(req.item_screen_rect, self.snapshot.virtual_bounds);
        let hinstance = unsafe { GetModuleHandleW(None) }.ok();
        let text_w = HSTRING::from(req.current_name.as_str());

        let Ok(edit_hwnd) = (unsafe {
            CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                &text_w,
                WS_CHILD
                    | WS_VISIBLE
                    | WS_BORDER
                    | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                        (ES_AUTOHSCROLL | ES_CENTER) as u32,
                    ),
                client_rect.left,
                client_rect.top,
                client_rect.width().max(64),
                client_rect.height().max(22),
                Some(self.hwnd),
                Some(HMENU(RENAME_EDIT_CTRL_ID as *mut c_void)),
                hinstance.map(Into::into),
                None,
            )
        }) else {
            self.emit(FreeDesktopEvent::RenameCancelled);
            return;
        };

        let (_, ch) = effective_cell_size(self.snapshot.cell_size);
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32;
        let font_px = -(compute_label_font_size(ch, dpi).round() as i32);
        let font = unsafe {
            CreateFontW(
                font_px,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (FF_DONTCARE.0 | DEFAULT_PITCH.0) as u32,
                w!("Segoe UI"),
            )
        };
        if !font.is_invalid() {
            unsafe {
                let _ = SendMessageW(
                    edit_hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }

        let state_ptr = self as *mut WindowState as usize;
        let subclassed = unsafe {
            SetWindowSubclass(
                edit_hwnd,
                Some(rename_edit_subclass_proc),
                RENAME_EDIT_SUBCLASS_ID,
                state_ptr,
            )
        };
        if !subclassed.as_bool() {
            unsafe {
                let _ = DestroyWindow(edit_hwnd);
                if !font.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
            self.emit(FreeDesktopEvent::RenameCancelled);
            return;
        }

        self.inline_rename = Some(InlineRenameState {
            edit_hwnd,
            font,
            old_path: req.path,
        });
        // Repaint so underlying icon label hides immediately beneath the EDIT control.
        self.repaint();

        let stem_len = rename_stem_utf16_len(&req.current_name);
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(edit_hwnd));
            let _ = SendMessageW(
                edit_hwnd,
                EM_SETSEL,
                Some(WPARAM(0)),
                Some(LPARAM(stem_len as isize)),
            );
        }
    }

    fn finish_inline_rename(&mut self, commit: bool) {
        let Some(rename) = self.inline_rename.take() else {
            return;
        };

        let event = if commit {
            let text = read_edit_text(rename.edit_hwnd);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                FreeDesktopEvent::RenameCancelled
            } else {
                FreeDesktopEvent::RenameCommitted {
                    old_path: rename.old_path.clone(),
                    new_name: trimmed.to_string(),
                }
            }
        } else {
            FreeDesktopEvent::RenameCancelled
        };

        cleanup_rename_controls(&rename);
        self.repaint();
        self.emit(event);
    }

    fn destroy_inline_rename_silently(&mut self) {
        if let Some(rename) = self.inline_rename.take() {
            cleanup_rename_controls(&rename);
            self.repaint();
        }
    }
}

fn cleanup_rename_controls(rename: &InlineRenameState) {
    unsafe {
        let _ = RemoveWindowSubclass(
            rename.edit_hwnd,
            Some(rename_edit_subclass_proc),
            RENAME_EDIT_SUBCLASS_ID,
        );
        let _ = DestroyWindow(rename.edit_hwnd);
        if !rename.font.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(rename.font.0));
        }
    }
}

fn read_edit_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied as usize])
}

fn run_window_thread(
    notify_hwnd: isize,
    notify_msg: u32,
    event_tx: std::sync::mpsc::Sender<FreeDesktopEvent>,
    pending: Arc<Mutex<PendingCommands>>,
    hwnd_slot: Arc<AtomicIsize>,
    ready_tx: std::sync::mpsc::SyncSender<Result<isize, Win32Error>>,
) {
    let com_hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let com_ok = com_hr.is_ok();

    let renderer = match DesktopRenderer::new() {
        Ok(r) => r,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            if com_ok {
                unsafe { CoUninitialize() };
            }
            return;
        }
    };

    let hinstance = unsafe { GetModuleHandleW(None) }.ok();
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(free_desktop_wnd_proc),
        hInstance: hinstance.map(Into::into).unwrap_or_default(),
        lpszClassName: FREE_DESKTOP_CLASS_NAME,
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassExW(&wc);
    }

    let boxed_state = Box::new(WindowState {
        hwnd: HWND::default(),
        hwnd_slot: Arc::clone(&hwnd_slot),
        notify_hwnd,
        notify_msg,
        event_tx,
        pending,
        snapshot: FreeDesktopSnapshot::default(),
        renderer,
        drag_state: MouseDragState::None,
        hovered_path: None,
        tracking_mouse_leave: false,
        suppress_next_lbutton_up: false,
        inline_rename: None,
        window_shown: false,
    });
    let state_ptr = Box::into_raw(boxed_state);

    let hwnd_res = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            FREE_DESKTOP_CLASS_NAME,
            FREE_DESKTOP_WINDOW_TITLE,
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinstance.map(Into::into),
            Some(state_ptr.cast()),
        )
    };

    let hwnd = match hwnd_res {
        Ok(h) => h,
        Err(err) => {
            // Reclaim leaked Box if CreateWindowExW failed before WM_NCDESTROY.
            unsafe {
                drop(Box::from_raw(state_ptr));
            }
            let _ = ready_tx.send(Err(win_err("CreateWindowExW(DeskLite_FreeDesktop)", err)));
            if com_ok {
                unsafe { CoUninitialize() };
            }
            return;
        }
    };

    unsafe {
        DragAcceptFiles(hwnd, true);
    }
    attach_free_desktop_canvas(hwnd.0 as isize);

    // Commit an initial 1x1 alpha=1 frame before ShowWindow so no uninitialized surface ever flashes.
    unsafe {
        let state = &mut *state_ptr;
        state.repaint();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        state.window_shown = true;
    }

    let _ = ready_tx.send(Ok(hwnd.0 as isize));
    hwnd_slot.store(hwnd.0 as isize, Ordering::Release);

    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if com_ok {
        unsafe { CoUninitialize() };
    }
}

unsafe extern "system" fn rename_edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid_subclass: usize,
    dw_ref_data: usize,
) -> LRESULT {
    let state_ptr = dw_ref_data as *mut WindowState;
    if state_ptr.is_null() {
        return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    }

    match msg {
        WM_GETDLGCODE => LRESULT(DLGC_WANTALLKEYS as isize),
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_RETURN.0 {
                let state = unsafe { &mut *state_ptr };
                state.finish_inline_rename(true);
                return LRESULT(0);
            }
            if vk == VK_ESCAPE.0 {
                let state = unsafe { &mut *state_ptr };
                state.finish_inline_rename(false);
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_CHAR => {
            // Suppress system beep on Enter/Esc inside single-line EDIT control.
            if matches!(wparam.0, 0x0A | 0x0D | 0x1B) {
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_KILLFOCUS => {
            let state = unsafe { &mut *state_ptr };
            if state
                .inline_rename
                .as_ref()
                .is_some_and(|r| r.edit_hwnd == hwnd)
            {
                // Defer teardown via PostMessageW so DefSubclassProc finishes focus transition cleanly.
                let _ = unsafe {
                    PostMessageW(Some(state.hwnd), WM_APP_CLOSE_RENAME, WPARAM(0), LPARAM(0))
                };
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_NCDESTROY => {
            let _ = unsafe {
                RemoveWindowSubclass(
                    hwnd,
                    Some(rename_edit_subclass_proc),
                    RENAME_EDIT_SUBCLASS_ID,
                )
            };
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn free_desktop_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let state_ptr = cs.lpCreateParams as *mut WindowState;
        if !state_ptr.is_null() {
            unsafe {
                (*state_ptr).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            }
        }
        return LRESULT(0);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_APP_SYNC_STATE => {
            state.sync_pending();
            LRESULT(0)
        }
        WM_APP_BEGIN_RENAME => {
            state.start_inline_rename_from_pending();
            LRESULT(0)
        }
        WM_APP_CLOSE_RENAME => {
            state.finish_inline_rename(false);
            LRESULT(0)
        }
        WM_APP_SHUTDOWN => {
            state.destroy_inline_rename_silently();
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            state.repaint();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            handle_mouse_move(state, lparam);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            state.tracking_mouse_leave = false;
            if state.hovered_path.take().is_some() {
                state.repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            handle_lbutton_down(state, wparam, lparam);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            handle_lbutton_up(state, lparam);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            handle_lbutton_dblclk(state, lparam);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            handle_rbutton_up(state, lparam);
            LRESULT(0)
        }
        WM_CONTEXTMENU => LRESULT(0),
        WM_CAPTURECHANGED => {
            let was_marquee = matches!(state.drag_state, MouseDragState::Marquee { .. });
            state.drag_state = MouseDragState::None;
            if was_marquee {
                state.repaint();
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            handle_drop_files(state, wparam);
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let activation_state = (wparam.0 & 0xFFFF) as u16;
            if activation_state == 0 {
                let activating = HWND(lparam.0 as *mut c_void);
                let is_child_edit = state
                    .inline_rename
                    .as_ref()
                    .is_some_and(|r| r.edit_hwnd == activating);
                if !is_child_edit {
                    state.finish_inline_rename(false);
                    state.emit(FreeDesktopEvent::FocusLost);
                }
            }
            LRESULT(0)
        }
        WM_CHAR => {
            // Suppress system beep on Enter/Esc inside single-line EDIT control.
            if matches!(wparam.0, 0x0A | 0x0D | 0x1B) {
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_KILLFOCUS => {
            let gaining = HWND(wparam.0 as *mut c_void);
            let is_child_edit = state
                .inline_rename
                .as_ref()
                .is_some_and(|r| r.edit_hwnd == gaining);
            if !is_child_edit {
                state.emit(FreeDesktopEvent::FocusLost);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            state.destroy_inline_rename_silently();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            state.hwnd_slot.store(0, Ordering::Release);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn handle_lbutton_down(state: &mut WindowState, wparam: WPARAM, lparam: LPARAM) {
    state.finish_inline_rename(false);
    state.suppress_next_lbutton_up = false;

    let screen_pt = screen_point_from_lparam(lparam, state.snapshot.virtual_bounds);
    let ctrl = (wparam.0 & MK_CONTROL.0 as usize) != 0;
    let shift = (wparam.0 & MK_SHIFT.0 as usize) != 0;

    unsafe {
        let _ = SetCapture(state.hwnd);
    }

    if let Some(idx) = hit_test_icon_at(&state.snapshot, screen_pt) {
        let path = state.snapshot.icons[idx].path.clone();
        state.drag_state = MouseDragState::PendingIcon {
            path,
            start_screen: screen_pt,
            ctrl,
            shift,
        };
    } else {
        state.drag_state = MouseDragState::PendingBlank {
            start_screen: screen_pt,
            additive: ctrl || shift,
        };
    }
}

fn handle_mouse_move(state: &mut WindowState, lparam: LPARAM) {
    if !state.tracking_mouse_leave {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: state.hwnd,
            dwHoverTime: 0,
        };
        if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
            state.tracking_mouse_leave = true;
        }
    }

    let screen_pt = screen_point_from_lparam(lparam, state.snapshot.virtual_bounds);
    match &mut state.drag_state {
        MouseDragState::PendingIcon { start_screen, .. } => {
            if exceeded_drag_threshold(*start_screen, screen_pt) {
                let start = *start_screen;
                state.drag_state = MouseDragState::None;
                unsafe {
                    if GetCapture() == state.hwnd {
                        let _ = ReleaseCapture();
                    }
                }
                state.emit(FreeDesktopEvent::StartDragIcons {
                    start_screen: (start.x, start.y),
                });
            }
        }
        MouseDragState::PendingBlank {
            start_screen,
            additive,
        } => {
            if exceeded_drag_threshold(*start_screen, screen_pt) {
                state.drag_state = MouseDragState::Marquee {
                    origin: *start_screen,
                    current: screen_pt,
                    additive: *additive,
                };
                state.repaint();
            }
        }
        MouseDragState::Marquee { current, .. } => {
            if *current != screen_pt {
                *current = screen_pt;
                state.repaint();
            }
        }
        MouseDragState::None => {
            let new_hover = hit_test_icon_at(&state.snapshot, screen_pt)
                .map(|i| state.snapshot.icons[i].path.clone());
            if new_hover != state.hovered_path {
                state.hovered_path = new_hover;
                state.repaint();
            }
        }
    }
}

fn handle_lbutton_up(state: &mut WindowState, lparam: LPARAM) {
    unsafe {
        if GetCapture() == state.hwnd {
            let _ = ReleaseCapture();
        }
    }
    let taken = std::mem::replace(&mut state.drag_state, MouseDragState::None);
    if state.suppress_next_lbutton_up {
        state.suppress_next_lbutton_up = false;
        if matches!(taken, MouseDragState::Marquee { .. }) {
            state.repaint();
        }
        return;
    }

    let _screen_pt = screen_point_from_lparam(lparam, state.snapshot.virtual_bounds);
    match taken {
        MouseDragState::PendingIcon {
            path, ctrl, shift, ..
        } => {
            state.emit(FreeDesktopEvent::IconClicked { path, ctrl, shift });
        }
        MouseDragState::PendingBlank { .. } => {
            state.emit(FreeDesktopEvent::BlankClicked);
        }
        MouseDragState::Marquee {
            origin,
            current,
            additive,
        } => {
            state.repaint();
            let rect = rect_from_points(origin, current);
            state.emit(FreeDesktopEvent::MarqueeSelect { rect, additive });
        }
        MouseDragState::None => {}
    }
}

fn handle_lbutton_dblclk(state: &mut WindowState, lparam: LPARAM) {
    unsafe {
        if GetCapture() == state.hwnd {
            let _ = ReleaseCapture();
        }
    }
    state.drag_state = MouseDragState::None;
    state.suppress_next_lbutton_up = true;

    let screen_pt = screen_point_from_lparam(lparam, state.snapshot.virtual_bounds);
    if let Some(idx) = hit_test_icon_at(&state.snapshot, screen_pt) {
        let path = state.snapshot.icons[idx].path.clone();
        state.emit(FreeDesktopEvent::IconDoubleClicked { path });
    } else {
        state.emit(FreeDesktopEvent::BlankDoubleClicked);
    }
}

fn handle_rbutton_up(state: &mut WindowState, lparam: LPARAM) {
    let mut pt = POINT {
        x: signed_loword(lparam.0),
        y: signed_hiword(lparam.0),
    };
    if !unsafe { ClientToScreen(state.hwnd, &mut pt) }.as_bool() {
        let screen_pt = screen_point_from_lparam(lparam, state.snapshot.virtual_bounds);
        pt.x = screen_pt.x;
        pt.y = screen_pt.y;
    }
    let screen_pt = Point { x: pt.x, y: pt.y };
    if let Some(idx) = hit_test_icon_at(&state.snapshot, screen_pt) {
        let path = state.snapshot.icons[idx].path.clone();
        state.emit(FreeDesktopEvent::ContextMenuIcon {
            path,
            screen_x: screen_pt.x,
            screen_y: screen_pt.y,
        });
    } else {
        state.emit(FreeDesktopEvent::ContextMenuBlank {
            screen_x: screen_pt.x,
            screen_y: screen_pt.y,
        });
    }
}

fn handle_drop_files(state: &mut WindowState, wparam: WPARAM) {
    let hdrop = HDROP(wparam.0 as *mut c_void);
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for idx in 0..count {
        let needed = unsafe { DragQueryFileW(hdrop, idx, None) };
        if needed == 0 {
            continue;
        }
        let mut buf = vec![0u16; needed as usize + 1];
        let copied = unsafe { DragQueryFileW(hdrop, idx, Some(&mut buf)) };
        if copied > 0 {
            let slice_ptr = slice_from_raw_parts(buf.as_ptr(), copied as usize);
            let path = String::from_utf16_lossy(unsafe { &*slice_ptr });
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }

    let mut pt = POINT::default();
    let _ = unsafe { DragQueryPoint(hdrop, &mut pt) };
    let _ = unsafe { ClientToScreen(state.hwnd, &mut pt) };
    unsafe { DragFinish(hdrop) };

    if !paths.is_empty() {
        state.emit(FreeDesktopEvent::FilesDroppedFromExplorer {
            paths,
            screen_x: pt.x,
            screen_y: pt.y,
        });
    }
}

fn signed_loword(value: isize) -> i32 {
    ((value as usize) & 0xFFFF) as u16 as i16 as i32
}

fn signed_hiword(value: isize) -> i32 {
    (((value as usize) >> 16) & 0xFFFF) as u16 as i16 as i32
}

fn screen_point_from_lparam(lparam: LPARAM, virtual_bounds: Rect) -> Point {
    let bounds = normalized_bounds(virtual_bounds);
    Point {
        x: bounds.left + signed_loword(lparam.0),
        y: bounds.top + signed_hiword(lparam.0),
    }
}

fn normalized_bounds(bounds: Rect) -> Rect {
    Rect::new(
        bounds.left,
        bounds.top,
        bounds.right.max(bounds.left + 1),
        bounds.bottom.max(bounds.top + 1),
    )
}

fn effective_cell_size(cell_size: (i32, i32)) -> (i32, i32) {
    (cell_size.0.max(48), cell_size.1.max(56))
}

fn compute_label_font_size(cell_h: i32, dpi: f32) -> f32 {
    let dpi_base = 12.0 * (dpi / 96.0);
    let cell_cap = (cell_h as f32 * 0.16).clamp(11.0, 22.0);
    dpi_base.min(cell_cap).max(11.0)
}

fn rect_from_points(a: Point, b: Point) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

fn exceeded_drag_threshold(start: Point, current: Point) -> bool {
    (current.x - start.x).abs() > DRAG_THRESHOLD_PX
        || (current.y - start.y).abs() > DRAG_THRESHOLD_PX
}

fn hit_test_icon_at(snapshot: &FreeDesktopSnapshot, screen_pt: Point) -> Option<usize> {
    if snapshot.hide_all_fences {
        return None;
    }
    let (cw, ch) = effective_cell_size(snapshot.cell_size);
    snapshot
        .icons
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, item)| {
            let right = item.pos.x.saturating_add(cw);
            let bottom = item.pos.y.saturating_add(ch);
            if screen_pt.x >= item.pos.x
                && screen_pt.x < right
                && screen_pt.y >= item.pos.y
                && screen_pt.y < bottom
            {
                Some(idx)
            } else {
                None
            }
        })
}

fn compute_rename_edit_client_rect(item_screen_rect: Rect, virtual_bounds: Rect) -> Rect {
    let bounds = normalized_bounds(virtual_bounds);
    let item_w = item_screen_rect.width().max(72);
    let item_h = item_screen_rect.height().max(48);
    let edit_h = (item_h / 3).clamp(22, 34);
    let top_offset = (item_h - edit_h - 2).max(0);
    let left = item_screen_rect.left - bounds.left + 2;
    let top = item_screen_rect.top - bounds.top + top_offset;
    let width = (item_w - 4).max(64);
    Rect::new(left, top, left + width, top + edit_h)
}

fn rename_stem_utf16_len(name: &str) -> usize {
    let stem = match name.rfind('.') {
        Some(dot_idx) if dot_idx > 0 => &name[..dot_idx],
        _ => name,
    };
    stem.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_points_normalizes_reverse_drag() {
        let rc = rect_from_points(Point { x: 300, y: 420 }, Point { x: 120, y: 90 });
        assert_eq!(rc, Rect::new(120, 90, 300, 420));
    }

    #[test]
    fn drag_threshold_requires_strictly_more_than_4px() {
        let start = Point { x: 100, y: 100 };
        assert!(!exceeded_drag_threshold(start, Point { x: 104, y: 104 }));
        assert!(exceeded_drag_threshold(start, Point { x: 105, y: 100 }));
        assert!(exceeded_drag_threshold(start, Point { x: 100, y: 95 }));
    }

    #[test]
    fn hit_test_respects_hide_all_fences_and_topmost_icon() {
        let mut snap = FreeDesktopSnapshot {
            virtual_bounds: Rect::new(-1920, 0, 1920, 1080),
            cell_size: (80, 96),
            hide_all_fences: false,
            icons: vec![
                FreeDesktopRenderItem {
                    path: "C:\\a.txt".to_string(),
                    display_name: "a.txt".to_string(),
                    pos: Point { x: -100, y: 50 },
                    selected: false,
                    is_cut: false,
                },
                FreeDesktopRenderItem {
                    path: "C:\\b.txt".to_string(),
                    display_name: "b.txt".to_string(),
                    pos: Point { x: -100, y: 50 },
                    selected: true,
                    is_cut: false,
                },
            ],
        };
        assert_eq!(hit_test_icon_at(&snap, Point { x: -60, y: 80 }), Some(1));
        assert_eq!(hit_test_icon_at(&snap, Point { x: 10, y: 10 }), None);

        snap.hide_all_fences = true;
        assert_eq!(hit_test_icon_at(&snap, Point { x: -60, y: 80 }), None);
    }

    #[test]
    fn rename_stem_and_edit_rect_match_multi_monitor_coords() {
        assert_eq!(rename_stem_utf16_len("notes.txt"), 5);
        assert_eq!(rename_stem_utf16_len(".gitignore"), 10);
        assert_eq!(rename_stem_utf16_len("文档.docx"), 2);

        let edit = compute_rename_edit_client_rect(
            Rect::new(-1820, 100, -1740, 196),
            Rect::new(-1920, 0, 1920, 1080),
        );
        assert_eq!(edit.left, 102);
        assert!(edit.top > 100 && edit.bottom <= 196);
        assert_eq!(edit.width(), 76);
    }
}
