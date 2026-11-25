use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Controls::IImageList;
use windows::Win32::UI::Shell::{
    SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES,
    SHGetFileInfoW, SHGetImageList, SHIL_EXTRALARGE, SHParseDisplayName, SHFILEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
use windows::core::{HSTRING, PCWSTR};

use crate::Win32Error;

#[derive(Debug, Clone)]
pub struct DecodedIcon {
    pub width: u32,
    pub height: u32,
    pub pbgra: Vec<u8>,
}

thread_local! {
    static TLS_WIC: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn with_thread_wic<T>(f: impl FnOnce(&IWICImagingFactory) -> Option<T>) -> Option<T> {
    TLS_WIC.with(|slot| {
        let mut borrow = slot.borrow_mut();
        if borrow.is_none() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let wic: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .ok()?;
            *borrow = Some(wic);
        }
        borrow.as_ref().and_then(f)
    })
}

#[derive(Default)]
pub struct IconCache {
    cache: HashMap<String, DecodedIcon>,
}

impl IconCache {
    pub fn new() -> Result<Self, Win32Error> {
        Ok(Self::default())
    }

    pub fn get_or_load(&mut self, path: &str) -> Option<&DecodedIcon> {
        let key = normalize_cache_key(path);
        if !self.cache.contains_key(&key)
            && let Some(decoded) = self.extract_icon(path)
        {
            self.cache.insert(key.clone(), decoded);
        }
        self.cache.get(&key)
    }

    fn extract_icon(&self, path: &str) -> Option<DecodedIcon> {
        let hicon = load_hicon_for_path(path)?;
        let decoded = hicon_to_pbgra(hicon);
        unsafe {
            let _ = DestroyIcon(hicon);
        }
        decoded
    }
}

fn hicon_to_pbgra(hicon: HICON) -> Option<DecodedIcon> {
    with_thread_wic(|wic| unsafe {
        let bmp = wic.CreateBitmapFromHICON(hicon).ok()?;
        let mut width = 0u32;
        let mut height = 0u32;
        bmp.GetSize(&mut width, &mut height).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(
            &bmp,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;
        let stride = width * 4;
        let mut pbgra = vec![0u8; (stride * height) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut pbgra).ok()?;
        Some(DecodedIcon {
            width,
            height,
            pbgra,
        })
    })
}

fn normalize_cache_key(path: &str) -> String {
    if path.starts_with("::{") {
        return path.to_ascii_lowercase();
    }
    let p = Path::new(path);
    if p.exists() {
        // Per-file icon for executables, shortcuts, directories, or images with thumbnails/custom icons.
        path.to_ascii_lowercase()
    } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        format!("ext:.{}", ext.to_ascii_lowercase())
    } else {
        path.to_ascii_lowercase()
    }
}

fn load_hicon_for_path(path: &str) -> Option<HICON> {
    unsafe {
        if path.starts_with("::{") {
            let wpath = HSTRING::from(path);
            let mut pidl = std::ptr::null_mut();
            if SHParseDisplayName(&wpath, None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                let mut sfi = SHFILEINFOW::default();
                let ok = SHGetFileInfoW(
                    PCWSTR(pidl.cast()),
                    FILE_ATTRIBUTE_NORMAL,
                    Some(&mut sfi),
                    std::mem::size_of::<SHFILEINFOW>() as u32,
                    SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON,
                );
                CoTaskMemFree(Some(pidl.cast()));
                if ok != 0 && !sfi.hIcon.is_invalid() {
                    return Some(sfi.hIcon);
                }
            }
            return None;
        }

        let wpath = HSTRING::from(path);
        let exists = Path::new(path).exists();
        let mut sfi = SHFILEINFOW::default();

        // Try 48x48 SHIL_EXTRALARGE system image list first when path exists.
        if exists {
            let res = SHGetFileInfoW(
                &wpath,
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut sfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX,
            );
            if res != 0
                && let Ok(img_list) = SHGetImageList::<IImageList>(SHIL_EXTRALARGE as i32)
                && let Ok(hicon) = img_list.GetIcon(sfi.iIcon, 0)
                && !hicon.is_invalid()
            {
                return Some(hicon);
            }
        }

        let flags = if exists {
            SHGFI_ICON | SHGFI_LARGEICON
        } else {
            SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES
        };
        let ok = SHGetFileInfoW(
            &wpath,
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        );
        if ok != 0 && !sfi.hIcon.is_invalid() {
            Some(sfi.hIcon)
        } else {
            None
        }
    }
}
