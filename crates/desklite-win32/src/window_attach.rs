use std::collections::HashMap;
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, FindWindowW, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT, GWLP_WNDPROC,
    GetWindowLongPtrW, HWND_BOTTOM, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WINDOWPOS, WM_NCDESTROY, WM_WINDOWPOSCHANGING,
    WNDPROC, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_POPUP, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING};

use crate::desktop_listview::probe_desktop_window_tree;

static ORIG_WNDPROCS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

fn orig_procs() -> &'static Mutex<HashMap<isize, isize>> {
    ORIG_WNDPROCS.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe extern "system" fn bottom_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_WINDOWPOSCHANGING {
        let wp = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if (wp.flags & SWP_NOZORDER).0 == 0 {
            wp.hwndInsertAfter = HWND_BOTTOM;
        }
    }

    let orig = orig_procs().lock().unwrap().get(&(hwnd.0 as isize)).copied();
    if msg == WM_NCDESTROY {
        orig_procs().lock().unwrap().remove(&(hwnd.0 as isize));
    }
    if let Some(proc_ptr) = orig {
        let wndproc: WNDPROC = unsafe { std::mem::transmute(proc_ptr) };
        unsafe { CallWindowProcW(wndproc, hwnd, msg, wparam, lparam) }
    } else {
        LRESULT(0)
    }
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
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0
    }
}

/// Configures a DeskLite fence `HWND`:
/// - Strips `WS_CAPTION | WS_SYSMENU` so DWM never draws phantom `[x]` close glows.
/// - Strips `WS_EX_APPWINDOW` and adds `WS_EX_TOOLWINDOW` so fences never clutter `Alt+Tab`.
/// - Attaches desktop owner (`WorkerW` / `Progman`) so `Win+D` keeps fences visible.
/// - Extends DWM frame + applies rounded corners & Acrylic backdrop (`DWMSBT_TRANSIENTWINDOW`).
/// - Subclasses `WM_WINDOWPOSCHANGING` to stay bottom-most without blocking `StartDrag` or focus.
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

        if let Some(tree) = probe_desktop_window_tree() {
            let owner = tree.worker_w.unwrap_or(tree.progman);
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize);
        }

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
                e.insert(prev);
            }
        }
        drop(map);

        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}
