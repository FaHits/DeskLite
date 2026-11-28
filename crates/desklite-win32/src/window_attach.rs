use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Mutex, MutexGuard, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_COLOR_NONE,
    DWMWA_EXCLUDED_FROM_PEEK, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DWMWCP_ROUND, DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, RDW_INTERNALPAINT, RDW_INVALIDATE, RDW_NOERASE, RedrawWindow, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, VK_CONTROL, VK_LBUTTON,
    VK_MENU, VK_PROCESSKEY, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CBT_CREATEWNDW, CWPSTRUCT, CallNextHookEx, CallWindowProcW,
    EVENT_OBJECT_REORDER, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, FindWindowW, GW_HWNDNEXT, GW_HWNDPREV, GWL_EXSTYLE, GWL_STYLE,
    GWLP_HWNDPARENT, GWLP_WNDPROC, GetClassNameW, GetCursorPos, GetDesktopWindow,
    GetForegroundWindow, GetTopWindow, GetWindow, GetWindowLongPtrW, GetWindowRect,
    HCBT_CREATEWND, HHOOK, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT,
    HTTOP, HTTOPLEFT, HTTOPRIGHT, HWND_BOTTOM, HWND_TOP, IDC_ARROW, IsIconic, IsWindow,
    IsWindowVisible, LoadCursorW, PostMessageW, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, STYLESTRUCT,
    SW_HIDE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSENDCHANGING, SWP_NOSIZE,
    SWP_NOZORDER, SetCursor, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, WH_CALLWNDPROC, WH_CBT, WINDOWPOS, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_CREATE, WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_EXITSIZEMOVE,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOVING, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCDESTROY,
    WM_NCHITTEST, WM_NCPAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SHOWWINDOW, WM_SIZING,
    WM_STYLECHANGING, WM_SYSCOMMAND, WM_WINDOWPOSCHANGING, WNDPROC, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_DLGFRAME, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_WINDOWEDGE, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZE, WS_MINIMIZEBOX,
    WS_POPUP, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING, PCSTR, w};

use crate::desktop_listview::probe_desktop_window_tree;

const WM_APP_ENSURE_DESKTOP_Z: u32 = WM_APP + 99;
const WM_APP_UNCLOAK: u32 = WM_APP + 101;
const DISALLOWED_STYLES: u32 = WS_CAPTION.0
    | WS_BORDER.0
    | WS_DLGFRAME.0
    | WS_SYSMENU.0
    | WS_MINIMIZEBOX.0
    | WS_MAXIMIZEBOX.0
    | WS_MAXIMIZE.0
    | WS_MINIMIZE.0;
const DISALLOWED_EX_STYLES: u32 =
    WS_EX_APPWINDOW.0 | WS_EX_WINDOWEDGE.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_CLIENTEDGE.0;

static IN_SIZE_MOVE_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static FREE_DESKTOP_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static FILE_DRAG_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DEFERRED_Z_AFTER_SIZEMOVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static REASSERTING_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static PREFERRED_TOP_FENCE_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static SELF_Z_REORDER_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static Z_REASSERT_NOTIFY_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static Z_REASSERT_NOTIFY_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static Z_REASSERT_QUEUED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static Z_REASSERT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static EXTERNAL_FG_NOTIFY_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static EXTERNAL_FG_NOTIFY_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_external_foreground_notify_target(hwnd: isize, msg: u32) {
    EXTERNAL_FG_NOTIFY_MSG.store(msg, std::sync::atomic::Ordering::Release);
    EXTERNAL_FG_NOTIFY_HWND.store(hwnd, std::sync::atomic::Ordering::Release);
}

/// Registers the helper window target for coalesced desktop Z-order reassertion messages.
pub fn set_z_reassert_notify_target(hwnd: isize, msg: u32) {
    Z_REASSERT_QUEUED.store(false, std::sync::atomic::Ordering::Release);
    Z_REASSERT_NOTIFY_MSG.store(msg, std::sync::atomic::Ordering::Release);
    Z_REASSERT_NOTIFY_HWND.store(hwnd, std::sync::atomic::Ordering::Release);
}

pub fn set_file_drag_active(active: bool) {
    FILE_DRAG_ACTIVE.store(active, std::sync::atomic::Ordering::Release);
}

pub fn is_file_drag_active() -> bool {
    FILE_DRAG_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
}

fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn free_desktop_hwnd() -> isize {
    FREE_DESKTOP_HWND.load(std::sync::atomic::Ordering::Acquire)
}

pub fn is_free_desktop_hwnd(raw_hwnd: isize) -> bool {
    raw_hwnd != 0 && free_desktop_hwnd() == raw_hwnd
}

pub fn is_in_size_move() -> bool {
    let raw = IN_SIZE_MOVE_HWND.load(std::sync::atomic::Ordering::Acquire);
    if raw == 0 {
        return false;
    }
    let alive = unsafe { IsWindow(Some(HWND(raw as *mut _))).as_bool() };
    if !alive || !is_primary_mouse_down() {
        if IN_SIZE_MOVE_HWND
            .compare_exchange(
                raw,
                0,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            let mut active = lock_unpoisoned(active_sizemove_rect());
            if active.is_some_and(|(h, _)| h == raw) {
                *active = None;
            }
        }
        return false;
    }
    true
}

pub fn cursor_screen_pos() -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        Some((pt.x, pt.y))
    } else {
        None
    }
}

pub fn window_outer_rect(raw_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if raw_hwnd == 0 {
        return None;
    }
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(HWND(raw_hwnd as *mut _), &mut rc) }.is_ok() {
        Some((rc.left, rc.top, rc.right, rc.bottom))
    } else {
        None
    }
}

pub fn is_primary_mouse_down() -> bool {
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}

pub fn is_ctrl_key_down() -> bool {
    (unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0
}

pub fn focus_window_now(raw_hwnd: isize) {
    if raw_hwnd != 0 {
        promote_fence_above_siblings(raw_hwnd);
        let hwnd = HWND(raw_hwnd as *mut _);
        unsafe {
            if GetForegroundWindow() != hwnd {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

pub fn allow_foreground_self() {
    unsafe {
        let _ = AllowSetForegroundWindow(GetCurrentProcessId());
    }
}

/// Grants foreground activation permission cleanly within the current process without
/// cross-thread `AttachThreadInput`, avoiding message-queue stalls and focus oscillation.
pub fn force_foreground_for_popup(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    allow_foreground_self();
    let hwnd = HWND(raw_hwnd as *mut _);
    unsafe {
        if GetForegroundWindow() != hwnd {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Enables Windows 10/11 dark-mode styling for native Win32 `TrackPopupMenuEx` context menus.
pub fn enable_dark_popup_menus() {
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else {
            return;
        };
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135usize as *const u8)) {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            let _ = f(2); // PreferredAppMode::AllowDark
        }
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136usize as *const u8)) {
            let f: unsafe extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

pub fn move_window_no_z(raw_hwnd: isize, x: i32, y: i32) {
    if raw_hwnd == 0 {
        return;
    }
    let _ = unsafe {
        SetWindowPos(
            HWND(raw_hwnd as *mut _),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

pub fn set_window_rect_no_z(raw_hwnd: isize, x: i32, y: i32, w: i32, h: i32) {
    if raw_hwnd == 0 {
        return;
    }
    let _ = unsafe {
        SetWindowPos(
            HWND(raw_hwnd as *mut _),
            None,
            x,
            y,
            w.max(1),
            h.max(1),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// Restricts a fence HWND to `[0, width) x [0, height)` during drawer animations and
/// disables DWM corner shadow so unrendered lower regions stay 100% invisible.
pub fn set_window_clip_rect(raw_hwnd: isize, width: i32, height: i32) {
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    let corner = DWMWCP_DONOTROUND;
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corner).cast(),
            size_of::<i32>() as u32,
        )
    };
    let hrgn = unsafe { CreateRectRgn(0, 0, width.max(1), height.max(1)) };
    if !hrgn.is_invalid() {
        let ok = unsafe { SetWindowRgn(hwnd, Some(hrgn), true) };
        if ok == 0 {
            unsafe {
                let _ = DeleteObject(hrgn.into());
            }
        }
    }
}

/// Removes any temporary HRGN animation clip and re-enables Win11 native rounded corners.
pub fn clear_window_clip_rect(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    let _ = unsafe { SetWindowRgn(hwnd, None, true) };
    let corner = DWMWCP_ROUND;
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corner).cast(),
            size_of::<i32>() as u32,
        )
    };
}

/// Posts an application message without blocking (used to request an async FreeDesktop refresh).
pub fn post_window_message(raw_hwnd: isize, msg: u32) {
    if raw_hwnd != 0 && msg != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw_hwnd as *mut _)),
                msg,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Invalidates a live child window without erasing its background so Win32/eframe dispatches
/// a reliable `WM_PAINT` / `RedrawRequested` frame even when cross-viewport repaint events lag.
pub fn invalidate_window_no_erase(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    if unsafe { IsWindow(Some(hwnd)).as_bool() } {
        unsafe {
            let _ = RedrawWindow(
                Some(hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_NOERASE | RDW_INTERNALPAINT,
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Win32Hotkey {
    SelectAll,
    Copy,
    CopyPaths,
    Cut,
    Paste,
    Undo,
}

static ACTIVE_TEXT_EDIT_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static INSPECTOR_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static LOCKED_HWNDS: OnceLock<Mutex<HashSet<isize>>> = OnceLock::new();
type SizeMoveRectEntry = (isize, (i32, i32, i32, i32));

static PENDING_HOTKEYS: OnceLock<Mutex<Vec<(isize, Win32Hotkey)>>> = OnceLock::new();
static ACTIVE_SIZEMOVE_RECT: OnceLock<Mutex<Option<SizeMoveRectEntry>>> = OnceLock::new();

fn locked_hwnds() -> &'static Mutex<HashSet<isize>> {
    LOCKED_HWNDS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn pending_hotkeys() -> &'static Mutex<Vec<(isize, Win32Hotkey)>> {
    PENDING_HOTKEYS.get_or_init(|| Mutex::new(Vec::new()))
}

fn active_sizemove_rect() -> &'static Mutex<Option<SizeMoveRectEntry>> {
    ACTIVE_SIZEMOVE_RECT.get_or_init(|| Mutex::new(None))
}

pub fn live_sizemove_rect_for_hwnd(raw_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if raw_hwnd == 0 {
        return None;
    }
    (*lock_unpoisoned(active_sizemove_rect()))
        .filter(|(h, _)| *h == raw_hwnd)
        .map(|(_, r)| r)
}

pub fn set_inspector_hwnd(raw_hwnd: isize) {
    INSPECTOR_HWND.store(raw_hwnd, std::sync::atomic::Ordering::Release);
}

pub fn inspector_hwnd() -> isize {
    INSPECTOR_HWND.load(std::sync::atomic::Ordering::Acquire)
}

pub fn is_popup_menu_hwnd(raw_hwnd: isize) -> bool {
    if raw_hwnd == 0 {
        return false;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
        return false;
    }
    let mut buf = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len]) == "#32768"
}

pub fn set_active_text_edit_hwnd(raw_hwnd: isize) {
    ACTIVE_TEXT_EDIT_HWND.store(raw_hwnd, std::sync::atomic::Ordering::Release);
}

pub fn set_hwnd_locked(raw_hwnd: isize, locked: bool) {
    if raw_hwnd == 0 {
        return;
    }
    let mut set = lock_unpoisoned(locked_hwnds());
    if locked {
        set.insert(raw_hwnd);
    } else {
        set.remove(&raw_hwnd);
    }
}

fn is_hwnd_locked(hwnd: HWND) -> bool {
    lock_unpoisoned(locked_hwnds()).contains(&(hwnd.0 as isize))
}

pub fn take_win32_hotkeys(raw_hwnd: isize) -> Vec<Win32Hotkey> {
    if raw_hwnd == 0 {
        return Vec::new();
    }
    let mut queue = lock_unpoisoned(pending_hotkeys());
    let mut out = Vec::new();
    let mut remaining = Vec::with_capacity(queue.len());
    for (h, hk) in queue.drain(..) {
        if h == raw_hwnd {
            out.push(hk);
        } else {
            remaining.push((h, hk));
        }
    }
    *queue = remaining;
    out
}

fn decode_ctrl_hotkey(wparam: WPARAM, lparam: LPARAM) -> Option<Win32Hotkey> {
    // Ignore key-repeat bit (bit 30) so holding Ctrl+V/Z doesn't double-fire.
    if (lparam.0 & (1 << 30)) != 0 {
        return None;
    }
    let ctrl_down = (unsafe { GetKeyState(VK_CONTROL.0 as i32) } as u16 & 0x8000) != 0;
    let alt_down = (unsafe { GetKeyState(VK_MENU.0 as i32) } as u16 & 0x8000) != 0;
    if !ctrl_down || alt_down {
        return None;
    }
    let mut vk = (wparam.0 & 0xFFFF) as u32;
    if vk == u32::from(VK_PROCESSKEY.0) {
        let scancode = ((lparam.0 >> 16) & 0xFF) as u32;
        if scancode != 0 {
            vk = unsafe { MapVirtualKeyW(scancode, MAPVK_VSC_TO_VK_EX) };
        }
    }
    let shift_down = (unsafe { GetKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000) != 0;
    match (vk, shift_down) {
        (0x41, false) => Some(Win32Hotkey::SelectAll), // 'A'
        (0x43, false) => Some(Win32Hotkey::Copy),      // 'C'
        (0x43, true) => Some(Win32Hotkey::CopyPaths),  // Ctrl+Shift+C
        (0x58, false) => Some(Win32Hotkey::Cut),       // 'X'
        (0x56, false) => Some(Win32Hotkey::Paste),     // 'V'
        (0x5A, false) => Some(Win32Hotkey::Undo),      // 'Z'
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct SubclassEntry {
    orig_proc: isize,
    owner_hwnd: isize,
    pending_z: bool,
}

static ORIG_WNDPROCS: OnceLock<Mutex<HashMap<isize, SubclassEntry>>> = OnceLock::new();

fn orig_procs() -> &'static Mutex<HashMap<isize, SubclassEntry>> {
    ORIG_WNDPROCS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn snapshot_fence_hwnd_set() -> HashSet<isize> {
    let mut set: HashSet<isize> = lock_unpoisoned(orig_procs()).keys().copied().collect();
    let free = free_desktop_hwnd();
    if free != 0 {
        set.insert(free);
    }
    set
}

pub fn all_fence_hwnds() -> Vec<isize> {
    snapshot_fence_hwnd_set().into_iter().collect()
}

pub fn all_bucket_fence_hwnds() -> Vec<isize> {
    let free = free_desktop_hwnd();
    lock_unpoisoned(orig_procs())
        .keys()
        .copied()
        .filter(|&h| h != 0 && h != free)
        .collect()
}

static PENDING_CLOAK_HWNDS: OnceLock<Mutex<HashSet<isize>>> = OnceLock::new();

fn pending_cloak_hwnds() -> &'static Mutex<HashSet<isize>> {
    PENDING_CLOAK_HWNDS.get_or_init(|| Mutex::new(HashSet::new()))
}

unsafe fn set_window_cloaked_hwnd(hwnd: HWND, cloaked: bool) {
    let val: BOOL = cloaked.into();
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            (&raw const val).cast(),
            size_of::<BOOL>() as u32,
        )
    };
}

/// Sets `DWMWA_CLOAK` on a window so DWM skips compositing it while initial WGL/DWM setup runs.
pub fn set_window_cloaked(raw_hwnd: isize, cloaked: bool) {
    if raw_hwnd == 0 {
        return;
    }
    if cloaked {
        lock_unpoisoned(pending_cloak_hwnds()).insert(raw_hwnd);
    } else {
        lock_unpoisoned(pending_cloak_hwnds()).remove(&raw_hwnd);
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    if unsafe { IsWindow(Some(hwnd)).as_bool() } {
        unsafe { set_window_cloaked_hwnd(hwnd, cloaked) };
    }
}

/// Queues `WM_APP_UNCLOAK` so a window is revealed strictly after the current `SwapBuffers` finishes.
pub fn post_uncloak_after_swap(raw_hwnd: isize) {
    if raw_hwnd != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw_hwnd as *mut _)),
                WM_APP_UNCLOAK,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

pub struct UiThreadCloakGuard(HHOOK, HHOOK);

impl Drop for UiThreadCloakGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = UnhookWindowsHookEx(self.0);
            }
        }
        if !self.1.is_invalid() {
            unsafe {
                let _ = UnhookWindowsHookEx(self.1);
            }
        }
    }
}

unsafe fn title_ptr_matches_desklite(ptr: *const u16) -> bool {
    if ptr.is_null() || (ptr as usize) <= 0xFFFF {
        return false;
    }
    for (idx, &want) in b"DeskLite".iter().enumerate() {
        let ch = unsafe { *ptr.add(idx) };
        if ch != u16::from(want) {
            return false;
        }
    }
    true
}

unsafe extern "system" fn ui_cloak_cbt_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HCBT_CREATEWND as i32 && wparam.0 != 0 && lparam.0 != 0 {
        let cbt = unsafe { &*(lparam.0 as *const CBT_CREATEWNDW) };
        if !cbt.lpcs.is_null() {
            let cs = unsafe { &mut *cbt.lpcs };
            if ((cs.style as u32) & WS_CHILD.0) == 0
                && unsafe { title_ptr_matches_desklite(cs.lpszName.0) }
            {
                // Override winit's initial CW_USEDEFAULT placement before CreateWindowExW ever
                // maps the surface so zero frames can appear in the center of the screen.
                cs.x = -32000;
                cs.y = -32000;
                cs.cx = 1;
                cs.cy = 1;
                cs.dwExStyle.0 = (cs.dwExStyle.0 & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0;
                lock_unpoisoned(pending_cloak_hwnds()).insert(wparam.0 as isize);
                let hwnd = HWND(wparam.0 as *mut _);
                unsafe {
                    set_window_cloaked_hwnd(hwnd, true);
                    let no_trans: BOOL = true.into();
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_TRANSITIONS_FORCEDISABLED,
                        (&raw const no_trans).cast(),
                        size_of::<BOOL>() as u32,
                    );
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn ui_cloak_callwnd_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let cwp = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
        if cwp.message == WM_CREATE || (cwp.message == WM_SHOWWINDOW && cwp.wParam.0 != 0) {
            let raw = cwp.hwnd.0 as isize;
            if lock_unpoisoned(pending_cloak_hwnds()).contains(&raw) {
                unsafe {
                    set_window_cloaked_hwnd(cwp.hwnd, true);
                    let no_trans: BOOL = true.into();
                    let _ = DwmSetWindowAttribute(
                        cwp.hwnd,
                        DWMWA_TRANSITIONS_FORCEDISABLED,
                        (&raw const no_trans).cast(),
                        size_of::<BOOL>() as u32,
                    );
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Installs thread-local `WH_CBT` + `WH_CALLWNDPROC` hooks on the main UI thread so every
/// `DeskLite*` top-level `HWND` starts offscreen (`-32000, -32000, 1x1`) and stays DWM-cloaked
/// through `WM_CREATE` / `WM_SHOWWINDOW` until the first on-screen `SwapBuffers` finishes.
pub fn install_ui_thread_cloak_hook() -> UiThreadCloakGuard {
    let tid = unsafe { GetCurrentThreadId() };
    let cbt = unsafe { SetWindowsHookExW(WH_CBT, Some(ui_cloak_cbt_proc), None, tid) }
        .unwrap_or_default();
    let cwp = unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(ui_cloak_callwnd_proc), None, tid) }
        .unwrap_or_default();
    UiThreadCloakGuard(cbt, cwp)
}

/// Immediately hides all visible DeskLite bucket fences and inspector windows before
/// tearing down OpenGL/DWM resources so exit never flashes opaque white rectangles.
pub fn hide_all_fences_immediately() {
    for raw in all_bucket_fence_hwnds() {
        let hwnd = HWND(raw as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            unsafe {
                set_window_cloaked_hwnd(hwnd, true);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
    let insp = inspector_hwnd();
    if insp != 0 {
        let hwnd = HWND(insp as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            unsafe {
                set_window_cloaked_hwnd(hwnd, true);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

unsafe fn is_desktop_shell_class(hwnd: HWND) -> bool {
    let mut buf = [0u16; 48];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    let cls = String::from_utf16_lossy(&buf[..len]);
    matches!(cls.as_str(), "Progman" | "WorkerW" | "SHELLDLL_DefView" | "SysListView32")
}

unsafe fn is_window_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    };
    hr.is_ok() && cloaked != 0
}

unsafe fn resolve_live_owner(owner_hwnd: isize) -> isize {
    if let Some(tree) = probe_desktop_window_tree() {
        return tree.worker_w.unwrap_or(tree.progman).0 as isize;
    }
    if owner_hwnd != 0 && unsafe { IsWindow(Some(HWND(owner_hwnd as *mut _))).as_bool() } {
        return owner_hwnd;
    }
    0
}

unsafe fn visible_progman_hwnd() -> isize {
    match unsafe { FindWindowW(w!("Progman"), None) } {
        Ok(p) if !p.is_invalid() && unsafe { IsWindowVisible(p).as_bool() } => p.0 as isize,
        _ => 0,
    }
}

struct DesktopZOrderAnalysis {
    live_owner: isize,
    last_topmost: Option<HWND>,
    last_visible_app: Option<HWND>,
    is_compliant: bool,
    chain: Vec<isize>,
}

#[cfg(test)]
fn evaluate_z_chain_plan(
    above_owner_raws: &[isize],
    app_hwnds: &HashSet<isize>,
    expected_fences: &[isize],
    free_hwnd: Option<isize>,
    saw_owner: bool,
) -> (bool, Vec<isize>) {
    evaluate_z_chain_plan_with_preferred(
        above_owner_raws,
        app_hwnds,
        expected_fences,
        free_hwnd,
        saw_owner,
        None,
    )
}

fn evaluate_z_chain_plan_with_preferred(
    above_owner_raws: &[isize],
    app_hwnds: &HashSet<isize>,
    expected_fences: &[isize],
    free_hwnd: Option<isize>,
    saw_owner: bool,
    preferred_top: Option<isize>,
) -> (bool, Vec<isize>) {
    let expected_set: HashSet<isize> = expected_fences.iter().copied().collect();
    let mut seen_fences: Vec<isize> = Vec::with_capacity(expected_fences.len());
    let mut seen_fence_set: HashSet<isize> = HashSet::with_capacity(expected_fences.len());
    let mut seen_any_desklite = false;
    let mut saw_free = false;
    let mut free_before_fence = false;
    let mut app_below_desklite = false;

    for &raw in above_owner_raws {
        if free_hwnd == Some(raw) {
            seen_any_desklite = true;
            saw_free = true;
        } else if expected_set.contains(&raw) {
            seen_any_desklite = true;
            if saw_free {
                free_before_fence = true;
            }
            if seen_fence_set.insert(raw) {
                seen_fences.push(raw);
            }
        } else if seen_any_desklite && app_hwnds.contains(&raw) {
            app_below_desklite = true;
        }
    }

    let mut missing_fences: Vec<isize> = expected_fences
        .iter()
        .copied()
        .filter(|raw| !seen_fence_set.contains(raw))
        .collect();
    missing_fences.sort_unstable();

    let preferred_valid = preferred_top.filter(|p| expected_set.contains(p));
    let preferred_not_top = preferred_valid.is_some_and(|p| seen_fences.first().copied() != Some(p));

    let mut chain = Vec::with_capacity(expected_fences.len() + usize::from(free_hwnd.is_some()));
    if let Some(pref) = preferred_valid {
        chain.push(pref);
    }
    for raw in seen_fences {
        if Some(raw) != preferred_valid {
            chain.push(raw);
        }
    }
    for raw in missing_fences.iter().copied() {
        if Some(raw) != preferred_valid {
            chain.push(raw);
        }
    }
    if let Some(free) = free_hwnd {
        chain.push(free);
    }

    let is_compliant = saw_owner
        && missing_fences.is_empty()
        && free_hwnd.is_none_or(|_| saw_free)
        && !free_before_fence
        && !app_below_desklite
        && !preferred_not_top;

    (is_compliant, chain)
}

unsafe fn analyze_desktop_z_order(live_owner: isize) -> DesktopZOrderAnalysis {
    let expected_fences: Vec<isize> = all_bucket_fence_hwnds()
        .into_iter()
        .filter(|&raw| {
            let h = HWND(raw as *mut _);
            unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() }
        })
        .collect();
    let expected_set: HashSet<isize> = expected_fences.iter().copied().collect();

    let free_raw = free_desktop_hwnd();
    let visible_free = if free_raw != 0 {
        let fh = HWND(free_raw as *mut _);
        (unsafe { IsWindow(Some(fh)).as_bool() && IsWindowVisible(fh).as_bool() })
            .then_some(free_raw)
    } else {
        None
    };

    if live_owner == 0 {
        let mut chain = expected_fences;
        chain.sort_unstable();
        if let Some(f) = visible_free {
            chain.push(f);
        }
        return DesktopZOrderAnalysis {
            live_owner,
            last_topmost: None,
            last_visible_app: None,
            is_compliant: true,
            chain,
        };
    }

    let progman_visible = unsafe { visible_progman_hwnd() };
    let mut above_owner_raws = Vec::new();
    let mut app_hwnds = HashSet::new();
    let mut last_topmost: Option<HWND> = None;
    let mut last_visible_app: Option<HWND> = None;
    let mut saw_owner = false;

    let mut cur = match unsafe { GetTopWindow(None) } {
        Ok(top) if !top.is_invalid() => top,
        _ => {
            let mut chain = expected_fences;
            chain.sort_unstable();
            if let Some(f) = visible_free {
                chain.push(f);
            }
            return DesktopZOrderAnalysis {
                live_owner,
                last_topmost: None,
                last_visible_app: None,
                is_compliant: false,
                chain,
            };
        }
    };

    for _ in 0..4096 {
        let raw = cur.0 as isize;
        if raw == live_owner || (progman_visible != 0 && raw == progman_visible) {
            saw_owner = true;
            break;
        }
        let is_ours = expected_set.contains(&raw) || visible_free == Some(raw);
        if is_ours {
            above_owner_raws.push(raw);
        } else if unsafe { IsWindowVisible(cur).as_bool() } && !unsafe { IsIconic(cur).as_bool() }
        {
            let ex = unsafe { GetWindowLongPtrW(cur, GWL_EXSTYLE) } as u32;
            if (ex & WS_EX_TOPMOST.0) != 0 {
                last_topmost = Some(cur);
            } else if !unsafe { is_desktop_shell_class(cur) } && !unsafe { is_window_cloaked(cur) }
            {
                let mut rc = RECT::default();
                if unsafe { GetWindowRect(cur, &mut rc) }.is_ok()
                    && (rc.right - rc.left) > 1
                    && (rc.bottom - rc.top) > 1
                {
                    last_visible_app = Some(cur);
                    app_hwnds.insert(raw);
                    above_owner_raws.push(raw);
                }
            }
        }
        cur = match unsafe { GetWindow(cur, GW_HWNDNEXT) } {
            Ok(next) if !next.is_invalid() => next,
            _ => break,
        };
    }

    let pref = PREFERRED_TOP_FENCE_HWND.load(std::sync::atomic::Ordering::Acquire);
    let preferred_top = (pref != 0).then_some(pref);
    let (is_compliant, chain) = evaluate_z_chain_plan_with_preferred(
        &above_owner_raws,
        &app_hwnds,
        &expected_fences,
        visible_free,
        saw_owner,
        preferred_top,
    );

    DesktopZOrderAnalysis {
        live_owner,
        last_topmost,
        last_visible_app,
        is_compliant,
        chain,
    }
}

unsafe fn are_desktop_windows_in_layer() -> bool {
    let live_owner = unsafe { resolve_live_owner(0) };
    if live_owner == 0 {
        return true;
    }
    unsafe { analyze_desktop_z_order(live_owner) }.is_compliant
}

struct InFlightReorderGuard;

impl InFlightReorderGuard {
    fn enter() -> Self {
        SELF_Z_REORDER_IN_FLIGHT.store(true, std::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for InFlightReorderGuard {
    fn drop(&mut self) {
        REASSERTING_HWND.store(0, std::sync::atomic::Ordering::Release);
        SELF_Z_REORDER_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

struct ReassertingHwndGuard;

impl ReassertingHwndGuard {
    fn enter(raw: isize) -> Self {
        REASSERTING_HWND.store(raw, std::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for ReassertingHwndGuard {
    fn drop(&mut self) {
        REASSERTING_HWND.store(0, std::sync::atomic::Ordering::Release);
    }
}

/// Synchronously verifies and, only if out of layer order, restores the full desktop Z-order
/// chain (`visible app windows > [Fence_1..k] > FreeDesktop > WorkerW/Progman`) in a single pass.
pub fn reassert_desktop_z_order_sync() {
    Z_REASSERT_QUEUED.store(false, std::sync::atomic::Ordering::Release);
    if is_in_size_move() {
        DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
        return;
    }

    let _serial_guard = lock_unpoisoned(Z_REASSERT_LOCK.get_or_init(|| Mutex::new(())));
    let _in_flight = InFlightReorderGuard::enter();

    let live_owner = unsafe { resolve_live_owner(0) };
    if live_owner == 0 {
        return;
    }

    let analysis = unsafe { analyze_desktop_z_order(live_owner) };
    if analysis.is_compliant || analysis.chain.is_empty() {
        return;
    }

    for &raw in &analysis.chain {
        let h = HWND(raw as *mut _);
        if !unsafe { IsWindow(Some(h)).as_bool() } {
            continue;
        }
        let cur_owner = unsafe { GetWindowLongPtrW(h, GWLP_HWNDPARENT) };
        if cur_owner != analysis.live_owner {
            unsafe { SetWindowLongPtrW(h, GWLP_HWNDPARENT, analysis.live_owner) };
        }
        if let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&raw) {
            e.owner_hwnd = analysis.live_owner;
            e.pending_z = false;
        }
    }

    let mut prev = analysis
        .last_visible_app
        .or(analysis.last_topmost)
        .unwrap_or(HWND_TOP);
    for &raw in &analysis.chain {
        let w = HWND(raw as *mut _);
        if !unsafe { IsWindow(Some(w)).as_bool() } {
            continue;
        }
        let _hwnd_guard = ReassertingHwndGuard::enter(raw);
        let _ = unsafe {
            SetWindowPos(
                w,
                Some(prev),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOSENDCHANGING,
            )
        };
        prev = w;
    }
}

fn has_sibling_fence_above(
    ordered_above: &[isize],
    sibling_fences: &HashSet<isize>,
    target: isize,
) -> bool {
    for &raw in ordered_above {
        if raw == target {
            return false;
        }
        if sibling_fences.contains(&raw) {
            return true;
        }
    }
    false
}

/// Marks a focused or dragged bucket fence as preferred top of the Desktop layer and
/// asynchronously asks the helper Z-order coordinator to raise it above sibling fences.
/// Lock-free wrt `Z_REASSERT_LOCK` and performs no cross-thread `SetWindowPos`, eliminating UI/helper AB-BA deadlocks.
pub fn promote_fence_above_siblings(raw_hwnd: isize) {
    if raw_hwnd == 0 || is_free_desktop_hwnd(raw_hwnd) || raw_hwnd == inspector_hwnd() {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    if !unsafe { IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool() } {
        return;
    }
    let fence_hwnds = snapshot_fence_hwnd_set();
    if !fence_hwnds.contains(&raw_hwnd) {
        return;
    }
    PREFERRED_TOP_FENCE_HWND.store(raw_hwnd, std::sync::atomic::Ordering::Release);
    let free_raw = free_desktop_hwnd();

    let mut sibling_fences: HashSet<isize> = fence_hwnds;
    sibling_fences.remove(&free_raw);

    let mut ordered_above = Vec::new();
    let mut cur = hwnd;
    for _ in 0..4096 {
        let Ok(prev) = (unsafe { GetWindow(cur, GW_HWNDPREV) }) else {
            break;
        };
        if prev.is_invalid() {
            break;
        }
        let prev_raw = prev.0 as isize;
        if sibling_fences.contains(&prev_raw) && unsafe { IsWindowVisible(prev).as_bool() } {
            ordered_above.push(prev_raw);
        }
        cur = prev;
    }
    ordered_above.reverse();

    if has_sibling_fence_above(&ordered_above, &sibling_fences, raw_hwnd) {
        post_reassert_all_fences_z();
    }
}

/// Walks top-down from `GetTopWindow(None)` down to `owner_hwnd` (or visible `Progman`) and
/// returns the `hwndInsertAfter` anchor preserving strict layering:
/// `visible app windows > DeskLite bucket fences > FreeDesktop canvas > WorkerW/Progman`.
unsafe fn find_desktop_layer_anchor(
    self_hwnd: HWND,
    owner_hwnd: isize,
    fence_hwnds: &HashSet<isize>,
) -> HWND {
    let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
    if live_owner == 0 {
        return HWND_BOTTOM;
    }
    let progman_visible = unsafe { visible_progman_hwnd() };
    let mut cur = match unsafe { GetTopWindow(None) } {
        Ok(top) if !top.is_invalid() => top,
        _ => return HWND_BOTTOM,
    };

    let free_raw = free_desktop_hwnd();
    let self_raw = self_hwnd.0 as isize;
    let is_free = free_raw != 0 && self_raw == free_raw;

    let mut last_topmost: Option<HWND> = None;
    let mut last_visible_app: Option<HWND> = None;
    let mut last_bucket_fence: Option<HWND> = None;

    for _ in 0..4096 {
        let raw = cur.0 as isize;
        if raw == live_owner || (progman_visible != 0 && raw == progman_visible) {
            break;
        }
        let is_ours = cur == self_hwnd
            || fence_hwnds.contains(&raw)
            || (free_raw != 0 && raw == free_raw);
        if is_free
            && cur != self_hwnd
            && fence_hwnds.contains(&raw)
            && raw != free_raw
            && unsafe { IsWindowVisible(cur).as_bool() }
        {
            last_bucket_fence = Some(cur);
        }
        if !is_ours && unsafe { IsWindowVisible(cur).as_bool() } && !unsafe { IsIconic(cur).as_bool() } {
            let ex = unsafe { GetWindowLongPtrW(cur, GWL_EXSTYLE) } as u32;
            if (ex & WS_EX_TOPMOST.0) != 0 {
                last_topmost = Some(cur);
            } else if !unsafe { is_desktop_shell_class(cur) } && !unsafe { is_window_cloaked(cur) } {
                let mut rc = RECT::default();
                if unsafe { GetWindowRect(cur, &mut rc) }.is_ok()
                    && (rc.right - rc.left) > 1
                    && (rc.bottom - rc.top) > 1
                {
                    last_visible_app = Some(cur);
                }
            }
        }
        cur = match unsafe { GetWindow(cur, GW_HWNDNEXT) } {
            Ok(next) if !next.is_invalid() => next,
            _ => break,
        };
    }

    if is_free {
        last_bucket_fence
            .or(last_visible_app)
            .or(last_topmost)
            .unwrap_or(HWND_TOP)
    } else {
        last_visible_app
            .or(last_topmost)
            .unwrap_or(HWND_TOP)
    }
}

unsafe fn is_strictly_in_desktop_layer(
    hwnd: HWND,
    owner_hwnd: isize,
    fence_hwnds: &HashSet<isize>,
) -> bool {
    let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
    if live_owner == 0 {
        return true;
    }
    let progman_visible = unsafe { visible_progman_hwnd() };
    let free_raw = free_desktop_hwnd();
    let self_raw = hwnd.0 as isize;
    let is_free = free_raw != 0 && self_raw == free_raw;
    let free_visible =
        free_raw != 0 && unsafe { IsWindowVisible(HWND(free_raw as *mut _)).as_bool() };

    let mut cur = hwnd;
    let mut saw_owner = false;
    let mut saw_free_below = false;
    for _ in 0..4096 {
        let Ok(next) = (unsafe { GetWindow(cur, GW_HWNDNEXT) }) else {
            return false;
        };
        if next.is_invalid() {
            return false;
        }
        let next_raw = next.0 as isize;
        if next_raw == live_owner || (progman_visible != 0 && next_raw == progman_visible) {
            saw_owner = true;
            break;
        }
        if free_raw != 0 && next_raw == free_raw {
            saw_free_below = true;
        }
        if is_free
            && fence_hwnds.contains(&next_raw)
            && next_raw != free_raw
            && unsafe { IsWindowVisible(next).as_bool() }
        {
            return false;
        }
        if !fence_hwnds.contains(&next_raw)
            && next_raw != free_raw
            && unsafe { IsWindowVisible(next).as_bool() }
            && !unsafe { IsIconic(next).as_bool() }
            && !unsafe { is_desktop_shell_class(next) }
            && !unsafe { is_window_cloaked(next) }
        {
            let mut rc = RECT::default();
            if unsafe { GetWindowRect(next, &mut rc) }.is_ok()
                && (rc.right - rc.left) > 1
                && (rc.bottom - rc.top) > 1
            {
                return false;
            }
        }
        cur = next;
    }
    saw_owner && (is_free || !free_visible || saw_free_below)
}

pub struct DesktopZHookGuard(Vec<HWINEVENTHOOK>);

impl Drop for DesktopZHookGuard {
    fn drop(&mut self) {
        for h in self.0.drain(..) {
            if !h.is_invalid() {
                unsafe {
                    let _ = UnhookWinEvent(h);
                }
            }
        }
    }
}

unsafe extern "system" fn desktop_z_win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND && !hwnd.is_invalid() {
        let raw = hwnd.0 as isize;
        let is_ours = is_free_desktop_hwnd(raw)
            || raw == inspector_hwnd()
            || lock_unpoisoned(orig_procs()).contains_key(&raw);
        if !is_ours && !is_popup_menu_hwnd(raw) {
            let notify_hwnd = EXTERNAL_FG_NOTIFY_HWND.load(std::sync::atomic::Ordering::Acquire);
            let notify_msg = EXTERNAL_FG_NOTIFY_MSG.load(std::sync::atomic::Ordering::Acquire);
            if notify_hwnd != 0 && notify_msg != 0 {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(notify_hwnd as *mut _)),
                        notify_msg,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        }
        if SELF_Z_REORDER_IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if is_in_size_move() {
            DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        if unsafe { are_desktop_windows_in_layer() } {
            return;
        }
        post_reassert_all_fences_z();
        return;
    }

    if SELF_Z_REORDER_IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    if event == EVENT_OBJECT_REORDER {
        if hwnd.is_invalid() {
            return;
        }
        let is_desktop_root = hwnd == unsafe { GetDesktopWindow() };
        if !is_desktop_root && !unsafe { is_desktop_shell_class(hwnd) } {
            return;
        }
    }
    if is_in_size_move() {
        DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
        return;
    }
    post_reassert_all_fences_z();
}

/// Installs OS event-driven WinEvent hooks on the calling message-loop thread
/// (`EVENT_SYSTEM_FOREGROUND`, `EVENT_SYSTEM_MINIMIZESTART..=END`, and `WorkerW`/`Progman`
/// `EVENT_OBJECT_REORDER`) so `Win+D` immediately keeps fences above `WorkerW` with 0% idle CPU.
pub fn install_desktop_z_win_event_hooks() -> DesktopZHookGuard {
    let mut hooks = Vec::new();
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_REORDER, EVENT_OBJECT_REORDER),
    ];
    for (min_ev, max_ev) in ranges {
        let h = unsafe {
            SetWinEventHook(
                min_ev,
                max_ev,
                None,
                Some(desktop_z_win_event_proc),
                0,
                0,
                flags,
            )
        };
        if !h.is_invalid() {
            hooks.push(h);
        }
    }
    DesktopZHookGuard(hooks)
}

/// Asynchronously requests a desktop Z-order verification/reassertion pass. When
/// `set_z_reassert_notify_target` has been configured, multiple triggers coalesce into a single
/// helper-window message so `[Fence_1..k, FreeDesktop]` reorder atomically in one sequence.
pub fn post_reassert_all_fences_z() {
    if is_in_size_move() {
        DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
        return;
    }
    let notify_hwnd = Z_REASSERT_NOTIFY_HWND.load(std::sync::atomic::Ordering::Acquire);
    let notify_msg = Z_REASSERT_NOTIFY_MSG.load(std::sync::atomic::Ordering::Acquire);
    if notify_hwnd != 0 && notify_msg != 0 {
        if Z_REASSERT_QUEUED
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            let ok = unsafe {
                PostMessageW(
                    Some(HWND(notify_hwnd as *mut _)),
                    notify_msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            if ok.is_err() {
                Z_REASSERT_QUEUED.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        return;
    }

    let hwnds: Vec<isize> = {
        let mut map = lock_unpoisoned(orig_procs());
        let mut out = Vec::with_capacity(map.len());
        for (&raw, entry) in map.iter_mut() {
            if raw != 0 && !entry.pending_z {
                entry.pending_z = true;
                out.push(raw);
            }
        }
        out
    };
    for raw in hwnds {
        let ok = unsafe {
            PostMessageW(
                Some(HWND(raw as *mut _)),
                WM_APP_ENSURE_DESKTOP_Z,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if ok.is_err()
            && let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&raw)
        {
            e.pending_z = false;
        }
    }
}

unsafe fn hit_test_resize_border(hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    if is_file_drag_active() || is_hwnd_locked(hwnd) {
        return None;
    }
    let mut wr = windows::Win32::Foundation::RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
        return None;
    }
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
    let edge = ((6 * dpi + 48) / 96).max(4);
    let collapsed = (wr.bottom - wr.top) <= (48 * dpi / 96);

    let left = x >= wr.left && (x - wr.left) < edge;
    let right = x <= wr.right && (wr.right - x) < edge;
    let top = y >= wr.top && (y - wr.top) < edge;
    let bottom = y <= wr.bottom && (wr.bottom - y) < edge;

    if collapsed {
        return match (left, right) {
            (true, _) => Some(LRESULT(HTLEFT as isize)),
            (_, true) => Some(LRESULT(HTRIGHT as isize)),
            _ => None,
        };
    }

    let code = match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, _, _, _) => HTLEFT,
        (_, true, _, _) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => return None,
    };
    Some(LRESULT(code as isize))
}

unsafe fn clamp_rect_to_cursor_work_area(rect: &mut RECT, preserve_size: bool) {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        pt.x = (rect.left + rect.right) / 2;
        pt.y = (rect.top + rect.bottom) / 2;
    }
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        return;
    }
    let work = mi.rcWork;
    if preserve_size {
        let w = (rect.right - rect.left).max(1);
        let h = (rect.bottom - rect.top).max(1);
        let max_l = (work.right - w).max(work.left);
        let max_t = (work.bottom - h).max(work.top);
        let nl = rect.left.clamp(work.left, max_l);
        let nt = rect.top.clamp(work.top, max_t);
        rect.left = nl;
        rect.top = nt;
        rect.right = nl + w;
        rect.bottom = nt + h;
    } else {
        rect.left = rect.left.max(work.left);
        rect.top = rect.top.max(work.top);
        rect.right = rect.right.min(work.right);
        rect.bottom = rect.bottom.min(work.bottom);
        if rect.right <= rect.left {
            rect.right = rect.left + 1;
        }
        if rect.bottom <= rect.top {
            rect.bottom = rect.top + 1;
        }
    }
}

unsafe extern "system" fn bottom_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if msg == WM_NCCALCSIZE && wparam.0 != 0 {
        return LRESULT(0);
    }
    if msg == WM_NCPAINT {
        return LRESULT(0);
    }
    if msg == WM_APP_UNCLOAK {
        if !is_free_desktop_hwnd(hwnd.0 as isize) && unsafe { IsWindowVisible(hwnd).as_bool() } {
            lock_unpoisoned(pending_cloak_hwnds()).remove(&(hwnd.0 as isize));
            unsafe { set_window_cloaked_hwnd(hwnd, false) };
        }
        return LRESULT(0);
    }
    if msg == WM_STYLECHANGING && lparam.0 != 0 {
        let ss = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        if wparam.0 == GWL_STYLE.0 as usize {
            ss.styleNew = (ss.styleNew & !DISALLOWED_STYLES) | WS_POPUP.0;
        } else if wparam.0 == GWL_EXSTYLE.0 as usize {
            ss.styleNew = (ss.styleNew & !DISALLOWED_EX_STYLES) | WS_EX_TOOLWINDOW.0;
        }
    }
    if msg == WM_SYSCOMMAND {
        let cmd = (wparam.0 as u32) & 0xFFF0;
        if cmd == SC_MINIMIZE || cmd == SC_MAXIMIZE || cmd == SC_RESTORE {
            return LRESULT(0);
        }
    }
    let is_free = is_free_desktop_hwnd(hwnd.0 as isize);
    if !is_free && msg == WM_MOVING && lparam.0 != 0 {
        let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
        unsafe { clamp_rect_to_cursor_work_area(rect, true) };
        *lock_unpoisoned(active_sizemove_rect()) =
            Some((hwnd.0 as isize, (rect.left, rect.top, rect.right, rect.bottom)));
        let insp = INSPECTOR_HWND.load(std::sync::atomic::Ordering::Acquire);
        if insp != 0 {
            unsafe {
                let _ = RedrawWindow(
                    Some(HWND(insp as *mut _)),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_INTERNALPAINT,
                );
            }
        }
        return LRESULT(1);
    }
    if !is_free && msg == WM_SIZING && lparam.0 != 0 {
        let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
        unsafe { clamp_rect_to_cursor_work_area(rect, false) };
        *lock_unpoisoned(active_sizemove_rect()) =
            Some((hwnd.0 as isize, (rect.left, rect.top, rect.right, rect.bottom)));
        let insp = INSPECTOR_HWND.load(std::sync::atomic::Ordering::Acquire);
        if insp != 0 {
            unsafe {
                let _ = RedrawWindow(
                    Some(HWND(insp as *mut _)),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_INTERNALPAINT,
                );
            }
        }
        return LRESULT(1);
    }
    if !is_free && msg == WM_ENTERSIZEMOVE {
        IN_SIZE_MOVE_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Release);
        promote_fence_above_siblings(hwnd.0 as isize);
    }
    if msg == WM_EXITSIZEMOVE || msg == WM_CAPTURECHANGED || msg == WM_CANCELMODE {
        let raw = hwnd.0 as isize;
        let mut active = lock_unpoisoned(active_sizemove_rect());
        if active.is_some_and(|(h, _)| h == raw) {
            *active = None;
        }
        drop(active);
        if IN_SIZE_MOVE_HWND
            .compare_exchange(
                raw,
                0,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
            && DEFERRED_Z_AFTER_SIZEMOVE.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            post_reassert_all_fences_z();
        }
    }
    if !is_free
        && msg == WM_NCHITTEST
        && let Some(hit) = unsafe { hit_test_resize_border(hwnd, lparam) }
    {
        return hit;
    }
    if msg == WM_SETCURSOR
        && (is_file_drag_active() || (is_free && (lparam.0 & 0xFFFF) as u16 == HTCLIENT as u16))
    {
        if let Ok(arrow) = unsafe { LoadCursorW(None, IDC_ARROW) } {
            unsafe {
                let _ = SetCursor(Some(arrow));
            }
        }
        return LRESULT(1);
    }
    if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
        if !is_free {
            promote_fence_above_siblings(hwnd.0 as isize);
        }
        unsafe {
            if GetForegroundWindow() != hwnd {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
    if !is_free
        && msg == WM_KEYDOWN
        && ACTIVE_TEXT_EDIT_HWND.load(std::sync::atomic::Ordering::Acquire) != (hwnd.0 as isize)
        && let Some(hk) = decode_ctrl_hotkey(wparam, lparam)
    {
        lock_unpoisoned(pending_hotkeys()).push((hwnd.0 as isize, hk));
        unsafe {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_INTERNALPAINT);
        }
        return LRESULT(0);
    }

    let (entry, owner_hwnd) = {
        let map = lock_unpoisoned(orig_procs());
        let entry = map.get(&(hwnd.0 as isize)).copied();
        let owner = entry.map(|e| e.owner_hwnd).unwrap_or(0);
        (entry, owner)
    };

    if msg == WM_NCACTIVATE {
        // Forward to original winit proc (so `is_active` updates and `ctx.input(|i| i.focused)` works)
        // while passing LPARAM(-1) to suppress DefWindowProc's white non-client border flash.
        if let Some(sub) = entry {
            let wndproc: WNDPROC = unsafe { std::mem::transmute(sub.orig_proc) };
            return unsafe { CallWindowProcW(wndproc, hwnd, msg, wparam, LPARAM(-1)) };
        }
    }

    if msg == WM_APP_ENSURE_DESKTOP_Z {
        if let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&(hwnd.0 as isize)) {
            e.pending_z = false;
        }
        post_reassert_all_fences_z();
        return LRESULT(0);
    }

    if msg == WM_WINDOWPOSCHANGING {
        let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if REASSERTING_HWND.load(std::sync::atomic::Ordering::Acquire) == (hwnd.0 as isize) {
            // Coordinated Z-order pass is positioning this window explicitly.
        } else if (wp.flags & SWP_NOZORDER).0 == 0 && owner_hwnd != 0 {
            if is_in_size_move() {
                wp.flags |= SWP_NOZORDER;
                DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
            } else {
                let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
                if live_owner != 0 && live_owner != owner_hwnd {
                    if let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&(hwnd.0 as isize)) {
                        e.owner_hwnd = live_owner;
                    }
                    post_reassert_all_fences_z();
                }
                let fence_hwnds = snapshot_fence_hwnd_set();
                let already_ok =
                    unsafe { is_strictly_in_desktop_layer(hwnd, live_owner, &fence_hwnds) };
                if already_ok {
                    wp.flags |= SWP_NOZORDER;
                } else {
                    let anchor =
                        unsafe { find_desktop_layer_anchor(hwnd, live_owner, &fence_hwnds) };
                    if anchor != hwnd {
                        wp.hwndInsertAfter = anchor;
                    }
                    post_reassert_all_fences_z();
                }
            }
        }
    }

    if msg == WM_NCDESTROY {
        let raw = hwnd.0 as isize;
        let _ = IN_SIZE_MOVE_HWND.compare_exchange(
            raw,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::Relaxed,
        );
        let _ = FREE_DESKTOP_HWND.compare_exchange(
            raw,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::Relaxed,
        );
        let _ = ACTIVE_TEXT_EDIT_HWND.compare_exchange(
            raw,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::Relaxed,
        );
        {
            let mut active = lock_unpoisoned(active_sizemove_rect());
            if active.is_some_and(|(h, _)| h == raw) {
                *active = None;
            }
        }
        lock_unpoisoned(locked_hwnds()).remove(&raw);
        lock_unpoisoned(pending_hotkeys()).retain(|(h, _)| *h != raw);
        lock_unpoisoned(pending_cloak_hwnds()).remove(&raw);
        lock_unpoisoned(orig_procs()).remove(&raw);
    }
    if let Some(sub) = entry {
        let wndproc: WNDPROC = unsafe { std::mem::transmute(sub.orig_proc) };
        unsafe { CallWindowProcW(wndproc, hwnd, msg, wparam, lparam) }
    } else {
        LRESULT(0)
    }
}

pub fn foreground_window_handle() -> isize {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() { 0 } else { hwnd.0 as isize }
}

pub fn find_window_by_exact_title(title: &str) -> Option<isize> {
    let wtitle = HSTRING::from(title);
    let hwnd = unsafe { FindWindowW(None, &wtitle) }.ok()?;
    if hwnd.is_invalid() {
        None
    } else {
        Some(hwnd.0 as isize)
    }
}

pub fn strip_alt_tab_from_host(raw_hwnd: isize) -> bool {
    if raw_hwnd == 0 {
        return false;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let clean_style =
            (style & !(WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0))
                | WS_POPUP.0;
        if clean_style != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, clean_style as isize);
        }

        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let next = (ex & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0;
        if next != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next as isize);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        if let Some(tree) = probe_desktop_window_tree() {
            let owner = tree.worker_w.unwrap_or(tree.progman);
            if GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT) != owner.0 as isize {
                SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize);
            }
        }
        let no_trans: BOOL = true.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const no_trans).cast(),
            size_of::<BOOL>() as u32,
        );
        // Host window is an invisible root owner for deferred viewports; keep it cloaked permanently.
        set_window_cloaked_hwnd(hwnd, true);
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0
    }
}

unsafe fn apply_transparent_canvas_chrome(hwnd: HWND, ex_extra: u32) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let clean_style = (style & !DISALLOWED_STYLES) | WS_POPUP.0;
        if clean_style != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, clean_style as isize);
        }

        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let next = (ex & !DISALLOWED_EX_STYLES) | WS_EX_TOOLWINDOW.0 | ex_extra;
        if next != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next as isize);
        }

        let no_trans: BOOL = true.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const no_trans).cast(),
            size_of::<BOOL>() as u32,
        );

        let no_border = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&raw const no_border).cast(),
            size_of::<u32>() as u32,
        );
    }
}

unsafe fn apply_glass_dwm_chrome(hwnd: HWND, ex_extra: u32) {
    unsafe {
        apply_transparent_canvas_chrome(hwnd, ex_extra);

        let dark: BOOL = true.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            size_of::<BOOL>() as u32,
        );

        let backdrop = DWMSBT_NONE;
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

unsafe fn exclude_from_aero_peek(hwnd: HWND) {
    let excluded: BOOL = true.into();
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_EXCLUDED_FROM_PEEK,
            (&raw const excluded).cast(),
            size_of::<BOOL>() as u32,
        )
    };
}

/// Configures a DeskLite fence `HWND`:
/// - Strips `WS_CAPTION | WS_SYSMENU` so DWM never draws phantom `[x]` close glows.
/// - Strips `WS_EX_APPWINDOW` and adds `WS_EX_TOOLWINDOW` so fences never clutter `Alt+Tab`.
/// - Disables DWM border & transition animations (`DWMWA_COLOR_NONE`, `DWMWA_TRANSITIONS_FORCEDISABLED`),
///   keeps `DWMSBT_NONE` so focus switches never flash acrylic, and enables `DWMWA_EXCLUDED_FROM_PEEK`.
/// - Attaches desktop owner (`WorkerW` / `Progman`) and subclasses Z-order to stay below all
///   visible app windows yet strictly above `FreeDesktop` and `WorkerW`.
pub fn attach_desktop_owner_and_rounded_corners(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut _);
    unsafe {
        let owner_hwnd = resolve_live_owner(0);
        if owner_hwnd != 0 {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner_hwnd);
        }

        apply_glass_dwm_chrome(hwnd, 0);
        exclude_from_aero_peek(hwnd);

        {
            let mut map = lock_unpoisoned(orig_procs());
            if let std::collections::hash_map::Entry::Vacant(e) = map.entry(raw_hwnd) {
                let prev =
                    SetWindowLongPtrW(hwnd, GWLP_WNDPROC, bottom_subclass_proc as *const () as isize);
                if prev != 0 {
                    e.insert(SubclassEntry {
                        orig_proc: prev,
                        owner_hwnd,
                        pending_z: false,
                    });
                }
            }
        }
        let fence_hwnds = snapshot_fence_hwnd_set();
        let insert_after = find_desktop_layer_anchor(hwnd, owner_hwnd, &fence_hwnds);

        let _ = SetWindowPos(
            hwnd,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Configures the full-screen transparent `FreeDesktop` canvas `HWND` to sit strictly
/// below all DeskLite bucket fences and visible app windows, yet above `WorkerW`/`Progman`.
pub fn attach_free_desktop_canvas(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    FREE_DESKTOP_HWND.store(raw_hwnd, std::sync::atomic::Ordering::Release);
    let hwnd = HWND(raw_hwnd as *mut _);
    unsafe {
        let owner_hwnd = resolve_live_owner(0);
        if owner_hwnd != 0 {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner_hwnd);
        }

        apply_transparent_canvas_chrome(hwnd, 0);
        exclude_from_aero_peek(hwnd);

        {
            let mut map = lock_unpoisoned(orig_procs());
            if let std::collections::hash_map::Entry::Vacant(e) = map.entry(raw_hwnd) {
                let prev =
                    SetWindowLongPtrW(hwnd, GWLP_WNDPROC, bottom_subclass_proc as *const () as isize);
                if prev != 0 {
                    e.insert(SubclassEntry {
                        orig_proc: prev,
                        owner_hwnd,
                        pending_z: false,
                    });
                }
            }
        }
        let fence_hwnds = snapshot_fence_hwnd_set();
        let insert_after = find_desktop_layer_anchor(hwnd, owner_hwnd, &fence_hwnds);

        let _ = SetWindowPos(
            hwnd,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    post_reassert_all_fences_z();
}

static INSPECTOR_ORIG_PROCS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

fn inspector_orig_procs() -> &'static Mutex<HashMap<isize, isize>> {
    INSPECTOR_ORIG_PROCS.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe extern "system" fn inspector_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    if msg == WM_NCCALCSIZE && wparam.0 != 0 {
        return LRESULT(0);
    }
    if msg == WM_NCPAINT {
        return LRESULT(0);
    }
    if msg == WM_APP_UNCLOAK {
        if unsafe { IsWindowVisible(hwnd).as_bool() } {
            lock_unpoisoned(pending_cloak_hwnds()).remove(&(hwnd.0 as isize));
            unsafe { set_window_cloaked_hwnd(hwnd, false) };
        }
        return LRESULT(0);
    }
    if msg == WM_STYLECHANGING && lparam.0 != 0 {
        let ss = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        if wparam.0 == GWL_STYLE.0 as usize {
            ss.styleNew = (ss.styleNew & !DISALLOWED_STYLES) | WS_POPUP.0;
        } else if wparam.0 == GWL_EXSTYLE.0 as usize {
            ss.styleNew =
                (ss.styleNew & !DISALLOWED_EX_STYLES) | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;
        }
    }
    if msg == WM_MOVING && lparam.0 != 0 {
        let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
        unsafe { clamp_rect_to_cursor_work_area(rect, true) };
        return LRESULT(1);
    }
    if msg == WM_NCHITTEST
        && let Some(hit) = unsafe { hit_test_resize_border(hwnd, lparam) }
    {
        return hit;
    }
    let orig = lock_unpoisoned(inspector_orig_procs())
        .get(&(hwnd.0 as isize))
        .copied();
    if msg == WM_NCACTIVATE
        && let Some(prev) = orig
    {
        let wndproc: WNDPROC = unsafe { std::mem::transmute(prev) };
        return unsafe { CallWindowProcW(wndproc, hwnd, msg, wparam, LPARAM(-1)) };
    }
    if msg == WM_NCDESTROY {
        let raw = hwnd.0 as isize;
        lock_unpoisoned(pending_cloak_hwnds()).remove(&raw);
        let _ = INSPECTOR_HWND.compare_exchange(
            raw,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::Relaxed,
        );
        lock_unpoisoned(inspector_orig_procs()).remove(&raw);
    }
    if let Some(prev) = orig {
        let wndproc: WNDPROC = unsafe { std::mem::transmute(prev) };
        unsafe { CallWindowProcW(wndproc, hwnd, msg, wparam, lparam) }
    } else {
        LRESULT(0)
    }
}

/// Applies rounded dark-glass styling for DeskLite's floating inspector window without pinning it to bottom Z-order.
pub fn style_floating_inspector_window(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
    }
    set_inspector_hwnd(raw_hwnd);
    let hwnd = HWND(raw_hwnd as *mut _);
    unsafe {
        apply_glass_dwm_chrome(hwnd, WS_EX_TOPMOST.0);
        let mut map = lock_unpoisoned(inspector_orig_procs());
        if let std::collections::hash_map::Entry::Vacant(e) = map.entry(raw_hwnd) {
            let prev =
                SetWindowLongPtrW(hwnd, GWLP_WNDPROC, inspector_subclass_proc as *const () as isize);
            if prev != 0 {
                e.insert(prev);
            }
        }
        drop(map);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_z_chain_plan_accepts_compliant_desktop_layer() {
        let apps: HashSet<isize> = [10, 11].into_iter().collect();
        let fences = [101, 102];
        let (ok, chain) = evaluate_z_chain_plan(&[10, 11, 102, 101, 200], &apps, &fences, Some(200), true);
        assert!(ok);
        assert_eq!(chain, vec![102, 101, 200]);
    }

    #[test]
    fn evaluate_z_chain_plan_detects_dropped_fence_and_misplaced_free_or_app() {
        let apps: HashSet<isize> = [10, 11].into_iter().collect();
        let fences = [101, 102, 103];

        // FreeDesktop above fence 101, and fence 103 missing (dropped below WorkerW).
        let (ok, chain) = evaluate_z_chain_plan(&[10, 200, 101, 102], &apps, &fences, Some(200), true);
        assert!(!ok);
        assert_eq!(chain, vec![101, 102, 103, 200]);

        // Visible app window slipped below a fence.
        let (ok2, chain2) = evaluate_z_chain_plan(&[10, 101, 11, 102, 103, 200], &apps, &fences, Some(200), true);
        assert!(!ok2);
        assert_eq!(chain2, vec![101, 102, 103, 200]);

        // Owner never reached in scan.
        let (ok3, _) = evaluate_z_chain_plan(&[10, 101, 102, 103, 200], &apps, &fences, Some(200), false);
        assert!(!ok3);
    }

    #[test]
    fn has_sibling_fence_above_detects_covering_fences_only() {
        let siblings: HashSet<isize> = [101, 102, 103].into_iter().collect();
        assert!(!has_sibling_fence_above(&[10, 101, 102, 103], &siblings, 101));
        assert!(has_sibling_fence_above(&[10, 101, 102, 103], &siblings, 102));
        assert!(has_sibling_fence_above(&[10, 101, 102, 103], &siblings, 103));
        assert!(!has_sibling_fence_above(&[10, 999, 102], &siblings, 102));
    }

    #[test]
    fn evaluate_z_chain_plan_promotes_preferred_fence_to_top() {
        let apps: HashSet<isize> = [10].into_iter().collect();
        let fences = [101, 102, 103];
        let (ok, chain) = evaluate_z_chain_plan_with_preferred(
            &[10, 101, 102, 103, 200],
            &apps,
            &fences,
            Some(200),
            true,
            Some(102),
        );
        assert!(!ok);
        assert_eq!(chain, vec![102, 101, 103, 200]);

        let (ok2, chain2) = evaluate_z_chain_plan_with_preferred(
            &[10, 102, 101, 103, 200],
            &apps,
            &fences,
            Some(200),
            true,
            Some(102),
        );
        assert!(ok2);
        assert_eq!(chain2, vec![102, 101, 103, 200]);
    }
}
