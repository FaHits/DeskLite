use std::ffi::c_void;
use std::mem::size_of;

use desklite_core::Point;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, TRUE, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{
    LVIF_TEXT, LVITEMW, LVM_GETITEMCOUNT, LVM_GETITEMPOSITION, LVM_GETITEMTEXTW,
    LVM_SETITEMPOSITION32,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, FindWindowW, GetAncestor, GetClassNameW,
    GetDesktopWindow, GetWindowThreadProcessId, GA_ROOT, SendMessageW,
};
use windows::core::{BOOL, w};

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
}
