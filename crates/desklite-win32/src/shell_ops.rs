use std::path::Path;

use desklite_core::{FenceItem, ItemKind};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SHFILEINFOW,
    SHGFI_DISPLAYNAME, SHGetFileInfoW, SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::{Win32Error, win_err};

pub fn launch_item(path: &str) -> Result<(), Win32Error> {
    let wpath = HSTRING::from(path);
    let hinst = unsafe { ShellExecuteW(None, w!("open"), &wpath, None, None, SW_SHOWNORMAL) };
    if (hinst.0 as isize) <= 32 {
        return Err(Win32Error::Status {
            op: "ShellExecuteW",
            code: hinst.0 as usize as u32,
        });
    }
    Ok(())
}

pub fn reveal_in_explorer(path: &str) -> Result<(), Win32Error> {
    let args = HSTRING::from(format!("/select,\"{path}\""));
    let hinst =
        unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, None, SW_SHOWNORMAL) };
    if (hinst.0 as isize) <= 32 {
        return Err(Win32Error::Status {
            op: "ShellExecuteW(explorer /select)",
            code: hinst.0 as usize as u32,
        });
    }
    Ok(())
}

pub fn resolve_dropped_item(path: &Path, order: i32) -> FenceItem {
    let raw_path = path.to_string_lossy().into_owned();
    let kind = if raw_path.starts_with("::{") {
        ItemKind::VirtualShell
    } else if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        ItemKind::Shortcut
    } else if path.is_dir() {
        ItemKind::Folder
    } else {
        ItemKind::File
    };

    let display_name = query_shell_display_name(&raw_path).unwrap_or_else(|| {
        if matches!(kind, ItemKind::Shortcut) {
            path.file_stem()
                .or_else(|| path.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw_path.clone())
        } else {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw_path.clone())
        }
    });

    FenceItem {
        display_name,
        order,
        kind,
        path: raw_path,
    }
}

fn query_shell_display_name(path: &str) -> Option<String> {
    let wpath = HSTRING::from(path);
    let mut sfi = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            &wpath,
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_DISPLAYNAME,
        )
    };
    if ok == 0 {
        return None;
    }
    let end = sfi
        .szDisplayName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(sfi.szDisplayName.len());
    let name = String::from_utf16_lossy(&sfi.szDisplayName[..end]);
    if name.is_empty() { None } else { Some(name) }
}

pub fn scan_portal_directory(dir: &Path) -> Vec<FenceItem> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case("desktop.ini")
            || name.eq_ignore_ascii_case("thumbs.db")
            || name.starts_with("~$")
        {
            continue;
        }
        entries.push(path);
    }
    entries.sort_by(|a, b| {
        let a_dir = a.is_dir();
        let b_dir = b.is_dir();
        b_dir.cmp(&a_dir).then_with(|| {
            a.file_name()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .cmp(&b.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()))
        })
    });
    entries
        .into_iter()
        .enumerate()
        .map(|(i, p)| resolve_dropped_item(&p, i as i32))
        .collect()
}

pub fn pick_folder_dialog(owner: HWND) -> Result<Option<String>, Win32Error> {
    unsafe {
        let dlg: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| win_err("CoCreateInstance(FileOpenDialog)", e))?;
        let opts = dlg.GetOptions().map_err(|e| win_err("GetOptions", e))?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .map_err(|e| win_err("SetOptions", e))?;
        dlg.SetTitle(w!("选择要映射的文件夹"))
            .map_err(|e| win_err("SetTitle", e))?;
        if dlg.Show(Some(owner)).is_err() {
            return Ok(None);
        }
        let item = dlg.GetResult().map_err(|e| win_err("GetResult", e))?;
        let psz = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| win_err("GetDisplayName", e))?;
        let path = psz.to_string().unwrap_or_default();
        CoTaskMemFree(Some(psz.0.cast()));
        if path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(path))
        }
    }
}
