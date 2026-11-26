use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use desklite_core::Point;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{
    LVIF_TEXT, LVITEMW, LVM_DELETEITEM, LVM_GETITEMCOUNT, LVM_GETITEMPOSITION, LVM_GETITEMTEXTW,
    LVM_GETORIGIN, LVM_SETITEMPOSITION32,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHCNE_CREATE, SHCNE_DELETE, SHCNE_MKDIR, SHCNE_RMDIR, SHCNE_UPDATEDIR, SHCNF_FLUSH,
    SHCNF_FLUSHNOWAIT, SHCNF_PATHW, SHChangeNotify,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, FindWindowW, GA_ROOT, GetAncestor, GetClassNameW,
    GetDesktopWindow, GetWindowThreadProcessId, SendMessageW, WM_COMMAND,
};
use windows::core::{BOOL, HSTRING, w};

use crate::shell_ops::{all_desktop_dirs, is_directly_on_desktop, shell_display_name};
use crate::{Win32Error, win_err};

#[derive(Debug, Clone)]
pub struct DesktopWindowTree {
    pub progman: HWND,
    pub worker_w: Option<HWND>,
    pub shell_def_view: HWND,
    pub list_view: HWND,
}

#[derive(Debug, Clone)]
pub struct DesktopIconInfo {
    pub index: usize,
    pub name: String,
    pub pos: Point,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct RemoteBuffer {
    process: HANDLE,
    ptr: *mut c_void,
}

impl RemoteBuffer {
    fn alloc(process: HANDLE, size: usize) -> Result<Self, Win32Error> {
        let ptr = unsafe {
            VirtualAllocEx(
                process,
                None,
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if ptr.is_null() {
            return Err(Win32Error::RemoteAllocFailed);
        }
        Ok(Self { process, ptr })
    }
}

impl Drop for RemoteBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let _ = VirtualFreeEx(self.process, self.ptr, 0, MEM_RELEASE);
            }
        }
    }
}

/// Locates `Progman`, optional `WorkerW`, `SHELLDLL_DefView`, and `SysListView32`
/// across both Windows 11 23H2 (`WorkerW` split) and 24H2 (`Progman` direct child) layouts.
pub fn probe_desktop_window_tree() -> Option<DesktopWindowTree> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), None).ok()?;
        if progman.is_invalid() {
            return None;
        }

        // Case 1: SHELLDLL_DefView directly under Progman (classic or Win11 24H2)
        if let Ok(def_view) = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None)
            && !def_view.is_invalid()
            && let Ok(lv) = FindWindowExW(Some(def_view), None, w!("SysListView32"), None)
            && !lv.is_invalid()
        {
            return Some(DesktopWindowTree {
                progman,
                worker_w: None,
                shell_def_view: def_view,
                list_view: lv,
            });
        }

        // Case 2: SHELLDLL_DefView under top-level WorkerW (Win10/Win11 23H2 with wallpaper worker)
        let desktop = GetDesktopWindow();
        let mut worker = HWND::default();
        loop {
            match FindWindowExW(Some(desktop), Some(worker), w!("WorkerW"), None) {
                Ok(w) if !w.is_invalid() => {
                    worker = w;
                    if let Ok(def_view) = FindWindowExW(Some(worker), None, w!("SHELLDLL_DefView"), None)
                        && !def_view.is_invalid()
                        && let Ok(lv) = FindWindowExW(Some(def_view), None, w!("SysListView32"), None)
                        && !lv.is_invalid()
                    {
                        return Some(DesktopWindowTree {
                            progman,
                            worker_w: Some(worker),
                            shell_def_view: def_view,
                            list_view: lv,
                        });
                    }
                }
                _ => break,
            }
        }
        // Case 3: Fallback scan via EnumWindows/EnumChildWindows (handles DeskGo/custom shells
        // that reparent SHELLDLL_DefView under TXMiniSkin or nested WorkerW).
        let mut found: Option<DesktopWindowTree> = None;
        unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let slot = unsafe { &mut *(lparam.0 as *mut Option<DesktopWindowTree>) };
            if slot.is_some() {
                return BOOL(0);
            }
            let mut cls = [0u16; 64];
            let len = unsafe { GetClassNameW(hwnd, &mut cls) } as usize;
            if String::from_utf16_lossy(&cls[..len]) == "SHELLDLL_DefView"
                && let Ok(lv) = unsafe { FindWindowExW(Some(hwnd), None, w!("SysListView32"), None) }
                && !lv.is_invalid()
            {
                let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
                let progman = unsafe { FindWindowW(w!("Progman"), None) }.unwrap_or(root);
                *slot = Some(DesktopWindowTree {
                    progman,
                    worker_w: if root != progman { Some(root) } else { None },
                    shell_def_view: hwnd,
                    list_view: lv,
                });
                return BOOL(0);
            }
            TRUE
        }
        unsafe extern "system" fn enum_top(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let slot = unsafe { &*(lparam.0 as *const Option<DesktopWindowTree>) };
            if slot.is_some() {
                return BOOL(0);
            }
            let _ = unsafe { EnumChildWindows(Some(hwnd), Some(enum_child), lparam) };
            let slot = unsafe { &*(lparam.0 as *const Option<DesktopWindowTree>) };
            if slot.is_some() { BOOL(0) } else { TRUE }
        }
        let _ = EnumWindows(Some(enum_top), LPARAM(&raw mut found as isize));
        found
    }
}

pub fn find_desktop_listview() -> Result<DesktopListView, Win32Error> {
    let tree = probe_desktop_window_tree().ok_or(Win32Error::DesktopListViewNotFound)?;
    DesktopListView::from_tree(tree)
}

pub struct DesktopListView {
    tree: DesktopWindowTree,
    explorer_pid: u32,
}

impl DesktopListView {
    pub fn from_tree(tree: DesktopWindowTree) -> Result<Self, Win32Error> {
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(tree.list_view, Some(&mut pid));
        }
        if pid == 0 {
            return Err(Win32Error::DesktopListViewNotFound);
        }
        Ok(Self {
            tree,
            explorer_pid: pid,
        })
    }

    pub fn tree(&self) -> &DesktopWindowTree {
        &self.tree
    }

    pub fn explorer_pid(&self) -> u32 {
        self.explorer_pid
    }

    pub fn listview_class_name(&self) -> String {
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(self.tree.list_view, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..len])
    }

    pub fn icon_count(&self) -> usize {
        let count = unsafe { SendMessageW(self.tree.list_view, LVM_GETITEMCOUNT, None, None).0 };
        count.max(0) as usize
    }

    fn open_explorer(&self) -> Result<OwnedHandle, Win32Error> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_VM_OPERATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_QUERY_INFORMATION,
                false,
                self.explorer_pid,
            )
        }
        .map_err(|e| win_err("OpenProcess(explorer.exe)", e))?;
        Ok(OwnedHandle(handle))
    }

    pub fn enumerate_icons(&self) -> Result<Vec<DesktopIconInfo>, Win32Error> {
        let count = self.icon_count();
        if count == 0 {
            return Ok(Vec::new());
        }

        const MAX_CHARS: usize = 260;
        let process = self.open_explorer()?;
        let pt_buf = RemoteBuffer::alloc(process.0, size_of::<POINT>())?;
        let item_buf = RemoteBuffer::alloc(process.0, size_of::<LVITEMW>())?;
        let text_buf = RemoteBuffer::alloc(process.0, MAX_CHARS * size_of::<u16>())?;

        let mut icons = Vec::with_capacity(count);
        for index in 0..count {
            unsafe {
                SendMessageW(
                    self.tree.list_view,
                    LVM_GETITEMPOSITION,
                    Some(WPARAM(index)),
                    Some(LPARAM(pt_buf.ptr as isize)),
                );
            }
            let mut pt = POINT::default();
            unsafe {
                ReadProcessMemory(
                    process.0,
                    pt_buf.ptr,
                    (&raw mut pt).cast(),
                    size_of::<POINT>(),
                    None,
                )
            }
            .map_err(|e| win_err("ReadProcessMemory(POINT)", e))?;

            let lvi = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                iSubItem: 0,
                pszText: windows::core::PWSTR(text_buf.ptr.cast()),
                cchTextMax: MAX_CHARS as i32,
                ..Default::default()
            };
            unsafe {
                WriteProcessMemory(
                    process.0,
                    item_buf.ptr,
                    (&raw const lvi).cast(),
                    size_of::<LVITEMW>(),
                    None,
                )
            }
            .map_err(|e| win_err("WriteProcessMemory(LVITEMW)", e))?;

            let chars = unsafe {
                SendMessageW(
                    self.tree.list_view,
                    LVM_GETITEMTEXTW,
                    Some(WPARAM(index)),
                    Some(LPARAM(item_buf.ptr as isize)),
                )
                .0
            }
            .max(0) as usize;

            let mut utf16 = vec![0u16; chars.min(MAX_CHARS)];
            if !utf16.is_empty() {
                unsafe {
                    ReadProcessMemory(
                        process.0,
                        text_buf.ptr,
                        utf16.as_mut_ptr().cast(),
                        utf16.len() * size_of::<u16>(),
                        None,
                    )
                }
                .map_err(|e| win_err("ReadProcessMemory(LVITEMW text)", e))?;
            }
            let end = utf16.iter().position(|&c| c == 0).unwrap_or(utf16.len());
            let name = String::from_utf16_lossy(&utf16[..end]);

            icons.push(DesktopIconInfo {
                index,
                name,
                pos: Point { x: pt.x, y: pt.y },
            });
        }

        Ok(icons)
    }

    pub fn get_icon_position(&self, index: usize) -> Result<Point, Win32Error> {
        let count = self.icon_count();
        if index >= count {
            return Err(Win32Error::IconIndexOutOfRange(index, count));
        }
        let process = self.open_explorer()?;
        let pt_buf = RemoteBuffer::alloc(process.0, size_of::<POINT>())?;
        unsafe {
            SendMessageW(
                self.tree.list_view,
                LVM_GETITEMPOSITION,
                Some(WPARAM(index)),
                Some(LPARAM(pt_buf.ptr as isize)),
            );
        }
        let mut pt = POINT::default();
        unsafe {
            ReadProcessMemory(
                process.0,
                pt_buf.ptr,
                (&raw mut pt).cast(),
                size_of::<POINT>(),
                None,
            )
        }
        .map_err(|e| win_err("ReadProcessMemory(POINT)", e))?;
        Ok(Point { x: pt.x, y: pt.y })
    }

    pub fn set_icon_position(&self, index: usize, pos: Point) -> Result<(), Win32Error> {
        let count = self.icon_count();
        if index >= count {
            return Err(Win32Error::IconIndexOutOfRange(index, count));
        }
        let process = self.open_explorer()?;
        let pt_buf = RemoteBuffer::alloc(process.0, size_of::<POINT>())?;
        let pt = POINT { x: pos.x, y: pos.y };
        unsafe {
            WriteProcessMemory(
                process.0,
                pt_buf.ptr,
                (&raw const pt).cast(),
                size_of::<POINT>(),
                None,
            )
        }
        .map_err(|e| win_err("WriteProcessMemory(POINT)", e))?;

        unsafe {
            SendMessageW(
                self.tree.list_view,
                LVM_SETITEMPOSITION32,
                Some(WPARAM(index)),
                Some(LPARAM(pt_buf.ptr as isize)),
            );
        }
        Ok(())
    }

    pub fn delete_icon(&self, index: usize) -> Result<(), Win32Error> {
        let count = self.icon_count();
        if index >= count {
            return Err(Win32Error::IconIndexOutOfRange(index, count));
        }
        unsafe {
            SendMessageW(self.tree.list_view, LVM_DELETEITEM, Some(WPARAM(index)), Some(LPARAM(0)));
        }
        Ok(())
    }

    pub fn screen_to_listview_point(&self, screen_x: i32, screen_y: i32) -> Point {
        let mut pt = POINT {
            x: screen_x,
            y: screen_y,
        };
        unsafe {
            let _ = ScreenToClient(self.tree.list_view, &mut pt);
        }
        let mut origin = POINT::default();
        if let Ok(process) = self.open_explorer()
            && let Ok(org_buf) = RemoteBuffer::alloc(process.0, size_of::<POINT>())
        {
            let ok = unsafe {
                SendMessageW(
                    self.tree.list_view,
                    LVM_GETORIGIN,
                    None,
                    Some(LPARAM(org_buf.ptr as isize)),
                )
                .0
            } != 0;
            if ok {
                let _ = unsafe {
                    ReadProcessMemory(
                        process.0,
                        org_buf.ptr,
                        (&raw mut origin).cast(),
                        size_of::<POINT>(),
                        None,
                    )
                };
            }
        }
        let dpi = unsafe { GetDpiForWindow(self.tree.list_view) };
        let scale = if dpi >= 96 { dpi as f32 / 96.0 } else { 1.0 };
        let half_cell = (36.0 * scale).round() as i32;
        Point {
            x: (pt.x + origin.x - half_cell).max(0),
            y: (pt.y + origin.y - half_cell).max(0),
        }
    }
}

fn candidate_icon_names(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(disp) = shell_display_name(path) {
        let s = disp.trim().to_ascii_lowercase();
        if !s.is_empty() {
            out.push(s);
        }
    }
    if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
        let s = fname.trim().to_ascii_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    // Only fall back to stem when Shell display name was unavailable (avoids matching `foo.txt` with folder `foo`).
    if out.len() <= 1
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        let s = stem.trim().to_ascii_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn find_icon_index_for_path(icons: &[DesktopIconInfo], path: &Path) -> Option<usize> {
    let candidates = candidate_icon_names(path);
    for cand in &candidates {
        if let Some(icon) = icons
            .iter()
            .find(|ic| ic.name.trim().to_ascii_lowercase() == *cand)
        {
            return Some(icon.index);
        }
    }
    None
}

static DESKTOP_LV_SYNC: OnceLock<Mutex<()>> = OnceLock::new();

fn lv_sync_lock() -> &'static Mutex<()> {
    DESKTOP_LV_SYNC.get_or_init(|| Mutex::new(()))
}

fn notify_shell_item(path: &Path, show: bool, flush: bool) {
    let event = if path.is_dir() {
        if show { SHCNE_MKDIR } else { SHCNE_RMDIR }
    } else if show {
        SHCNE_CREATE
    } else {
        SHCNE_DELETE
    };
    let flags = if flush {
        SHCNF_PATHW | SHCNF_FLUSH
    } else {
        SHCNF_PATHW | SHCNF_FLUSHNOWAIT
    };
    let wpath = HSTRING::from(path.to_string_lossy().as_ref());
    unsafe {
        SHChangeNotify(event, flags, Some(wpath.as_ptr().cast()), None);
    }
}

fn hide_icons_unlocked(lv: &DesktopListView, managed_paths: &[String]) {
    let desktop_paths: Vec<&Path> = managed_paths
        .iter()
        .map(Path::new)
        .filter(|p| is_directly_on_desktop(p) && p.exists())
        .collect();
    if desktop_paths.is_empty() {
        return;
    }
    let Ok(icons) = lv.enumerate_icons() else {
        return;
    };
    // Win11 Explorer's Desktop SysListView32 uses LVS_OWNERDATA, where LVM_DELETEITEM ignores item
    // index and trims from the end; SHCNE_DELETE/SHCNE_RMDIR evicts exact items from CDefView
    // without touching disk files or shifting unrelated icons.
    let to_hide: Vec<&Path> = desktop_paths
        .into_iter()
        .filter(|p| find_icon_index_for_path(&icons, p).is_some())
        .collect();
    for p in to_hide {
        // SHCNF_FLUSH on each notification prevents Shell from coalescing >10 events into UPDATEDIR.
        notify_shell_item(p, false, true);
    }
}

/// Hides icons matching `managed_paths` (direct children of User/Public Desktop)
/// from Explorer's `SysListView32` in-place, leaving all disk files intact on Desktop.
pub fn hide_desktop_icons_for_paths(managed_paths: &[String]) {
    let _guard = lv_sync_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Ok(lv) = find_desktop_listview() else {
        return;
    };
    hide_icons_unlocked(&lv, managed_paths);
}

/// Non-blocking wrapper around `hide_desktop_icons_for_paths`.
pub fn hide_desktop_icons_for_paths_async(managed_paths: Vec<String>) {
    if managed_paths.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        hide_desktop_icons_for_paths(&managed_paths);
    });
}

static DELAYED_HIDE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Coalesced delayed re-hide pass for Desktop icons after Explorer finishes processing
/// asynchronous filesystem watcher / shell rename/paste notifications.
pub fn hide_desktop_icons_for_paths_delayed(managed_paths: Vec<String>, delay_ms: u64) {
    if managed_paths.is_empty() {
        return;
    }
    let epoch = DELAYED_HIDE_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if DELAYED_HIDE_GEN.load(std::sync::atomic::Ordering::Acquire) == epoch {
            hide_desktop_icons_for_paths(&managed_paths);
        }
    });
}

/// Restores a batch of Desktop-resident icons onto `SysListView32` in one serialized pass.
pub fn restore_desktop_icons_async(paths: Vec<String>) {
    let desktop_paths: Vec<String> = paths
        .into_iter()
        .filter(|s| {
            let p = Path::new(s.as_str());
            is_directly_on_desktop(p) && p.exists()
        })
        .collect();
    if desktop_paths.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let _guard = lv_sync_lock().lock().unwrap_or_else(|e| e.into_inner());
        let Ok(lv) = find_desktop_listview() else {
            return;
        };
        let Ok(icons) = lv.enumerate_icons() else {
            return;
        };
        for raw in desktop_paths {
            let p = Path::new(&raw);
            if find_icon_index_for_path(&icons, p).is_none() {
                notify_shell_item(p, true, true);
            }
        }
    });
}

/// Restores a Desktop-resident icon back onto `SysListView32` (off the UI thread so egui never stalls),
/// optionally placing it at `(screen_x, screen_y)` while keeping other managed items hidden.
pub fn restore_desktop_icon_async(
    path_str: String,
    screen_drop_pt: Option<(i32, i32)>,
    still_hidden_paths: Vec<String>,
) {
    let p = Path::new(&path_str);
    if !is_directly_on_desktop(p) || !p.exists() {
        return;
    }
    std::thread::spawn(move || {
        restore_desktop_icon(Path::new(&path_str), screen_drop_pt, &still_hidden_paths);
    });
}

pub fn restore_desktop_icon(
    path: &Path,
    screen_drop_pt: Option<(i32, i32)>,
    still_hidden_paths: &[String],
) {
    if !is_directly_on_desktop(path) || !path.exists() {
        return;
    }
    let _guard = lv_sync_lock().lock().unwrap_or_else(|e| e.into_inner());
    let Ok(lv) = find_desktop_listview() else {
        return;
    };

    let mut found_idx = lv
        .enumerate_icons()
        .ok()
        .and_then(|icons| find_icon_index_for_path(&icons, path));

    if found_idx.is_none() {
        notify_shell_item(path, true, true);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Ok(icons) = lv.enumerate_icons()
                && let Some(idx) = find_icon_index_for_path(&icons, path)
            {
                found_idx = Some(idx);
                break;
            }
        }
    }

    if let Some((sx, sy)) = screen_drop_pt
        && let Some(idx) = found_idx
    {
        let lv_pt = lv.screen_to_listview_point(sx, sy);
        let _ = lv.set_icon_position(idx, lv_pt);
        std::thread::sleep(std::time::Duration::from_millis(45));
        if let Ok(icons2) = lv.enumerate_icons()
            && let Some(idx2) = find_icon_index_for_path(&icons2, path)
        {
            let _ = lv.set_icon_position(idx2, lv_pt);
        }
    }
    let _ = still_hidden_paths;
}

/// Restores all icons on Windows Desktop when DeskLite exits cleanly.
pub fn restore_all_desktop_icons() {
    if let Ok(lv) = find_desktop_listview() {
        unsafe {
            SendMessageW(
                lv.tree.shell_def_view,
                WM_COMMAND,
                Some(WPARAM(0x7103)),
                Some(LPARAM(0)),
            );
        }
    }
    for dir in all_desktop_dirs() {
        let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEDIR,
                SHCNF_PATHW | SHCNF_FLUSH,
                Some(wdir.as_ptr().cast()),
                None,
            );
        }
    }
}
