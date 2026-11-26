use std::cell::Cell;
use std::mem::size_of;
use std::ptr::copy_nonoverlapping;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};

use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, GlobalFree, HANDLE, HGLOBAL, HWND,
    LPARAM, POINT, S_FALSE, S_OK, WPARAM,
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
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
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
    OpenProcess, PROCESS_VM_OPERATION, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{LVHITTESTINFO, LVM_HITTEST};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetWindowThreadProcessId, PostMessageW,
    SendMessageW, WindowFromPoint,
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

#[derive(Debug, Clone)]
pub struct AsyncDragRequest {
    pub source_fence_id: Option<i32>,
    pub paths: Vec<String>,
    pub is_desktop_resident: bool,
    pub bucket_hwnds: Vec<isize>,
    pub free_desktop_hwnd: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragTargetOutcome {
    DroppedOnFence {
        target_hwnd: isize,
        screen_x: i32,
        screen_y: i32,
    },
    DroppedOnFreeDesktop {
        screen_x: i32,
        screen_y: i32,
    },
    DroppedExternal {
        moved: bool,
    },
    Cancelled,
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

const MAX_GET_DATA_CALLS: usize = 256;

#[implement(IDataObject)]
struct FileDropDataObject {
    hdrop_payload: Vec<u8>,
    format: FORMATETC,
    get_data_calls: Cell<usize>,
}

fn encode_hdrop_payload(paths: &[String]) -> Vec<u8> {
    let mut utf16_buf: Vec<u16> = Vec::new();
    for p in paths {
        utf16_buf.extend(p.encode_utf16());
        utf16_buf.push(0);
    }
    utf16_buf.push(0);

    let header_bytes = size_of::<DROPFILES>();
    let utf16_bytes = utf16_buf.len() * size_of::<u16>();
    let mut payload = vec![0u8; header_bytes + utf16_bytes];
    let df = DROPFILES {
        pFiles: header_bytes as u32,
        pt: POINT::default(),
        fNC: BOOL(0),
        fWide: BOOL(1),
    };
    unsafe {
        copy_nonoverlapping(&df as *const DROPFILES as *const u8, payload.as_mut_ptr(), header_bytes);
        copy_nonoverlapping(
            utf16_buf.as_ptr().cast::<u8>(),
            payload.as_mut_ptr().add(header_bytes),
            utf16_bytes,
        );
    }
    payload
}

fn alloc_hglobal_from_bytes(payload: &[u8]) -> WinResult<HGLOBAL> {
    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, payload.len())?;
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(windows::core::Error::from_thread());
        }
        copy_nonoverlapping(payload.as_ptr(), ptr.cast::<u8>(), payload.len());
        let _ = GlobalUnlock(hglobal);
        Ok(hglobal)
    }
}

fn build_hdrop_hglobal(paths: &[String]) -> WinResult<HGLOBAL> {
    let payload = encode_hdrop_payload(paths);
    alloc_hglobal_from_bytes(&payload)
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
        Self {
            hdrop_payload: encode_hdrop_payload(&paths),
            format,
            get_data_calls: Cell::new(0),
        }
    }

    fn matches(&self, f: *const FORMATETC) -> bool {
        if f.is_null() {
            return false;
        }
        let fmt = unsafe { &*f };
        fmt.cfFormat == self.format.cfFormat
            && (fmt.tymed & (TYMED_HGLOBAL.0 as u32)) != 0
            && fmt.dwAspect == DVASPECT_CONTENT.0
            && (fmt.lindex == -1 || fmt.lindex == 0)
    }

    fn build_hdrop(&self) -> WinResult<HGLOBAL> {
        alloc_hglobal_from_bytes(&self.hdrop_payload)
    }
}

impl IDataObject_Impl for FileDropDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        if !self.matches(pformatetcin) {
            return Err(windows::core::Error::from(DV_E_FORMATETC));
        }
        let calls = self.get_data_calls.get();
        if calls >= MAX_GET_DATA_CALLS {
            return Err(windows::core::Error::from(E_OUTOFMEMORY));
        }
        self.get_data_calls.set(calls + 1);
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
        pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        if pformatectin.is_null() || pformatetcout.is_null() {
            return E_INVALIDARG;
        }
        unsafe {
            *pformatetcout = *pformatectin;
            (*pformatetcout).ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
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
    let Ok(proc_h) = (unsafe { OpenProcess(PROCESS_VM_OPERATION | PROCESS_VM_WRITE, false, pid) })
    else {
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
        let info = LVHITTESTINFO {
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
            hit_idx = unsafe {
                SendMessageW(
                    lv,
                    LVM_HITTEST,
                    None,
                    Some(windows::Win32::Foundation::LPARAM(mem as isize)),
                )
                .0
            } as i32;
        }
        let _ = unsafe { VirtualFreeEx(proc_h, mem, 0, MEM_RELEASE) };
    }
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(proc_h) };
    hit_idx
}

fn is_primary_mouse_down_async() -> bool {
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}

fn match_hwnd_in_slice(hit: HWND, root: HWND, candidates: &[isize]) -> Option<isize> {
    let hit_raw = hit.0 as isize;
    if hit_raw != 0 && candidates.contains(&hit_raw) {
        return Some(hit_raw);
    }
    let root_raw = root.0 as isize;
    if root_raw != 0 && candidates.contains(&root_raw) {
        return Some(root_raw);
    }
    None
}

fn is_native_desktop_window(hit: HWND, root: HWND, pt: POINT) -> bool {
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

fn is_desktop_point(pt: POINT, fence_hwnds: &[isize]) -> bool {
    let hit = unsafe { WindowFromPoint(pt) };
    if hit.is_invalid() {
        return false;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    if match_hwnd_in_slice(hit, root, fence_hwnds).is_some() {
        return false;
    }
    is_native_desktop_window(hit, root, pt)
}

const ASYNC_DROP_NONE: u8 = 0;
const ASYNC_DROP_FENCE: u8 = 1;
const ASYNC_DROP_FREE_DESKTOP: u8 = 2;

#[implement(IDropSource)]
struct AsyncDropSource {
    bucket_hwnds: Vec<isize>,
    free_desktop_hwnd: isize,
    drop_kind: Arc<AtomicU8>,
    target_hwnd: Arc<AtomicIsize>,
    drop_x: Arc<AtomicI32>,
    drop_y: Arc<AtomicI32>,
}

impl IDropSource_Impl for AsyncDropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        let lbutton_released =
            (grfkeystate.0 & MK_LBUTTON.0) == 0 || !is_primary_mouse_down_async();
        if lbutton_released {
            let mut pt = POINT::default();
            if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                let hit = unsafe { WindowFromPoint(pt) };
                if !hit.is_invalid() {
                    let root = unsafe { GetAncestor(hit, GA_ROOT) };
                    if let Some(bucket_hwnd) = match_hwnd_in_slice(hit, root, &self.bucket_hwnds) {
                        self.target_hwnd.store(bucket_hwnd, Ordering::Relaxed);
                        self.drop_x.store(pt.x, Ordering::Relaxed);
                        self.drop_y.store(pt.y, Ordering::Relaxed);
                        self.drop_kind.store(ASYNC_DROP_FENCE, Ordering::Release);
                        return DRAGDROP_S_CANCEL;
                    }
                    let hit_raw = hit.0 as isize;
                    let root_raw = root.0 as isize;
                    let hit_free = self.free_desktop_hwnd != 0
                        && (hit_raw == self.free_desktop_hwnd
                            || root_raw == self.free_desktop_hwnd);
                    if hit_free || is_native_desktop_window(hit, root, pt) {
                        self.drop_x.store(pt.x, Ordering::Relaxed);
                        self.drop_y.store(pt.y, Ordering::Relaxed);
                        self.drop_kind
                            .store(ASYNC_DROP_FREE_DESKTOP, Ordering::Release);
                        return DRAGDROP_S_CANCEL;
                    }
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

fn post_drag_notify(notify_hwnd: Option<isize>, notify_msg: u32, source_fence_id: Option<i32>) {
    if let Some(nh) = notify_hwnd
        && nh != 0
        && notify_msg != 0
    {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(nh as *mut _)),
                notify_msg,
                WPARAM(source_fence_id.unwrap_or(0) as usize),
                LPARAM(0),
            );
        }
    }
}

/// Runs a non-blocking OLE `DoDragDrop` session on a dedicated STA thread so the UI
/// event loop (`RedrawRequested`) is never stalled and internal fence/FreeDesktop drops
/// bypass `winit` `DroppedFile` batching completely.
pub fn start_file_drag_out_async(
    req: AsyncDragRequest,
    notify_hwnd: Option<isize>,
    notify_msg: u32,
) -> Receiver<DragTargetOutcome> {
    let (tx, rx) = mpsc::channel();
    let source_fence_id = req.source_fence_id;
    if req.paths.is_empty() {
        let _ = tx.send(DragTargetOutcome::Cancelled);
        post_drag_notify(notify_hwnd, notify_msg, source_fence_id);
        return rx;
    }

    let tx_fallback = tx.clone();
    let spawn_res = std::thread::Builder::new()
        .name("desklite-drag-out".to_string())
        .spawn(move || {
            let outcome = unsafe {
                // Phase 1: Pure internal cursor tracking while the cursor stays over DeskLite
                // windows or the desktop. This avoids calling OLE DoDragDrop on a background
                // STA thread while hovering same-process winit IDropTarget windows (which would
                // cause a cross-apartment COM SendMessage deadlock -> Application Hang).
                let mut enter_external_ole = false;
                let mut internal_outcome: Option<DragTargetOutcome> = None;
                loop {
                    let esc_down = (GetAsyncKeyState(0x1B) as u16 & 0x8000) != 0;
                    if esc_down {
                        internal_outcome = Some(DragTargetOutcome::Cancelled);
                        break;
                    }
                    let mut pt = POINT::default();
                    let has_pt = GetCursorPos(&mut pt).is_ok();
                    let hit = if has_pt { WindowFromPoint(pt) } else { HWND::default() };
                    let root = if !hit.is_invalid() {
                        GetAncestor(hit, GA_ROOT)
                    } else {
                        HWND::default()
                    };
                    let hit_bucket = if !hit.is_invalid() {
                        match_hwnd_in_slice(hit, root, &req.bucket_hwnds)
                    } else {
                        None
                    };
                    let hit_free = !hit.is_invalid()
                        && req.free_desktop_hwnd != 0
                        && (hit.0 as isize == req.free_desktop_hwnd
                            || root.0 as isize == req.free_desktop_hwnd);
                    let hit_native_desktop =
                        !hit.is_invalid() && is_native_desktop_window(hit, root, pt);

                    if !is_primary_mouse_down_async() {
                        if let Some(target_hwnd) = hit_bucket {
                            internal_outcome = Some(DragTargetOutcome::DroppedOnFence {
                                target_hwnd,
                                screen_x: pt.x,
                                screen_y: pt.y,
                            });
                        } else if hit_free || hit_native_desktop {
                            internal_outcome = Some(DragTargetOutcome::DroppedOnFreeDesktop {
                                screen_x: pt.x,
                                screen_y: pt.y,
                            });
                        } else {
                            internal_outcome = Some(DragTargetOutcome::Cancelled);
                        }
                        break;
                    }

                    if !hit.is_invalid() && hit_bucket.is_none() && !hit_free && !hit_native_desktop
                    {
                        enter_external_ole = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(12));
                }

                if let Some(done) = internal_outcome {
                    done
                } else if enter_external_ole {
                    let ole_ok = OleInitialize(None).is_ok();
                    let _ole_guard = OleGuard(ole_ok);
                    let data_obj: IDataObject = FileDropDataObject::new(req.paths).into();
                    let drop_kind = Arc::new(AtomicU8::new(ASYNC_DROP_NONE));
                    let target_hwnd = Arc::new(AtomicIsize::new(0));
                    let drop_x = Arc::new(AtomicI32::new(0));
                    let drop_y = Arc::new(AtomicI32::new(0));
                    let drop_src: IDropSource = AsyncDropSource {
                        bucket_hwnds: req.bucket_hwnds,
                        free_desktop_hwnd: req.free_desktop_hwnd,
                        drop_kind: Arc::clone(&drop_kind),
                        target_hwnd: Arc::clone(&target_hwnd),
                        drop_x: Arc::clone(&drop_x),
                        drop_y: Arc::clone(&drop_y),
                    }
                    .into();
                    let mut effect = DROPEFFECT(0);
                    let ok_effects = if req.is_desktop_resident {
                        DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK
                    } else {
                        DROPEFFECT_COPY | DROPEFFECT_LINK
                    };
                    let hr = DoDragDrop(&data_obj, &drop_src, ok_effects, &mut effect);
                    match drop_kind.load(Ordering::Acquire) {
                        ASYNC_DROP_FENCE => DragTargetOutcome::DroppedOnFence {
                            target_hwnd: target_hwnd.load(Ordering::Relaxed),
                            screen_x: drop_x.load(Ordering::Relaxed),
                            screen_y: drop_y.load(Ordering::Relaxed),
                        },
                        ASYNC_DROP_FREE_DESKTOP => DragTargetOutcome::DroppedOnFreeDesktop {
                            screen_x: drop_x.load(Ordering::Relaxed),
                            screen_y: drop_y.load(Ordering::Relaxed),
                        },
                        _ => {
                            if hr == DRAGDROP_S_DROP {
                                DragTargetOutcome::DroppedExternal {
                                    moved: (effect.0 & DROPEFFECT_MOVE.0) != 0,
                                }
                            } else {
                                DragTargetOutcome::Cancelled
                            }
                        }
                    }
                } else {
                    DragTargetOutcome::Cancelled
                }
            };
            let _ = tx.send(outcome);
            post_drag_notify(notify_hwnd, notify_msg, source_fence_id);
        });

    if spawn_res.is_err() {
        let _ = tx_fallback.send(DragTargetOutcome::Cancelled);
        post_drag_notify(notify_hwnd, notify_msg, source_fence_id);
    }

    rx
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
        if (grfkeystate.0 & MK_LBUTTON.0) == 0 || !is_primary_mouse_down_async() {
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
    const DELAYS_MS: [u64; 5] = [2, 4, 6, 8, 10];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_hdrop_payload_writes_wide_double_null_list() {
        let paths = vec!["C:\\a.txt".to_string(), "D:\\b.txt".to_string()];
        let bytes = encode_hdrop_payload(&paths);
        let hdr = size_of::<DROPFILES>();
        assert!(bytes.len() > hdr);
        let df = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<DROPFILES>()) };
        assert_eq!(df.pFiles as usize, hdr);
        assert!(df.fWide.as_bool());
        let wide: Vec<u16> = bytes[hdr..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        assert_eq!(wide.last().copied(), Some(0));
        let text = String::from_utf16_lossy(&wide);
        assert!(text.starts_with("C:\\a.txt\0D:\\b.txt\0\0"));
    }

    #[test]
    fn canonical_format_etc_copies_format_and_clears_ptd() {
        let obj: IDataObject = FileDropDataObject::new(vec!["C:\\x.txt".to_string()]).into();
        let mut fake_ptd = 0x1234usize as *mut _;
        let fmt_in = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: fake_ptd,
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let mut fmt_out = FORMATETC::default();
        let hr = unsafe { obj.GetCanonicalFormatEtc(&fmt_in, &mut fmt_out) };
        assert_eq!(hr, DATA_S_SAMEFORMATETC);
        assert_eq!(fmt_out.cfFormat, CF_HDROP.0);
        assert_eq!(fmt_out.lindex, -1);
        assert!(fmt_out.ptd.is_null());
        let _ = &mut fake_ptd;

        let hr_null = unsafe { obj.GetCanonicalFormatEtc(std::ptr::null(), &mut fmt_out) };
        assert_eq!(hr_null, E_INVALIDARG);
    }

    #[test]
    fn start_file_drag_out_async_empty_returns_cancelled_immediately() {
        let rx = start_file_drag_out_async(
            AsyncDragRequest {
                source_fence_id: Some(101),
                paths: Vec::new(),
                is_desktop_resident: true,
                bucket_hwnds: Vec::new(),
                free_desktop_hwnd: 0,
            },
            None,
            0,
        );
        assert_eq!(rx.recv().unwrap(), DragTargetOutcome::Cancelled);
    }

    #[test]
    fn match_hwnd_in_slice_checks_hit_and_root() {
        let hit = HWND(10 as *mut _);
        let root = HWND(20 as *mut _);
        assert_eq!(match_hwnd_in_slice(hit, root, &[20, 30]), Some(20));
        assert_eq!(match_hwnd_in_slice(hit, root, &[10, 20]), Some(10));
        assert_eq!(match_hwnd_in_slice(hit, root, &[30]), None);
    }
}
