use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Mutex, MutexGuard, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_CLOAKED, DWMWA_COLOR_NONE,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, RDW_INTERNALPAINT,
    RDW_INVALIDATE, RedrawWindow,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, VK_CONTROL, VK_LBUTTON,
    VK_MENU, VK_PROCESSKEY, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CallWindowProcW, EVENT_OBJECT_REORDER, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, FindWindowW, GW_HWNDNEXT, GWL_EXSTYLE,
    GWL_STYLE, GWLP_HWNDPARENT, GWLP_WNDPROC, GetClassNameW, GetCursorPos, GetDesktopWindow,
    GetForegroundWindow, GetTopWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    HWND_BOTTOM, HWND_TOP, IDC_ARROW, IsIconic, IsWindow, IsWindowVisible, LoadCursorW,
    PostMessageW, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, STYLESTRUCT, SW_HIDE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetCursor, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOWPOS, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CANCELMODE, WM_CAPTURECHANGED, WM_ENTERSIZEMOVE,
    WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOVING, WM_NCACTIVATE,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCPAINT, WM_RBUTTONDOWN, WM_SETCURSOR,
    WM_SIZING, WM_STYLECHANGING, WM_SYSCOMMAND, WM_WINDOWPOSCHANGING, WNDPROC, WS_BORDER,
    WS_CAPTION, WS_DLGFRAME, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_WINDOWEDGE, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZE,
    WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING, PCSTR, w};

use crate::desktop_listview::probe_desktop_window_tree;

const WM_APP_ENSURE_DESKTOP_Z: u32 = WM_APP + 99;
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
static LAST_SELF_Z_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static EXTERNAL_FG_NOTIFY_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static EXTERNAL_FG_NOTIFY_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static START_MONO: OnceLock<std::time::Instant> = OnceLock::new();

pub fn set_external_foreground_notify_target(hwnd: isize, msg: u32) {
    EXTERNAL_FG_NOTIFY_MSG.store(msg, std::sync::atomic::Ordering::Release);
    EXTERNAL_FG_NOTIFY_HWND.store(hwnd, std::sync::atomic::Ordering::Release);
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

fn mono_ms() -> u64 {
    START_MONO
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
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
        unsafe {
            let _ = SetForegroundWindow(HWND(raw_hwnd as *mut _));
        }
    }
}

pub fn allow_foreground_self() {
    unsafe {
        let _ = AllowSetForegroundWindow(GetCurrentProcessId());
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

/// Immediately hides all visible DeskLite bucket fences and inspector windows before
/// tearing down OpenGL/DWM resources so exit never flashes opaque white rectangles.
pub fn hide_all_fences_immediately() {
    for raw in all_bucket_fence_hwnds() {
        let hwnd = HWND(raw as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
    let insp = inspector_hwnd();
    if insp != 0 {
        let hwnd = HWND(insp as *mut _);
        if unsafe { IsWindow(Some(hwnd)).as_bool() } {
            unsafe {
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

/// Walks top-down from `GetTopWindow(None)` down to `owner_hwnd` and returns the
/// `hwndInsertAfter` anchor preserving strict layering:
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
    let owner = HWND(live_owner as *mut _);
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

    for _ in 0..2048 {
        if cur == owner {
            break;
        }
        let raw = cur.0 as isize;
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
    let owner = HWND(live_owner as *mut _);
    let free_raw = free_desktop_hwnd();
    let self_raw = hwnd.0 as isize;
    let is_free = free_raw != 0 && self_raw == free_raw;
    let free_visible =
        free_raw != 0 && unsafe { IsWindowVisible(HWND(free_raw as *mut _)).as_bool() };

    let mut cur = hwnd;
    let mut saw_owner = false;
    let mut saw_free_below = false;
    for _ in 0..2048 {
        let Ok(next) = (unsafe { GetWindow(cur, GW_HWNDNEXT) }) else {
            return false;
        };
        if next.is_invalid() {
            return false;
        }
        if next == owner {
            saw_owner = true;
            break;
        }
        let next_raw = next.0 as isize;
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
    if event == EVENT_OBJECT_REORDER {
        if hwnd.is_invalid() {
            return;
        }
        let is_desktop_root = hwnd == unsafe { GetDesktopWindow() };
        if !is_desktop_root && !unsafe { is_desktop_shell_class(hwnd) } {
            return;
        }
        if is_in_size_move() {
            DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
            return;
        }
        let now = mono_ms();
        let last = LAST_SELF_Z_MS.load(std::sync::atomic::Ordering::Acquire);
        if is_desktop_root && now.saturating_sub(last) < 60 {
            return;
        }
    } else if is_in_size_move() {
        DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
        return;
    }
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

/// Asynchronously asks all attached DeskLite fence windows to verify their Z-order
/// right above `WorkerW`/`Progman` and below all visible app windows (safe from background threads).
pub fn post_reassert_all_fences_z() {
    if is_in_size_move() {
        DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
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
        unsafe {
            if GetForegroundWindow() != hwnd {
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
    if msg == WM_KEYDOWN
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
        if is_in_size_move() {
            DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
            return LRESULT(0);
        }
        let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
        if live_owner != 0 {
            if live_owner != owner_hwnd {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, live_owner) };
                if let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&(hwnd.0 as isize)) {
                    e.owner_hwnd = live_owner;
                }
            }
            let fence_hwnds = snapshot_fence_hwnd_set();
            let ok = unsafe { is_strictly_in_desktop_layer(hwnd, live_owner, &fence_hwnds) };
            if !ok {
                let anchor = unsafe { find_desktop_layer_anchor(hwnd, live_owner, &fence_hwnds) };
                if anchor != hwnd {
                    LAST_SELF_Z_MS.store(mono_ms(), std::sync::atomic::Ordering::Release);
                    let _ = unsafe {
                        SetWindowPos(
                            hwnd,
                            Some(anchor),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                    LAST_SELF_Z_MS.store(mono_ms(), std::sync::atomic::Ordering::Release);
                }
            }
        }
        return LRESULT(0);
    }

    if msg == WM_WINDOWPOSCHANGING {
        let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if (wp.flags & SWP_NOZORDER).0 == 0 && owner_hwnd != 0 {
            if is_in_size_move() {
                wp.flags |= SWP_NOZORDER;
                DEFERRED_Z_AFTER_SIZEMOVE.store(true, std::sync::atomic::Ordering::Release);
            } else {
                let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
                if live_owner != 0 && live_owner != owner_hwnd {
                    if let Some(e) = lock_unpoisoned(orig_procs()).get_mut(&(hwnd.0 as isize)) {
                        e.owner_hwnd = live_owner;
                    }
                    unsafe {
                        let _ = PostMessageW(
                            Some(hwnd),
                            WM_APP_ENSURE_DESKTOP_Z,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
                let fence_hwnds = snapshot_fence_hwnd_set();
                let already_ok =
                    unsafe { is_strictly_in_desktop_layer(hwnd, live_owner, &fence_hwnds) };
                if already_ok
                    && (wp.hwndInsertAfter.0.is_null() || wp.hwndInsertAfter == HWND_BOTTOM)
                {
                    wp.flags |= SWP_NOZORDER;
                } else {
                    let anchor =
                        unsafe { find_desktop_layer_anchor(hwnd, live_owner, &fence_hwnds) };
                    if anchor != hwnd {
                        wp.hwndInsertAfter = anchor;
                    }
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

/// Configures a DeskLite fence `HWND`:
/// - Strips `WS_CAPTION | WS_SYSMENU` so DWM never draws phantom `[x]` close glows.
/// - Strips `WS_EX_APPWINDOW` and adds `WS_EX_TOOLWINDOW` so fences never clutter `Alt+Tab`.
/// - Disables DWM border & transition animations (`DWMWA_COLOR_NONE`, `DWMWA_TRANSITIONS_FORCEDISABLED`)
///   so focus switches never flash a white border and `Win+D` stays flicker-free.
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
