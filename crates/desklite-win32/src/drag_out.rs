use std::cell::Cell;
use std::mem::size_of;
use std::ptr::copy_nonoverlapping;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL,
    GlobalFree, HANDLE, HGLOBAL, HWND, POINT, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumFORMATETC_Impl, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock, MEM_COMMIT, MEM_RELEASE,
    MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows::Win32::System::Ole::{
    CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{LVHITTESTINFO, LVM_HITTEST};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetWindowThreadProcessId, SendMessageW,
    WindowFromPoint,
};
use windows::core::{BOOL, HRESULT, Ref, Result as WinResult, implement, w};

use crate::{Win32Error, win_err};
use crate::desktop_listview::probe_desktop_window_tree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOutOutcome {
    Cancelled,
    DroppedExternal { moved: bool },
    DroppedOnDesktop { x: i32, y: i32 },
}

#[implement(IEnumFORMATETC)]
struct SingleFormatEnum {
    format: FORMATETC,
    index: Cell<usize>,
}

impl IEnumFORMATETC_Impl for SingleFormatEnum_Impl {
    fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
        if rgelt.is_null() {
            return windows::Win32::Foundation::E_INVALIDARG;
        }
        let idx = self.index.get();
        if idx == 0 && celt >= 1 {
            unsafe {
                *rgelt = self.format;
                if !pceltfetched.is_null() {
                    *pceltfetched = 1;
                }
            }
            self.index.set(1);
            if celt == 1 { S_OK } else { S_FALSE }
        } else {
            if !pceltfetched.is_null() {
                unsafe {
                    *pceltfetched = 0;
                }
            }
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> WinResult<()> {
        let next = self.index.get().saturating_add(celt as usize);
        if next <= 1 {
            self.index.set(next);
            Ok(())
        } else {
            self.index.set(1);
            Err(windows::core::Error::from(S_FALSE))
        }
    }

    fn Reset(&self) -> WinResult<()> {
        self.index.set(0);
        Ok(())
    }

    fn Clone(&self) -> WinResult<IEnumFORMATETC> {
        Ok(SingleFormatEnum {
            format: self.format,
            index: Cell::new(self.index.get()),
        }
        .into())
    }
}

#[implement(IDataObject)]
struct FileDropDataObject {
    paths: Vec<String>,
    format: FORMATETC,
}

fn build_hdrop_hglobal(paths: &[String]) -> WinResult<HGLOBAL> {
    let mut utf16_buf: Vec<u16> = Vec::new();
    for p in paths {
        utf16_buf.extend(p.encode_utf16());
        utf16_buf.push(0);
    }
    utf16_buf.push(0);

    let header_bytes = size_of::<DROPFILES>();
    let total_bytes = header_bytes + utf16_buf.len() * size_of::<u16>();

    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_bytes)?;
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(windows::core::Error::from_thread());
        }
        let df = DROPFILES {
            pFiles: header_bytes as u32,
            pt: POINT::default(),
            fNC: BOOL(0),
            fWide: BOOL(1),
        };
        copy_nonoverlapping(&df as *const DROPFILES as *const u8, ptr.cast::<u8>(), header_bytes);
        copy_nonoverlapping(
            utf16_buf.as_ptr().cast::<u8>(),
            ptr.cast::<u8>().add(header_bytes),
            utf16_buf.len() * size_of::<u16>(),
        );
        let _ = GlobalUnlock(hglobal);
        Ok(hglobal)
    }
}

impl FileDropDataObject {
    fn new(paths: Vec<String>) -> Self {
        let format = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        Self { paths, format }
    }

    fn matches(&self, f: *const FORMATETC) -> bool {
        if f.is_null() {
            return false;
        }
        let fmt = unsafe { &*f };
        fmt.cfFormat == self.format.cfFormat
            && (fmt.tymed & (TYMED_HGLOBAL.0 as u32)) != 0
            && fmt.dwAspect == DVASPECT_CONTENT.0
    }

    fn build_hdrop(&self) -> WinResult<HGLOBAL> {
        build_hdrop_hglobal(&self.paths)
    }
}

impl IDataObject_Impl for FileDropDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        if !self.matches(pformatetcin) {
            return Err(windows::core::Error::from(DV_E_FORMATETC));
        }
        let hglobal = self.build_hdrop()?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> WinResult<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if self.matches(pformatetc) {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        if !pformatetcout.is_null() {
            unsafe {
                (*pformatetcout).ptd = std::ptr::null_mut();
            }
        }
        windows::Win32::Foundation::DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> WinResult<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(windows::core::Error::from(E_NOTIMPL));
        }
        Ok(SingleFormatEnum {
            format: self.format,
            index: Cell::new(0),
        }
        .into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<IAdviseSink>,
    ) -> WinResult<u32> {
        Err(windows::core::Error::from(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
        Err(windows::core::Error::from(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        Err(windows::core::Error::from(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED))
    }
}

fn window_class_name(hwnd: HWND) -> String {
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

fn hit_listview_item_index(lv: HWND, screen_pt: POINT) -> i32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(lv, Some(&mut pid)) };
    if pid == 0 {
        return -1;
    }
    let Ok(proc_h) = (unsafe {
        OpenProcess(
            PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE,
            false,
            pid,
        )
    }) else {
        return -1;
    };
    let mut client_pt = screen_pt;
    unsafe {
        let _ = ScreenToClient(lv, &mut client_pt);
    }
    let mem = unsafe {
        VirtualAllocEx(
            proc_h,
            None,
            size_of::<LVHITTESTINFO>(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    let mut hit_idx = -1i32;
    if !mem.is_null() {
        let mut info = LVHITTESTINFO {
            pt: client_pt,
            ..Default::default()
        };
        if unsafe {
            WriteProcessMemory(
                proc_h,
                mem,
                (&raw const info).cast(),
                size_of::<LVHITTESTINFO>(),
                None,
            )
        }
        .is_ok()
        {
            hit_idx = unsafe { SendMessageW(lv, LVM_HITTEST, None, Some(windows::Win32::Foundation::LPARAM(mem as isize))).0 } as i32;
            let _ = unsafe {
                ReadProcessMemory(
                    proc_h,
                    mem,
                    (&raw mut info).cast(),
                    size_of::<LVHITTESTINFO>(),
                    None,
                )
            };
        }
        let _ = unsafe { VirtualFreeEx(proc_h, mem, 0, MEM_RELEASE) };
    }
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(proc_h) };
    hit_idx
}

fn is_desktop_point(pt: POINT, fence_hwnds: &[isize]) -> bool {
    let hit = unsafe { WindowFromPoint(pt) };
    if hit.is_invalid() {
        return false;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    let hit_raw = hit.0 as isize;
    let root_raw = root.0 as isize;
    if fence_hwnds.contains(&hit_raw) || fence_hwnds.contains(&root_raw) {
        return false;
    }

    let tree = probe_desktop_window_tree();
    if let Some(ref t) = tree {
        if hit == t.list_view {
            // Do not intercept drops directly onto a Desktop folder or Recycle Bin icon.
            return hit_listview_item_index(hit, pt) < 0;
        }
        if hit == t.shell_def_view || Some(hit) == t.worker_w || hit == t.progman {
            return true;
        }
    }

    let hit_cls = window_class_name(hit);
    let root_cls = window_class_name(root);
    matches!(root_cls.as_str(), "Progman" | "WorkerW")
        && matches!(
            hit_cls.as_str(),
            "SysListView32" | "SHELLDLL_DefView" | "WorkerW" | "Progman"
        )
}

#[implement(IDropSource)]
struct SimpleDropSource {
    intercept_desktop_drop: bool,
    fence_hwnds: Vec<isize>,
    dropped_desktop: Arc<AtomicBool>,
    drop_x: Arc<AtomicI32>,
    drop_y: Arc<AtomicI32>,
}

impl IDropSource_Impl for SimpleDropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if (grfkeystate.0 & MK_LBUTTON.0) == 0 {
            if self.intercept_desktop_drop {
                let mut pt = POINT::default();
                if unsafe { GetCursorPos(&mut pt) }.is_ok()
                    && is_desktop_point(pt, &self.fence_hwnds)
                {
                    self.drop_x.store(pt.x, Ordering::Relaxed);
                    self.drop_y.store(pt.y, Ordering::Relaxed);
                    self.dropped_desktop.store(true, Ordering::Release);
                    return DRAGDROP_S_CANCEL;
                }
            }
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

struct OleGuard(bool);

impl Drop for OleGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                OleUninitialize();
            }
        }
    }
}

/// Starts a synchronous Windows OLE `DoDragDrop` operation carrying `CF_HDROP`
/// file paths so the user can drag items out of a DeskLite fence onto the
/// desktop, Explorer, or any Windows app.
///
/// When `is_desktop_resident` is true and the cursor releases over empty Windows Desktop
/// (not a DeskLite fence and not an external folder/app), we intercept cleanly so
/// no duplicate file is created and the caller can restore the existing Desktop icon at `(x, y)`.
pub fn start_file_drag_out(
    paths: &[String],
    is_desktop_resident: bool,
    fence_hwnds: &[isize],
) -> Result<DragOutOutcome, Win32Error> {
    if paths.is_empty() {
        return Ok(DragOutOutcome::Cancelled);
    }
    unsafe {
        let ole_ok = OleInitialize(None).is_ok();
        let _ole_guard = OleGuard(ole_ok);
        let data_obj: IDataObject = FileDropDataObject::new(paths.to_vec()).into();
        let dropped_desktop = Arc::new(AtomicBool::new(false));
        let drop_x = Arc::new(AtomicI32::new(0));
        let drop_y = Arc::new(AtomicI32::new(0));
        let drop_src: IDropSource = SimpleDropSource {
            intercept_desktop_drop: is_desktop_resident,
            fence_hwnds: fence_hwnds.to_vec(),
            dropped_desktop: Arc::clone(&dropped_desktop),
            drop_x: Arc::clone(&drop_x),
            drop_y: Arc::clone(&drop_y),
        }
        .into();
        let mut effect = DROPEFFECT(0);
        let ok_effects = if is_desktop_resident {
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK
        } else {
            DROPEFFECT_COPY | DROPEFFECT_LINK
        };
        let hr = DoDragDrop(&data_obj, &drop_src, ok_effects, &mut effect);
        if dropped_desktop.load(Ordering::Acquire) {
            return Ok(DragOutOutcome::DroppedOnDesktop {
                x: drop_x.load(Ordering::Relaxed),
                y: drop_y.load(Ordering::Relaxed),
            });
        }
        let moved = (effect.0 & DROPEFFECT_MOVE.0) != 0;
        if hr == DRAGDROP_S_DROP {
            Ok(DragOutOutcome::DroppedExternal { moved })
        } else {
            Ok(DragOutOutcome::Cancelled)
        }
    }
}

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn build_drop_effect_hglobal(effect: u32) -> WinResult<HGLOBAL> {
    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size_of::<u32>())?;
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(windows::core::Error::from_thread());
        }
        *(ptr.cast::<u32>()) = effect;
        let _ = GlobalUnlock(hglobal);
        Ok(hglobal)
    }
}

fn open_clipboard_with_retry(owner_hwnd: isize) -> Result<ClipboardGuard, Win32Error> {
    const DELAYS_MS: [u64; 11] = [3, 5, 8, 10, 12, 15, 15, 15, 20, 20, 20];
    let owner = (owner_hwnd != 0).then_some(HWND(owner_hwnd as *mut _));
    let mut last_err = None;
    for attempt in 0..=DELAYS_MS.len() {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return Ok(ClipboardGuard);
        }
        last_err = Some(windows::core::Error::from_thread());
        if let Some(&ms) = DELAYS_MS.get(attempt) {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
    Err(win_err(
        "OpenClipboard",
        last_err.unwrap_or_else(windows::core::Error::from_thread),
    ))
}

/// Fast non-blocking check whether `CF_HDROP` files are currently present on the clipboard.
pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_ok()
}

/// Places `paths` onto the Windows clipboard as `CF_HDROP` with `Preferred DropEffect`
/// (`DROPEFFECT_COPY` or `DROPEFFECT_MOVE` for cut) so Explorer/apps honor Copy & Cut.
pub fn set_clipboard_files(paths: &[String], cut: bool) -> Result<(), Win32Error> {
    set_clipboard_files_for_window(0, paths, cut)
}

/// Same as `set_clipboard_files`, binding clipboard ownership to `owner_hwnd` so
/// `EmptyClipboard()` succeeds reliably even under clipboard-history contention.
pub fn set_clipboard_files_for_window(
    owner_hwnd: isize,
    paths: &[String],
    cut: bool,
) -> Result<(), Win32Error> {
    let valid: Vec<String> = paths
        .iter()
        .filter(|p| !p.is_empty() && std::path::Path::new(p.as_str()).exists())
        .cloned()
        .collect();
    if valid.is_empty() {
        return Ok(());
    }

    let hdrop = build_hdrop_hglobal(&valid).map_err(|e| win_err("build_hdrop_hglobal", e))?;
    let effect_val = if cut { DROPEFFECT_MOVE.0 } else { DROPEFFECT_COPY.0 };
    let heffect = build_drop_effect_hglobal(effect_val).ok();

    let _guard = match open_clipboard_with_retry(owner_hwnd) {
        Ok(g) => g,
        Err(e) => {
            unsafe {
                let _ = GlobalFree(Some(hdrop));
                if let Some(he) = heffect {
                    let _ = GlobalFree(Some(he));
                }
            }
            return Err(e);
        }
    };

    if let Err(e) = unsafe { EmptyClipboard() } {
        unsafe {
            let _ = GlobalFree(Some(hdrop));
            if let Some(he) = heffect {
                let _ = GlobalFree(Some(he));
            }
        }
        return Err(win_err("EmptyClipboard", e));
    }
    if let Err(e) = unsafe { SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(hdrop.0))) } {
        unsafe {
            let _ = GlobalFree(Some(hdrop));
            if let Some(he) = heffect {
                let _ = GlobalFree(Some(he));
            }
        }
        return Err(win_err("SetClipboardData(CF_HDROP)", e));
    }

    if let Some(he) = heffect {
        let fmt = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
        if fmt == 0 || unsafe { SetClipboardData(fmt, Some(HANDLE(he.0))) }.is_err() {
            unsafe {
                let _ = GlobalFree(Some(he));
            }
        }
    }
    Ok(())
}

/// Copies `paths` onto the Windows clipboard as `CF_HDROP`.
pub fn copy_files_to_clipboard(paths: &[String]) -> Result<(), Win32Error> {
    set_clipboard_files(paths, false)
}

/// Copies UTF-16 text (`CF_UNICODETEXT`) onto the Windows clipboard (e.g. file paths).
pub fn copy_text_to_clipboard(text: &str) -> Result<(), Win32Error> {
    if text.is_empty() {
        return Ok(());
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let bytes = utf16.len() * size_of::<u16>();
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes) }
        .map_err(|e| win_err("GlobalAlloc(CF_UNICODETEXT)", e))?;
    unsafe {
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(win_err(
                "GlobalLock(CF_UNICODETEXT)",
                windows::core::Error::from_thread(),
            ));
        }
        copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), ptr.cast::<u8>(), bytes);
        let _ = GlobalUnlock(hglobal);
    }

    let _guard = match open_clipboard_with_retry(0) {
        Ok(g) => g,
        Err(e) => {
            unsafe {
                let _ = GlobalFree(Some(hglobal));
            }
            return Err(e);
        }
    };
    if let Err(e) = unsafe { EmptyClipboard() } {
        unsafe {
            let _ = GlobalFree(Some(hglobal));
        }
        return Err(win_err("EmptyClipboard", e));
    }
    if let Err(e) = unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hglobal.0))) } {
        unsafe {
            let _ = GlobalFree(Some(hglobal));
        }
        return Err(win_err("SetClipboardData(CF_UNICODETEXT)", e));
    }
    Ok(())
}

/// Reads `CF_HDROP` file paths and whether `Preferred DropEffect` indicates Cut (`DROPEFFECT_MOVE`).
pub fn read_clipboard_files() -> Option<(Vec<String>, bool)> {
    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_err() {
        return None;
    }
    let _guard = open_clipboard_with_retry(0).ok()?;
    let hdata = unsafe { GetClipboardData(CF_HDROP.0 as u32) }.ok()?;
    let hdrop = HDROP(hdata.0);
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    if count == 0 {
        return None;
    }
    let mut paths = Vec::with_capacity(count as usize);
    for idx in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, idx, None) } as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let copied = unsafe { DragQueryFileW(hdrop, idx, Some(&mut buf)) } as usize;
        let s = String::from_utf16_lossy(&buf[..copied]);
        if !s.is_empty() {
            paths.push(s);
        }
    }
    if paths.is_empty() {
        return None;
    }

    let mut is_cut = false;
    let fmt = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    if fmt != 0
        && let Ok(heff) = unsafe { GetClipboardData(fmt) }
    {
        let hg = HGLOBAL(heff.0);
        let ptr = unsafe { GlobalLock(hg) };
        if !ptr.is_null() {
            let eff = unsafe { *(ptr.cast::<u32>()) };
            is_cut = (eff & DROPEFFECT_MOVE.0) != 0 && (eff & DROPEFFECT_COPY.0) == 0;
            let _ = unsafe { GlobalUnlock(hg) };
        }
    }
    Some((paths, is_cut))
}

/// Reads UTF-16 text (`CF_UNICODETEXT`) from the Windows clipboard.
pub fn read_clipboard_text() -> Option<String> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_err() {
        return None;
    }
    let _guard = open_clipboard_with_retry(0).ok()?;
    let hdata = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }.ok()?;
    let hg = HGLOBAL(hdata.0);
    let ptr = unsafe { GlobalLock(hg) };
    if ptr.is_null() {
        return None;
    }
    let text = unsafe {
        let wptr = ptr.cast::<u16>();
        let mut len = 0usize;
        while *wptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(wptr, len))
    };
    let _ = unsafe { GlobalUnlock(hg) };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
