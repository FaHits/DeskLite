use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW, WM_APP};
use windows::core::PCWSTR;

use crate::{Win32Error, win_err};

pub const WM_APP_TRAY: u32 = WM_APP + 1;

pub struct TrayIcon {
    hwnd: HWND,
    id: u32,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, id: u32, tooltip: &str) -> Result<Self, Win32Error> {
        let embedded = unsafe { GetModuleHandleW(None) }.ok().and_then(|hmod| unsafe {
            LoadIconW(Some(hmod.into()), PCWSTR(std::ptr::without_provenance(1))).ok()
        });
        let icon = match embedded {
            Some(h) => h,
            None => unsafe { LoadIconW(None, IDI_APPLICATION) }
                .map_err(|e| win_err("LoadIconW", e))?,
        };
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: id,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: icon,
            ..Default::default()
        };
        let utf16: Vec<u16> = tooltip.encode_utf16().collect();
        let copy_len = utf16.len().min(nid.szTip.len().saturating_sub(1));
        nid.szTip[..copy_len].copy_from_slice(&utf16[..copy_len]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
        if !ok.as_bool() {
            return Err(Win32Error::Status {
                op: "Shell_NotifyIconW(NIM_ADD)",
                code: 0,
            });
        }
        Ok(Self { hwnd, id })
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: self.id,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }
}
