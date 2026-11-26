use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::HSTRING;

pub const WM_APP_PORTAL_DIRTY: u32 = WM_APP + 2;

struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

struct SendHwnd(HWND);
unsafe impl Send for SendHwnd {}

struct WatchedEntry {
    dir: PathBuf,
    handle: Option<HANDLE>,
}

impl WatchedEntry {
    fn open(dir: &Path) -> Option<HANDLE> {
        let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME;
        unsafe { FindFirstChangeNotificationW(&wdir, false, filter) }.ok()
    }
}

pub struct FolderWatcher {
    stop_event: HANDLE,
    join: Option<JoinHandle<()>>,
}

impl FolderWatcher {
    pub fn start(fence_id: i32, dir: &Path, notify_hwnd: HWND) -> Option<Self> {
        Self::start_multi(fence_id, &[dir.to_path_buf()], notify_hwnd)
    }

    pub fn start_desktop(fence_id: i32, notify_hwnd: HWND) -> Option<Self> {
        let dirs = crate::shell_ops::all_desktop_dirs();
        Self::start_multi(fence_id, &dirs, notify_hwnd)
    }

    pub fn start_multi(fence_id: i32, dirs: &[PathBuf], notify_hwnd: HWND) -> Option<Self> {
        if dirs.is_empty() {
            return None;
        }
        let mut entries = Vec::with_capacity(dirs.len().min(63));
        for dir in dirs.iter().take(63) {
            if let Some(h) = WatchedEntry::open(dir) {
                entries.push(WatchedEntry {
                    dir: dir.clone(),
                    handle: Some(h),
                });
            }
        }
        if entries.is_empty() {
            return None;
        }
        let Ok(stop_event) = (unsafe { CreateEventW(None, true, false, None) }) else {
            for e in entries {
                if let Some(h) = e.handle {
                    unsafe {
                        let _ = FindCloseChangeNotification(h);
                    }
                }
            }
            return None;
        };

        let se = SendHandle(stop_event);
        let hw = SendHwnd(notify_hwnd);
        let initial: Vec<(PathBuf, SendHandle)> = entries
            .into_iter()
            .filter_map(|e| e.handle.map(|h| (e.dir, SendHandle(h))))
            .collect();

        let join = thread::Builder::new()
            .name(format!("desklite-watch-{fence_id}"))
            .spawn(move || {
                let se = se;
                let hw = hw;
                let mut watched: Vec<WatchedEntry> = initial
                    .into_iter()
                    .map(|(dir, sh)| WatchedEntry {
                        dir,
                        handle: Some(sh.0),
                    })
                    .collect();
                let mut backoff_ms: u32 = 250;

                loop {
                    // Attempt reconnect for any closed watcher slots.
                    let mut reconnected = false;
                    for slot in &mut watched {
                        if slot.handle.is_none()
                            && let Some(h) = WatchedEntry::open(&slot.dir)
                        {
                            slot.handle = Some(h);
                            reconnected = true;
                        }
                    }
                    if reconnected {
                        backoff_ms = 250;
                        unsafe {
                            let _ = PostMessageW(
                                Some(hw.0),
                                WM_APP_PORTAL_DIRTY,
                                WPARAM(fence_id as usize),
                                LPARAM(0),
                            );
                        }
                    }

                    let mut handles: Vec<HANDLE> = Vec::with_capacity(1 + watched.len());
                    let mut slot_indices: Vec<usize> = Vec::with_capacity(watched.len());
                    handles.push(se.0);
                    for (idx, slot) in watched.iter().enumerate() {
                        if let Some(h) = slot.handle {
                            handles.push(h);
                            slot_indices.push(idx);
                        }
                    }

                    if slot_indices.is_empty() {
                        let stop_wait = unsafe { WaitForSingleObject(se.0, backoff_ms) };
                        if stop_wait != WAIT_TIMEOUT {
                            break;
                        }
                        backoff_ms = (backoff_ms * 2).min(4000);
                        continue;
                    }

                    let has_missing = watched.iter().any(|s| s.handle.is_none());
                    let timeout_ms = if has_missing { backoff_ms } else { INFINITE };
                    let wait = unsafe { WaitForMultipleObjects(&handles, false, timeout_ms) };

                    if wait == WAIT_OBJECT_0 {
                        break;
                    }
                    if wait == WAIT_TIMEOUT {
                        backoff_ms = (backoff_ms * 2).min(4000);
                        continue;
                    }
                    if wait == WAIT_FAILED {
                        for slot in &mut watched {
                            if let Some(h) = slot.handle.take() {
                                unsafe {
                                    let _ = FindCloseChangeNotification(h);
                                }
                            }
                        }
                        unsafe {
                            let _ = PostMessageW(
                                Some(hw.0),
                                WM_APP_PORTAL_DIRTY,
                                WPARAM(fence_id as usize),
                                LPARAM(1),
                            );
                        }
                        let stop_wait = unsafe { WaitForSingleObject(se.0, backoff_ms) };
                        if stop_wait != WAIT_TIMEOUT {
                            break;
                        }
                        backoff_ms = (backoff_ms * 2).min(4000);
                        continue;
                    }

                    let fired_pos = (wait.0 - WAIT_OBJECT_0.0) as usize;
                    if fired_pos == 0 || fired_pos > slot_indices.len() {
                        break;
                    }
                    let dir_idx = slot_indices[fired_pos - 1];
                    unsafe {
                        let _ = PostMessageW(
                            Some(hw.0),
                            WM_APP_PORTAL_DIRTY,
                            WPARAM(fence_id as usize),
                            LPARAM(0),
                        );
                        if let Some(ch) = watched[dir_idx].handle
                            && FindNextChangeNotification(ch).is_err()
                        {
                            let _ = FindCloseChangeNotification(ch);
                            watched[dir_idx].handle = None;
                            let _ = PostMessageW(
                                Some(hw.0),
                                WM_APP_PORTAL_DIRTY,
                                WPARAM(fence_id as usize),
                                LPARAM(1),
                            );
                        }
                    }
                }

                for slot in &mut watched {
                    if let Some(h) = slot.handle.take() {
                        unsafe {
                            let _ = FindCloseChangeNotification(h);
                        }
                    }
                }
            })
            .ok()?;

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
