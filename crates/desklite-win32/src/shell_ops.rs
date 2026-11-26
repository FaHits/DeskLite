use std::path::{Path, PathBuf};

use desklite_core::{FenceItem, ItemKind};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_NORMAL, MOVEFILE_COPY_ALLOWED, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_PublicDesktop, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
    FileOpenDialog, IFileOpenDialog, KNOWN_FOLDER_FLAG, SHCNE_UPDATEDIR, SHCNF_PATHW,
    SHChangeNotify, SHFILEINFOW, SHGFI_DISPLAYNAME, SHGetFileInfoW, SHGetKnownFolderPath,
    SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{GUID, HSTRING, w};

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

pub fn shell_display_name(path: &Path) -> Option<String> {
    query_shell_display_name(&path.to_string_lossy())
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

fn query_known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let psz = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = psz.to_string().unwrap_or_default();
        CoTaskMemFree(Some(psz.0.cast()));
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }
}

pub fn user_desktop_dir() -> Option<PathBuf> {
    query_known_folder(&FOLDERID_Desktop)
}

pub fn all_desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(ud) = query_known_folder(&FOLDERID_Desktop) {
        dirs.push(ud);
    }
    if let Some(pd) = query_known_folder(&FOLDERID_PublicDesktop)
        && !dirs
            .iter()
            .any(|d| normalize_path_key(d) == normalize_path_key(&pd))
    {
        dirs.push(pd);
    }
    dirs
}

pub fn desktop_vault_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("DeskLite").join("Vault")
}

fn normalize_path_key(p: &Path) -> String {
    p.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

pub fn is_vault_path(path: &Path) -> bool {
    let vault = normalize_path_key(&desktop_vault_dir());
    let cand = normalize_path_key(path);
    cand == vault || cand.starts_with(&format!("{vault}\\"))
}

pub fn is_directly_on_desktop(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let pkey = normalize_path_key(parent);
    if let Some(ud) = query_known_folder(&FOLDERID_Desktop)
        && pkey == normalize_path_key(&ud)
    {
        return true;
    }
    if let Some(pd) = query_known_folder(&FOLDERID_PublicDesktop)
        && pkey == normalize_path_key(&pd)
    {
        return true;
    }
    false
}

fn unique_dest_path(dir: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let name_path = Path::new(file_name);
    let stem = name_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let ext = name_path.extension().map(|e| e.to_string_lossy().into_owned());
    for idx in 2..1000 {
        let next_name = match ext {
            Some(ref e) => format!("{stem} ({idx}).{e}"),
            None => format!("{stem} ({idx})"),
        };
        let next = dir.join(next_name);
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!(
        "{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        file_name.to_string_lossy()
    ))
}

fn notify_shell_dir_updated(dir: &Path) {
    let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
    unsafe {
        SHChangeNotify(
            SHCNE_UPDATEDIR,
            SHCNF_PATHW,
            Some(wdir.as_ptr().cast()),
            None,
        );
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ftype = entry.file_type()?;
        let src_child = entry.path();
        let dst_child = dst.join(entry.file_name());
        if ftype.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "refusing cross-volume recursive move across symlink",
            ));
        } else if ftype.is_dir() {
            copy_dir_recursive(&src_child, &dst_child)?;
        } else {
            std::fs::copy(&src_child, &dst_child)?;
        }
    }
    Ok(())
}

fn move_path_cross_volume(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) => {
            let meta = std::fs::symlink_metadata(src)?;
            if meta.file_type().is_symlink() {
                return Err(err);
            }
            if meta.is_dir() {
                if let Err(e) = copy_dir_recursive(src, dst) {
                    let _ = std::fs::remove_dir_all(dst);
                    return Err(e);
                }
                std::fs::remove_dir_all(src)
            } else {
                let wsrc = HSTRING::from(src.to_string_lossy().as_ref());
                let wdst = HSTRING::from(dst.to_string_lossy().as_ref());
                unsafe {
                    MoveFileExW(
                        &wsrc,
                        &wdst,
                        MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH,
                    )
                }
                .map_err(|_| err)
            }
        }
    }
}

/// Restores any legacy Vault-stashed item back to `FOLDERID_Desktop`, returning the
/// restored path on success so Desktop files remain directly on Desktop.
pub fn migrate_vault_item_to_desktop(path: &Path) -> Option<PathBuf> {
    if !is_vault_path(path) || !path.exists() {
        return None;
    }
    let file_name = path.file_name()?;
    let desktop = user_desktop_dir()?;
    let dst = unique_dest_path(&desktop, file_name);
    match move_path_cross_volume(path, &dst) {
        Ok(()) => {
            notify_shell_dir_updated(&desktop);
            Some(dst)
        }
        Err(_) => None,
    }
}
