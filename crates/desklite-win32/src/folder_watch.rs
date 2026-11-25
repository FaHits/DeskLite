use std::path::Path;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::HSTRING;

pub const WM_APP_PORTAL_DIRTY: u32 = WM_APP + 2;

struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

pub struct FolderWatcher {
    stop_event: HANDLE,
    join: Option<JoinHandle<()>>,
}

impl FolderWatcher {
    pub fn start(fence_id: i32, dir: &Path, notify_hwnd: HWND) -> Option<Self> {
        let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME;
        let change_handle = unsafe { FindFirstChangeNotificationW(&wdir, false, filter) }.ok()?;
        let stop_event = unsafe { CreateEventW(None, true, false, None) }.ok()?;

        let ch = SendHandle(change_handle);
        let se = SendHandle(stop_event);
        let hw = SendHwnd(notify_hwnd);

        let join = thread::spawn(move || {
            let ch = ch;
            let se = se;
            let hw = hw;
            let handles = [ch.0, se.0];
            loop {
                let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                if wait == WAIT_OBJECT_0 {
                    unsafe {
                        let _ = PostMessageW(
                            Some(hw.0),
                            WM_APP_PORTAL_DIRTY,
                            WPARAM(fence_id as usize),
                            LPARAM(0),
                        );
                        if FindNextChangeNotification(ch.0).is_err() {
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            unsafe {
                let _ = FindCloseChangeNotification(ch.0);
            }
        });

        Some(Self {
            stop_event,
            join: Some(join),
        })
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop_event);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop_event);
        }
    }
}
