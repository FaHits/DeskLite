use std::collections::HashMap;
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_CLOAKED, DWMWA_COLOR_NONE,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, EVENT_OBJECT_REORDER, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, FindWindowW, GW_HWNDNEXT, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT,
    GWLP_WNDPROC, GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetTopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, HWND_BOTTOM, HWND_TOP, IsIconic, IsWindow, IsWindowVisible,
    PostMessageW, SC_MINIMIZE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WINDOWPOS, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_ERASEBKGND, WM_NCACTIVATE, WM_NCDESTROY, WM_SYSCOMMAND,
    WM_WINDOWPOSCHANGING, WNDPROC, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING};

use crate::desktop_listview::probe_desktop_window_tree;

const WM_APP_ENSURE_DESKTOP_Z: u32 = WM_APP + 99;

#[derive(Clone, Copy)]
struct SubclassEntry {
    orig_proc: isize,
    owner_hwnd: isize,
}

static ORIG_WNDPROCS: OnceLock<Mutex<HashMap<isize, SubclassEntry>>> = OnceLock::new();

fn orig_procs() -> &'static Mutex<HashMap<isize, SubclassEntry>> {
    ORIG_WNDPROCS.get_or_init(|| Mutex::new(HashMap::new()))
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
    if owner_hwnd != 0 && unsafe { IsWindow(Some(HWND(owner_hwnd as *mut _))).as_bool() } {
        return owner_hwnd;
    }
    probe_desktop_window_tree()
        .map(|t| t.worker_w.unwrap_or(t.progman).0 as isize)
        .unwrap_or(0)
}

/// Walks top-down from `GetTopWindow(None)` down to `owner_hwnd` and returns the
/// `hwndInsertAfter` anchor right below every visible non-topmost app window (and
/// right above `owner_hwnd`), or after the last topmost window if all apps are minimized
/// (e.g. during `Win+D`).
unsafe fn find_desktop_layer_anchor(
    self_hwnd: HWND,
    owner_hwnd: isize,
    map: &HashMap<isize, SubclassEntry>,
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

    let mut last_topmost: Option<HWND> = None;
    let mut last_visible_app: Option<HWND> = None;

    for _ in 0..2048 {
        if cur == owner {
            break;
        }
        let raw = cur.0 as isize;
        let is_ours = cur == self_hwnd || map.contains_key(&raw);
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

    last_visible_app
        .or(last_topmost)
        .unwrap_or(HWND_TOP)
}

unsafe fn is_strictly_in_desktop_layer(
    hwnd: HWND,
    owner_hwnd: isize,
    map: &HashMap<isize, SubclassEntry>,
) -> bool {
    let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
    if live_owner == 0 {
        return true;
    }
    let owner = HWND(live_owner as *mut _);
    let mut cur = hwnd;
    let mut saw_owner = false;
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
        if !map.contains_key(&(next.0 as isize))
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
    saw_owner
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
    let hwnds: Vec<isize> = orig_procs().lock().unwrap().keys().copied().collect();
    for raw in hwnds {
        if raw != 0 {
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(raw as *mut _)),
                    WM_APP_ENSURE_DESKTOP_Z,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
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
    if msg == WM_SYSCOMMAND && ((wparam.0 as u32) & 0xFFF0) == SC_MINIMIZE {
        return LRESULT(0);
    }

    let (entry, owner_hwnd) = {
        let map = orig_procs().lock().unwrap();
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
        let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
        if live_owner != 0 {
            if live_owner != owner_hwnd {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, live_owner) };
                if let Some(e) = orig_procs().lock().unwrap().get_mut(&(hwnd.0 as isize)) {
                    e.owner_hwnd = live_owner;
                }
            }
            let map = orig_procs().lock().unwrap();
            let ok = unsafe { is_strictly_in_desktop_layer(hwnd, live_owner, &map) };
            if !ok {
                let anchor = unsafe { find_desktop_layer_anchor(hwnd, live_owner, &map) };
                drop(map);
                if anchor != hwnd {
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
                }
            }
        }
        return LRESULT(0);
    }

    if msg == WM_WINDOWPOSCHANGING {
        let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if (wp.flags & SWP_NOZORDER).0 == 0 && owner_hwnd != 0 {
            let live_owner = unsafe { resolve_live_owner(owner_hwnd) };
            let map = orig_procs().lock().unwrap();
            let already_ok = unsafe { is_strictly_in_desktop_layer(hwnd, live_owner, &map) };
            let anchor = unsafe { find_desktop_layer_anchor(hwnd, live_owner, &map) };
            drop(map);

            if already_ok && (wp.hwndInsertAfter.0.is_null() || wp.hwndInsertAfter == HWND_BOTTOM) {
                wp.flags |= SWP_NOZORDER;
            } else {
                wp.hwndInsertAfter = anchor;
            }
        }
    }

    if msg == WM_NCDESTROY {
        orig_procs().lock().unwrap().remove(&(hwnd.0 as isize));
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

/// Configures a DeskLite fence `HWND`:
/// - Strips `WS_CAPTION | WS_SYSMENU` so DWM never draws phantom `[x]` close glows.
/// - Strips `WS_EX_APPWINDOW` and adds `WS_EX_TOOLWINDOW` so fences never clutter `Alt+Tab`.
/// - Disables DWM border & transition animations (`DWMWA_COLOR_NONE`, `DWMWA_TRANSITIONS_FORCEDISABLED`)
///   so focus switches never flash a white border and `Win+D` stays flicker-free.
/// - Attaches desktop owner (`WorkerW` / `Progman`) and subclasses Z-order to stay below all
///   visible app windows yet strictly above `WorkerW`.
pub fn attach_desktop_owner_and_rounded_corners(raw_hwnd: isize) {
    if raw_hwnd == 0 {
        return;
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
        }

        let owner_hwnd = if let Some(tree) = probe_desktop_window_tree() {
            let owner = tree.worker_w.unwrap_or(tree.progman);
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize);
            owner.0 as isize
        } else {
            0
        };

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

        let mut map = orig_procs().lock().unwrap();
        if let std::collections::hash_map::Entry::Vacant(e) = map.entry(raw_hwnd) {
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, bottom_subclass_proc as *const () as isize);
            if prev != 0 {
                e.insert(SubclassEntry {
                    orig_proc: prev,
                    owner_hwnd,
                });
            }
        }
        let insert_after = find_desktop_layer_anchor(hwnd, owner_hwnd, &map);
        drop(map);

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
