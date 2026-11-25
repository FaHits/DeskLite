use std::mem::size_of;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_CLICK, BS_DEFPUSHBUTTON, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    ES_AUTOHSCROLL, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDOK, MSG, PostQuitMessage, RegisterClassExW, SW_SHOW, SendMessageW,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, w};

const IDC_EDIT: usize = 1001;

struct PromptState {
    edit: HWND,
    accepted: Option<String>,
    done: bool,
}

unsafe extern "system" fn prompt_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA;

    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        return LRESULT(0);
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut PromptState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let st = unsafe { &mut *ptr };

    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            if id == IDOK.0 {
                let len = unsafe { GetWindowTextLengthW(st.edit) }.max(0) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = unsafe { GetWindowTextW(st.edit, &mut buf) }.max(0) as usize;
                st.accepted = Some(String::from_utf16_lossy(&buf[..got]).trim().to_string());
                st.done = true;
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            } else if id == 2 {
                st.done = true;
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
        }
        WM_CLOSE => {
            st.done = true;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            st.done = true;
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Shows a lightweight native Win32 input prompt for renaming or creating a fence.
pub fn prompt_fence_name(owner: HWND, title: &str, initial: &str) -> Option<String> {
    let hinst = unsafe { GetModuleHandleW(None) }.ok()?;
    let cls = w!("DeskLitePromptDlg");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(prompt_wnd_proc),
        hInstance: hinst.into(),
        lpszClassName: cls,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassExW(&wc) };

    let mut owner_rc = RECT::default();
    let _ = unsafe { GetWindowRect(owner, &mut owner_rc) };
    let dlg_w = 340;
    let dlg_h = 136;
    let x = if owner_rc.right > owner_rc.left {
        (owner_rc.left + owner_rc.right - dlg_w) / 2
    } else {
        320
    };
    let y = if owner_rc.bottom > owner_rc.top {
        (owner_rc.top + owner_rc.bottom - dlg_h) / 2
    } else {
        260
    };

    let mut state = PromptState {
        edit: HWND::default(),
        accepted: None,
        done: false,
    };

    let title_w = HSTRING::from(title);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            cls,
            &title_w,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            dlg_w,
            dlg_h,
            Some(owner),
            None,
            Some(hinst.into()),
            Some((&raw mut state).cast()),
        )
    }
    .ok()?;

    let init_w = HSTRING::from(initial);
    let edit = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("EDIT"),
            &init_w,
            WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            16,
            16,
            292,
            26,
            Some(hwnd),
            Some(HMENU(IDC_EDIT as *mut _)),
            Some(hinst.into()),
            None,
        )
    }
    .ok()?;
    state.edit = edit;

    let ok_btn = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("确定"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            146,
            56,
            76,
            28,
            Some(hwnd),
            Some(HMENU(IDOK.0 as isize as *mut _)),
            Some(hinst.into()),
            None,
        )
    }
    .ok()?;

    let _cancel_btn = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("取消"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            232,
            56,
            76,
            28,
            Some(hwnd),
            Some(HMENU(2usize as *mut _)),
            Some(hinst.into()),
            None,
        )
    }
    .ok()?;

    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        SendMessageW(ok_btn, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetFocus(Some(edit));
    }

    let mut msg = MSG::default();
    while !state.done && unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.message == WM_KEYDOWN {
            if msg.wParam.0 == usize::from(VK_RETURN.0) {
                unsafe {
                    SendMessageW(ok_btn, BM_CLICK, None, None);
                }
                continue;
            } else if msg.wParam.0 == usize::from(VK_ESCAPE.0) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                continue;
            }
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        let _ = EnableWindow(owner, true);
    }

    state.accepted.filter(|s| !s.is_empty())
}
