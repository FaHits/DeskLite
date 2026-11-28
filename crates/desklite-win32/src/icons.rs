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
    SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SYSICONINDEX,
    SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetImageList, SHIL_EXTRALARGE, SHIL_JUMBO,
    SHParseDisplayName,
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
    static TLS_COM_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_thread_com_init() {
    TLS_COM_INIT.with(|init| {
        if !init.get() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            init.set(true);
        }
    });
}

fn with_thread_wic<T>(f: impl FnOnce(&IWICImagingFactory) -> Option<T>) -> Option<T> {
    ensure_thread_com_init();
    TLS_WIC.with(|slot| {
        let mut borrow = slot.borrow_mut();
        if borrow.is_none() {
            let wic: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .ok()?;
            *borrow = Some(wic);
        }
        borrow.as_ref().and_then(f)
    })
}

const DEFAULT_MAX_CACHE_ENTRIES: usize = 1024;

#[derive(Debug, Clone)]
struct CacheEntry {
    icon: DecodedIcon,
    last_used: u64,
}

pub struct IconCache {
    cache: HashMap<String, CacheEntry>,
    max_entries: usize,
    access_seq: u64,
}

impl Default for IconCache {
    fn default() -> Self {
        Self {
            cache: HashMap::new(),
            max_entries: DEFAULT_MAX_CACHE_ENTRIES,
            access_seq: 0,
        }
    }
}

impl IconCache {
    pub fn new() -> Result<Self, Win32Error> {
        Ok(Self::default())
    }

    pub fn with_capacity(max_entries: usize) -> Result<Self, Win32Error> {
        Ok(Self {
            cache: HashMap::new(),
            max_entries: max_entries.max(1),
            access_seq: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub fn invalidate_path(&mut self, path: &Path) {
        let raw = path.to_string_lossy();
        let trimmed = raw.trim_end_matches(['\\', '/']).to_ascii_lowercase();
        self.cache.remove(&trimmed);
        let raw_lower = raw.to_ascii_lowercase();
        if raw_lower != trimmed {
            self.cache.remove(&raw_lower);
        }
    }

    fn insert_lru(&mut self, key: String, icon: DecodedIcon) {
        let max_entries = self.max_entries.max(1);
        if !self.cache.contains_key(&key) {
            while self.cache.len() >= max_entries {
                let Some(oldest_key) = self
                    .cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                self.cache.remove(&oldest_key);
            }
        }
        self.access_seq = self.access_seq.wrapping_add(1);
        let seq = self.access_seq;
        self.cache.insert(
            key,
            CacheEntry {
                icon,
                last_used: seq,
            },
        );
    }

    pub fn get_cached_only(&mut self, path: &str) -> Option<&DecodedIcon> {
        let key = normalize_cache_key(path);
        if !self.cache.contains_key(&key) {
            return None;
        }
        self.access_seq = self.access_seq.wrapping_add(1);
        let seq = self.access_seq;
        let entry = self.cache.get_mut(&key)?;
        entry.last_used = seq;
        Some(&entry.icon)
    }

    pub fn insert_predecoded(&mut self, path: &str, icon: DecodedIcon) {
        let key = normalize_cache_key(path);
        self.insert_lru(key, icon);
    }

    pub fn get_or_load(&mut self, path: &str) -> Option<&DecodedIcon> {
        let key = normalize_cache_key(path);
        if !self.cache.contains_key(&key) {
            let decoded = self.extract_icon(path)?;
            self.insert_lru(key.clone(), decoded);
            return self.cache.get(&key).map(|entry| &entry.icon);
        }
        self.access_seq = self.access_seq.wrapping_add(1);
        let seq = self.access_seq;
        let entry = self.cache.get_mut(&key)?;
        entry.last_used = seq;
        Some(&entry.icon)
    }

    fn extract_icon(&self, path: &str) -> Option<DecodedIcon> {
        extract_file_icon_pbgra(Path::new(path))
    }
}

/// Extracts a 32bpp premultiplied BGRA (`PBGRA`) icon bitmap for `path` on the calling thread.
pub fn extract_file_icon_pbgra(path: &Path) -> Option<DecodedIcon> {
    ensure_thread_com_init();
    let raw = path.to_string_lossy();
    let path_str = raw.as_ref();
    if path.exists()
        && let Some(jumbo_hicon) = load_sys_image_list_icon(path_str, SHIL_JUMBO as i32)
    {
        let jumbo = hicon_to_pbgra(jumbo_hicon);
        unsafe {
            let _ = DestroyIcon(jumbo_hicon);
        }
        if let Some(decoded) = jumbo
            && has_pixels_outside_top_left(&decoded, 64)
        {
            return Some(decoded);
        }
    }
    let hicon = load_hicon_for_path(path_str)?;
    let decoded = hicon_to_pbgra(hicon);
    unsafe {
        let _ = DestroyIcon(hicon);
    }
    decoded
}

fn has_pixels_outside_top_left(icon: &DecodedIcon, threshold: u32) -> bool {
    if icon.width <= threshold && icon.height <= threshold {
        return true;
    }
    let w = icon.width as usize;
    let h = icon.height as usize;
    let t = threshold as usize;
    for y in 0..h {
        let row_start = y * w * 4;
        let x_start = if y < t { t.min(w) } else { 0 };
        for x in x_start..w {
            if icon.pbgra[row_start + x * 4 + 3] > 8 {
                return true;
            }
        }
    }
    false
}

fn load_sys_image_list_icon(path: &str, list_id: i32) -> Option<HICON> {
    let wpath = HSTRING::from(path);
    let mut sfi = SHFILEINFOW::default();
    unsafe {
        let res = SHGetFileInfoW(
            &wpath,
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        );
        if res != 0
            && let Ok(img_list) = SHGetImageList::<IImageList>(list_id)
            && let Ok(hicon) = img_list.GetIcon(sfi.iIcon, 0)
            && !hicon.is_invalid()
        {
            return Some(hicon);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_icon(tag: u8) -> DecodedIcon {
        DecodedIcon {
            width: 1,
            height: 1,
            pbgra: vec![tag, tag, tag, 255],
        }
    }

    #[test]
    fn icon_cache_lru_evicts_least_recently_used() {
        let mut cache = IconCache::with_capacity(2).unwrap();
        cache.insert_lru("a".to_string(), dummy_icon(1));
        cache.insert_lru("b".to_string(), dummy_icon(2));
        // Touch "a" so "b" becomes LRU.
        assert!(cache.cache.contains_key("a"));
        cache.access_seq += 1;
        cache.cache.get_mut("a").unwrap().last_used = cache.access_seq;

        cache.insert_lru("c".to_string(), dummy_icon(3));
        assert_eq!(cache.len(), 2);
        assert!(cache.cache.contains_key("a"));
        assert!(!cache.cache.contains_key("b"));
        assert!(cache.cache.contains_key("c"));
    }

    #[test]
    fn icon_cache_invalidate_path_removes_exact_entry() {
        let mut cache = IconCache::with_capacity(4).unwrap();
        cache.insert_lru("c:\\users\\test\\desktop\\foo.txt".to_string(), dummy_icon(1));
        cache.insert_lru("ext:.txt".to_string(), dummy_icon(2));

        cache.invalidate_path(Path::new("C:\\Users\\Test\\Desktop\\foo.txt"));
        assert!(!cache.cache.contains_key("c:\\users\\test\\desktop\\foo.txt"));
        assert!(cache.cache.contains_key("ext:.txt"));
    }
}
