use std::cell::Cell;
use std::mem::size_of;
use std::ptr::copy_nonoverlapping;

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL,
    GlobalFree, HGLOBAL, POINT, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumFORMATETC_Impl, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DoDragDrop,
    IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::DROPFILES;
use windows::core::{BOOL, HRESULT, Ref, Result as WinResult, implement};

use crate::Win32Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOutOutcome {
    Dropped,
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

#[implement(IDataObject)]
struct FileDropDataObject {
    paths: Vec<String>,
    format: FORMATETC,
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
        let mut utf16_buf: Vec<u16> = Vec::new();
        for p in &self.paths {
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

#[implement(IDropSource)]
struct SimpleDropSource;

impl IDropSource_Impl for SimpleDropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if (grfkeystate.0 & MK_LBUTTON.0) == 0 {
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
pub fn start_file_drag_out(paths: &[String]) -> Result<DragOutOutcome, Win32Error> {
    if paths.is_empty() {
        return Ok(DragOutOutcome::Cancelled);
    }
    unsafe {
        let ole_ok = OleInitialize(None).is_ok();
        let _ole_guard = OleGuard(ole_ok);
        let data_obj: IDataObject = FileDropDataObject::new(paths.to_vec()).into();
        let drop_src: IDropSource = SimpleDropSource.into();
        let mut effect = DROPEFFECT(0);
        let hr = DoDragDrop(
            &data_obj,
            &drop_src,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
            &mut effect,
        );
        if hr == DRAGDROP_S_DROP {
            Ok(DragOutOutcome::Dropped)
        } else {
            Ok(DragOutOutcome::Cancelled)
        }
    }
}
