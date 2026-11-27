use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::mem::size_of;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODT_MENU};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_ASYNCVERBSTATE, CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO,
    CMINVOKECOMMANDINFOEX, GCS_VERBA, GCS_VERBW, IContextMenu, IContextMenu2, IContextMenu3,
    ILFindLastID, ILFree, IShellFolder, SEE_MASK_UNICODE, SHBindToParent, SHGetDesktopFolder,
    SHParseDisplayName,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DeleteMenu, DestroyMenu, GetMenuItemCount, HMENU, InsertMenuW,
    MENU_ITEM_FLAGS, MF_BYPOSITION, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, SW_SHOWNORMAL,
    WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
};
use windows::core::{HSTRING, Interface, PCSTR, PCWSTR, PSTR};

use crate::Win32Error;

pub const SHELL_MENU_CMD_FIRST: u32 = 1;
pub const SHELL_MENU_CMD_LAST: u32 = 0x6FFF;
pub const CUSTOM_MENU_CMD_BASE: u32 = 0x7000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomMenuItem {
    Action { id: u32, label: String },
    Submenu { label: String, items: Vec<(u32, String)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HybridMenuOutcome {
    Cancelled,
    Custom(u32),
    InterceptedRename,
    InterceptedCopy { cut: bool },
    ShellInvoked { verb: Option<String> },
    ShellFailed { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellVerbInterception {
    InlineRename,
    Copy,
    Cut,
    None,
}

#[inline]
pub fn is_shell_menu_cmd_id(raw_cmd: u32) -> bool {
    (SHELL_MENU_CMD_FIRST..=SHELL_MENU_CMD_LAST).contains(&raw_cmd)
}

pub fn decode_utf16_until_nul(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

pub fn parse_canonical_verb(raw: &str) -> String {
    raw.trim_end_matches('\0').trim().to_ascii_lowercase()
}

pub fn classify_shell_verb(verb: Option<&str>) -> ShellVerbInterception {
    let Some(raw) = verb else {
        return ShellVerbInterception::None;
    };
    match parse_canonical_verb(raw).as_str() {
        "rename" => ShellVerbInterception::InlineRename,
        "copy" => ShellVerbInterception::Copy,
        "cut" => ShellVerbInterception::Cut,
        _ => ShellVerbInterception::None,
    }
}

pub fn paths_share_same_shell_parent(paths: &[String]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let first_virtual = paths[0].starts_with("::{");
    if first_virtual {
        return paths.iter().all(|p| p.starts_with("::{"));
    }
    let Some(first_parent) = Path::new(&paths[0]).parent() else {
        return false;
    };
    let expected = first_parent.to_string_lossy().to_ascii_lowercase();
    paths.iter().all(|p| {
        !p.starts_with("::{")
            && Path::new(p)
                .parent()
                .is_some_and(|parent| parent.to_string_lossy().to_ascii_lowercase() == expected)
    })
}

fn is_vk_down(vk: u16) -> bool {
    (unsafe { GetKeyState(i32::from(vk)) } as u16 & 0x8000) != 0
}

struct OwnedPidl(*mut ITEMIDLIST);

impl Drop for OwnedPidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ILFree(Some(self.0)) };
            self.0 = std::ptr::null_mut();
        }
    }
}

pub struct OwnedMenu(pub HMENU);

impl OwnedMenu {
    pub fn new() -> Option<Self> {
        unsafe { CreatePopupMenu() }.ok().map(Self)
    }

    pub fn release(mut self) -> HMENU {
        let h = self.0;
        self.0 = HMENU::default();
        h
    }
}

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DestroyMenu(self.0);
            }
            self.0 = HMENU::default();
        }
    }
}

struct LazyDesktopBgState {
    sub_hwnd: isize,
    owner_hwnd: HWND,
    loaded: bool,
    cm: Option<IContextMenu>,
}

struct ActiveShellMenuState {
    cm3: Option<IContextMenu3>,
    cm2: Option<IContextMenu2>,
    ignored_menus: HashSet<isize>,
    lazy_bg: Option<LazyDesktopBgState>,
}

thread_local! {
    static ACTIVE_SHELL_MENU: Cell<*mut ActiveShellMenuState> = const { Cell::new(std::ptr::null_mut()) };
}

pub struct ActiveShellMenuGuard {
    state: Box<ActiveShellMenuState>,
    prev: *mut ActiveShellMenuState,
}

impl ActiveShellMenuGuard {
    pub fn install(cm: &IContextMenu, ignored_menus: HashSet<isize>) -> Self {
        let cm3 = cm.cast::<IContextMenu3>().ok();
        let cm2 = if cm3.is_some() {
            None
        } else {
            cm.cast::<IContextMenu2>().ok()
        };
        let mut state = Box::new(ActiveShellMenuState {
            cm3,
            cm2,
            ignored_menus,
            lazy_bg: None,
        });
        let ptr: *mut ActiveShellMenuState = &mut *state;
        let prev = ACTIVE_SHELL_MENU.with(|cell| {
            let old = cell.get();
            cell.set(ptr);
            old
        });
        Self { state, prev }
    }

    pub fn install_lazy_desktop_bg(
        sub_hwnd: isize,
        owner_hwnd: HWND,
        ignored_menus: HashSet<isize>,
    ) -> Self {
        let mut state = Box::new(ActiveShellMenuState {
            cm3: None,
            cm2: None,
            ignored_menus,
            lazy_bg: Some(LazyDesktopBgState {
                sub_hwnd,
                owner_hwnd,
                loaded: false,
                cm: None,
            }),
        });
        let ptr: *mut ActiveShellMenuState = &mut *state;
        let prev = ACTIVE_SHELL_MENU.with(|cell| {
            let old = cell.get();
            cell.set(ptr);
            old
        });
        Self { state, prev }
    }

    pub fn take_lazy_context_menu(&mut self) -> Option<IContextMenu> {
        self.state.lazy_bg.as_mut().and_then(|bg| bg.cm.take())
    }
}

impl Drop for ActiveShellMenuGuard {
    fn drop(&mut self) {
        ACTIVE_SHELL_MENU.with(|cell| cell.set(self.prev));
    }
}

/// Forwards owner-drawn and lazy-submenu popup messages to an active `IContextMenu2`/`IContextMenu3`.
pub fn handle_active_shell_menu_msg(
    _hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    if !matches!(
        msg,
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR
    ) {
        return None;
    }
    let ptr = ACTIVE_SHELL_MENU.with(|cell| cell.get());
    if ptr.is_null() {
        return None;
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_INITMENUPOPUP => {
            let hmenu = wparam.0 as isize;
            if state.ignored_menus.contains(&hmenu) {
                return None;
            }
            if let Some(lazy) = state.lazy_bg.as_mut()
                && hmenu == lazy.sub_hwnd
                && !lazy.loaded
            {
                lazy.loaded = true;
                let sub = HMENU(hmenu as *mut _);
                let _ = unsafe { DeleteMenu(sub, 0, MF_BYPOSITION) };
                if let Some(cm) = acquire_desktop_background_context_menu(lazy.owner_hwnd) {
                    let mut flags = CMF_NORMAL | CMF_ASYNCVERBSTATE;
                    if is_vk_down(VK_SHIFT.0) {
                        flags |= CMF_EXTENDEDVERBS;
                    }
                    let hr = unsafe {
                        cm.QueryContextMenu(
                            sub,
                            0,
                            SHELL_MENU_CMD_FIRST,
                            SHELL_MENU_CMD_LAST,
                            flags,
                        )
                    };
                    if !hr.is_err() && unsafe { GetMenuItemCount(Some(sub)) } > 0 {
                        state.cm3 = cm.cast::<IContextMenu3>().ok();
                        state.cm2 = if state.cm3.is_some() {
                            None
                        } else {
                            cm.cast::<IContextMenu2>().ok()
                        };
                        lazy.cm = Some(cm);
                    }
                }
                if unsafe { GetMenuItemCount(Some(sub)) } <= 0 {
                    let empty_lbl = HSTRING::from("无可用系统桌面项");
                    let _ = unsafe { AppendMenuW(sub, MF_STRING | MF_GRAYED, 0, &empty_lbl) };
                }
            }
        }
        WM_DRAWITEM => {
            if lparam.0 == 0 {
                return None;
            }
            let dis = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if dis.CtlType != ODT_MENU || dis.itemID >= CUSTOM_MENU_CMD_BASE {
                return None;
            }
        }
        WM_MEASUREITEM => {
            if lparam.0 == 0 {
                return None;
            }
            let mis = unsafe { &*(lparam.0 as *const MEASUREITEMSTRUCT) };
            if mis.CtlType != ODT_MENU || mis.itemID >= CUSTOM_MENU_CMD_BASE {
                return None;
            }
        }
        _ => {}
    }

    if let Some(cm3) = &state.cm3 {
        let mut lres = LRESULT(0);
        if unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut lres)) }.is_ok() {
            if msg == WM_MENUCHAR && lres.0 == 0 {
                return None;
            }
            return Some(lres);
        }
    } else if let Some(cm2) = &state.cm2
        && msg != WM_MENUCHAR
        && unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok()
    {
        return Some(LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 }));
    }

    None
}

fn ensure_sta_com() {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

fn acquire_items_context_menu(owner_hwnd: HWND, paths: &[String]) -> Option<IContextMenu> {
    if !paths_share_same_shell_parent(paths) {
        return None;
    }
    ensure_sta_com();

    let mut pidls = Vec::with_capacity(paths.len());
    for p in paths {
        let mut raw_pidl = std::ptr::null_mut();
        let wp = HSTRING::from(p.as_str());
        if unsafe { SHParseDisplayName(&wp, None, &mut raw_pidl, 0, None) }.is_err()
            || raw_pidl.is_null()
        {
            return None;
        }
        pidls.push(OwnedPidl(raw_pidl));
    }

    let parent: IShellFolder = unsafe { SHBindToParent(pidls[0].0, None) }.ok()?;
    let mut child_pidls: Vec<*const ITEMIDLIST> = Vec::with_capacity(pidls.len());
    for pidl in &pidls {
        let last = unsafe { ILFindLastID(pidl.0) };
        if last.is_null() {
            return None;
        }
        child_pidls.push(last as *const ITEMIDLIST);
    }

    unsafe { parent.GetUIObjectOf::<IContextMenu>(owner_hwnd, &child_pidls, None) }.ok()
}

pub fn acquire_desktop_background_context_menu(owner_hwnd: HWND) -> Option<IContextMenu> {
    ensure_sta_com();
    let desktop = unsafe { SHGetDesktopFolder() }.ok()?;
    unsafe { desktop.CreateViewObject::<IContextMenu>(owner_hwnd) }.ok()
}

fn get_shell_command_verb(cm: &IContextMenu, id_cmd: u32) -> Option<String> {
    let mut buf_w = [0u16; 128];
    let hr_w = unsafe {
        cm.GetCommandString(
            id_cmd as usize,
            GCS_VERBW,
            None,
            PSTR(buf_w.as_mut_ptr() as *mut u8),
            buf_w.len() as u32,
        )
    };
    if hr_w.is_ok() {
        let s = parse_canonical_verb(&decode_utf16_until_nul(&buf_w));
        if !s.is_empty() {
            return Some(s);
        }
    }

    let mut buf_a = [0u8; 128];
    let hr_a = unsafe {
        cm.GetCommandString(
            id_cmd as usize,
            GCS_VERBA,
            None,
            PSTR(buf_a.as_mut_ptr()),
            buf_a.len() as u32,
        )
    };
    if hr_a.is_ok() {
        let len = buf_a.iter().position(|&b| b == 0).unwrap_or(buf_a.len());
        let s = parse_canonical_verb(&String::from_utf8_lossy(&buf_a[..len]));
        if !s.is_empty() {
            return Some(s);
        }
    }

    None
}

pub fn invoke_shell_context_command(
    cm: &IContextMenu,
    owner_hwnd: HWND,
    id_cmd: u32,
    x: i32,
    y: i32,
) -> Result<(), Win32Error> {
    let mut fmask = SEE_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    if is_vk_down(VK_SHIFT.0) {
        fmask |= CMIC_MASK_SHIFT_DOWN;
    }
    if is_vk_down(VK_CONTROL.0) {
        fmask |= CMIC_MASK_CONTROL_DOWN;
    }

    let ici = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: fmask,
        hwnd: owner_hwnd,
        lpVerb: PCSTR(id_cmd as usize as *const u8),
        lpVerbW: PCWSTR(id_cmd as usize as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x, y },
        ..Default::default()
    };

    unsafe { cm.InvokeCommand(&ici as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO) }
        .map_err(|source| Win32Error::Win {
            op: "IContextMenu::InvokeCommand",
            source,
        })
}

fn insert_custom_items_at_top(
    menu: HMENU,
    items: &[CustomMenuItem],
    add_trailing_separator: bool,
    ignored_menus: &mut HashSet<isize>,
) {
    if items.is_empty() {
        return;
    }
    let mut pos = 0u32;
    for item in items {
        match item {
            CustomMenuItem::Action { id, label } => {
                let wlabel = HSTRING::from(label.as_str());
                if unsafe { InsertMenuW(menu, pos, MF_BYPOSITION | MF_STRING, *id as usize, &wlabel) }
                    .is_ok()
                {
                    pos += 1;
                }
            }
            CustomMenuItem::Submenu { label, items: sub_items } => {
                let Some(sub) = OwnedMenu::new() else {
                    continue;
                };
                for &(sid, ref sname) in sub_items {
                    let wname = HSTRING::from(sname.as_str());
                    let _ = unsafe { AppendMenuW(sub.0, MF_STRING, sid as usize, &wname) };
                }
                let sub_handle = sub.0;
                let wlabel = HSTRING::from(label.as_str());
                if unsafe {
                    InsertMenuW(
                        menu,
                        pos,
                        MF_BYPOSITION | MF_POPUP,
                        sub_handle.0 as usize,
                        &wlabel,
                    )
                }
                .is_ok()
                {
                    ignored_menus.insert(sub_handle.0 as isize);
                    let _ = sub.release();
                    pos += 1;
                }
            }
        }
    }
    if add_trailing_separator && pos > 0 {
        let _ = unsafe { InsertMenuW(menu, pos, MF_BYPOSITION | MF_SEPARATOR, 0, None) };
    }
}

pub fn append_custom_items(
    menu: HMENU,
    items: &[CustomMenuItem],
    ignored_menus: &mut HashSet<isize>,
) {
    for item in items {
        match item {
            CustomMenuItem::Action { id, label } => {
                let wlabel = HSTRING::from(label.as_str());
                let _ = unsafe { AppendMenuW(menu, MF_STRING, *id as usize, &wlabel) };
            }
            CustomMenuItem::Submenu { label, items: sub_items } => {
                let Some(sub) = OwnedMenu::new() else {
                    continue;
                };
                for &(sid, ref sname) in sub_items {
                    let wname = HSTRING::from(sname.as_str());
                    let _ = unsafe { AppendMenuW(sub.0, MF_STRING, sid as usize, &wname) };
                }
                let sub_handle = sub.0;
                let wlabel = HSTRING::from(label.as_str());
                if unsafe { AppendMenuW(menu, MF_POPUP, sub_handle.0 as usize, &wlabel) }.is_ok() {
                    ignored_menus.insert(sub_handle.0 as isize);
                    let _ = sub.release();
                }
            }
        }
    }
}

pub fn append_menu_separator(menu: HMENU) {
    let _ = unsafe { AppendMenuW(menu, MENU_ITEM_FLAGS(MF_SEPARATOR.0), 0, None) };
}

struct PrefetchedShellMenu {
    paths: Vec<String>,
    shift_down: bool,
    built_at: Instant,
    menu: OwnedMenu,
    cm: IContextMenu,
}

thread_local! {
    static PREFETCHED_SHELL_MENU: RefCell<Option<PrefetchedShellMenu>> = const { RefCell::new(None) };
}

const PREFETCH_SHELL_MENU_TTL: Duration = Duration::from_millis(2500);

fn build_shell_items_menu_pair(
    invoke_hwnd: HWND,
    paths: &[String],
    shift_down: bool,
) -> Option<(OwnedMenu, Option<IContextMenu>)> {
    let menu = OwnedMenu::new()?;
    let cm_opt = acquire_items_context_menu(invoke_hwnd, paths);
    if let Some(cm) = &cm_opt {
        let mut flags = CMF_NORMAL | CMF_ASYNCVERBSTATE;
        if paths.len() == 1 {
            flags |= CMF_CANRENAME;
        }
        if shift_down {
            flags |= CMF_EXTENDEDVERBS;
        }
        let hr = unsafe {
            cm.QueryContextMenu(menu.0, 0, SHELL_MENU_CMD_FIRST, SHELL_MENU_CMD_LAST, flags)
        };
        if !hr.is_err() && unsafe { GetMenuItemCount(Some(menu.0)) } > 0 {
            return Some((menu, Some(cm.clone())));
        }
    }
    Some((menu, None))
}

/// Pre-builds the Windows Shell `IContextMenu` & `HMENU` on the current STA thread when an item
/// is single-clicked so a subsequent right-click pops up in <1ms without waiting for extensions.
pub fn prefetch_shell_items_menu(invoke_hwnd: HWND, paths: &[String]) {
    if !paths_share_same_shell_parent(paths) {
        clear_prefetched_shell_menu();
        return;
    }
    let shift_down = is_vk_down(VK_SHIFT.0);
    let already_fresh = PREFETCHED_SHELL_MENU.with(|cell| {
        cell.borrow().as_ref().is_some_and(|c| {
            c.paths == paths
                && c.shift_down == shift_down
                && c.built_at.elapsed() <= Duration::from_millis(1500)
        })
    });
    if already_fresh {
        return;
    }
    // Drop any previous cached menu before querying the new one.
    clear_prefetched_shell_menu();
    if let Some((menu, Some(cm))) = build_shell_items_menu_pair(invoke_hwnd, paths, shift_down) {
        PREFETCHED_SHELL_MENU.with(|cell| {
            *cell.borrow_mut() = Some(PrefetchedShellMenu {
                paths: paths.to_vec(),
                shift_down,
                built_at: Instant::now(),
                menu,
                cm,
            });
        });
    }
}

/// Releases any unused prefetched `IContextMenu`/`HMENU` entry on the current thread.
pub fn clear_prefetched_shell_menu() {
    PREFETCHED_SHELL_MENU.with(|cell| {
        let _ = cell.borrow_mut().take();
    });
}

/// Shows a hybrid Win32 context menu combining DeskLite actions at the top with full native Windows
/// Explorer `IContextMenu` (`IContextMenu2` / `IContextMenu3`) shell items below.
///
/// Falls back cleanly to `fallback_items` if `paths` span multiple directories or cannot be parsed.
pub fn show_hybrid_shell_items_menu(
    invoke_hwnd: HWND,
    paths: &[String],
    x: i32,
    y: i32,
    top_custom_items: &[CustomMenuItem],
    fallback_items: &[CustomMenuItem],
    track_popup: impl FnOnce(HMENU) -> u32,
) -> HybridMenuOutcome {
    let shift_down = is_vk_down(VK_SHIFT.0);
    let cached_hit = PREFETCHED_SHELL_MENU.with(|cell| {
        let taken = cell.borrow_mut().take();
        taken.filter(|c| {
            c.paths == paths
                && c.shift_down == shift_down
                && c.built_at.elapsed() <= PREFETCH_SHELL_MENU_TTL
        })
    });

    let (menu, cm_opt) = if let Some(cached) = cached_hit {
        (cached.menu, Some(cached.cm))
    } else if let Some(pair) = build_shell_items_menu_pair(invoke_hwnd, paths, shift_down) {
        pair
    } else {
        return HybridMenuOutcome::Cancelled;
    };

    let shell_ready = cm_opt.is_some();
    let mut ignored_menus = HashSet::new();

    if shell_ready {
        insert_custom_items_at_top(menu.0, top_custom_items, true, &mut ignored_menus);
    } else {
        let mut unused_ignored = HashSet::new();
        append_custom_items(menu.0, fallback_items, &mut unused_ignored);
    }

    let selected = {
        let _shell_guard = if shell_ready {
            cm_opt
                .as_ref()
                .map(|cm| ActiveShellMenuGuard::install(cm, ignored_menus))
        } else {
            None
        };
        track_popup(menu.0)
    };

    if selected == 0 {
        return HybridMenuOutcome::Cancelled;
    }
    if selected >= CUSTOM_MENU_CMD_BASE {
        return HybridMenuOutcome::Custom(selected);
    }

    if shell_ready
        && is_shell_menu_cmd_id(selected)
        && let Some(cm) = cm_opt
    {
        let id_cmd = selected - SHELL_MENU_CMD_FIRST;
        let verb = get_shell_command_verb(&cm, id_cmd);
        return match classify_shell_verb(verb.as_deref()) {
            ShellVerbInterception::InlineRename => HybridMenuOutcome::InterceptedRename,
            ShellVerbInterception::Copy => HybridMenuOutcome::InterceptedCopy { cut: false },
            ShellVerbInterception::Cut => HybridMenuOutcome::InterceptedCopy { cut: true },
            ShellVerbInterception::None => {
                match invoke_shell_context_command(&cm, invoke_hwnd, id_cmd, x, y) {
                    Ok(()) => HybridMenuOutcome::ShellInvoked { verb },
                    Err(err) => HybridMenuOutcome::ShellFailed {
                        error: err.to_string(),
                    },
                }
            }
        };
    }

    HybridMenuOutcome::Cancelled
}

/// Appends an initially-empty cascading placeholder submenu for Windows desktop background
/// items (`新建 >` / `显示设置` / `个性化`), populated lazily on `WM_INITMENUPOPUP` so
/// blank desktop right-click opens instantaneously without stalling on Shell COM extensions.
pub fn append_lazy_desktop_background_submenu(parent_menu: HMENU, label: &str) -> Option<isize> {
    let sub = OwnedMenu::new()?;
    let loading = HSTRING::from("加载中...");
    let _ = unsafe { AppendMenuW(sub.0, MF_STRING | MF_GRAYED, 0, &loading) };
    let sub_h = sub.0;
    let wlabel = HSTRING::from(label);
    if unsafe { AppendMenuW(parent_menu, MF_POPUP, sub_h.0 as usize, &wlabel) }.is_err() {
        return None;
    }
    let _ = sub.release();
    Some(sub_h.0 as isize)
}

/// Pre-warms Windows Shell `IContextMenu` COM extensions in the background without displaying a menu
/// so the user's first file right-click avoids cold DLL initialization stalls.
pub fn warmup_shell_context_menu(sample_path: &str) {
    let paths = [sample_path.to_string()];
    let Some(cm) = acquire_items_context_menu(HWND::default(), &paths) else {
        return;
    };
    let Some(menu) = OwnedMenu::new() else {
        return;
    };
    let _ = unsafe {
        cm.QueryContextMenu(
            menu.0,
            0,
            SHELL_MENU_CMD_FIRST,
            SHELL_MENU_CMD_LAST,
            CMF_NORMAL | CMF_ASYNCVERBSTATE,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_id_ranges_do_not_overlap() {
        assert!(!is_shell_menu_cmd_id(0));
        assert!(is_shell_menu_cmd_id(SHELL_MENU_CMD_FIRST));
        assert!(is_shell_menu_cmd_id(SHELL_MENU_CMD_LAST));
        assert!(!is_shell_menu_cmd_id(CUSTOM_MENU_CMD_BASE));
        const { assert!(SHELL_MENU_CMD_LAST < CUSTOM_MENU_CMD_BASE) };
    }

    #[test]
    fn shell_verb_classification_handles_rename_copy_cut_and_case() {
        assert_eq!(
            classify_shell_verb(Some("rename\0")),
            ShellVerbInterception::InlineRename
        );
        assert_eq!(
            classify_shell_verb(Some("  RENAME ")),
            ShellVerbInterception::InlineRename
        );
        assert_eq!(
            classify_shell_verb(Some("Copy")),
            ShellVerbInterception::Copy
        );
        assert_eq!(
            classify_shell_verb(Some("cut")),
            ShellVerbInterception::Cut
        );
        assert_eq!(
            classify_shell_verb(Some("properties")),
            ShellVerbInterception::None
        );
        assert_eq!(classify_shell_verb(None), ShellVerbInterception::None);
    }

    #[test]
    fn decode_utf16_until_nul_stops_at_first_terminator() {
        let raw: Vec<u16> = "rename\0junk".encode_utf16().collect();
        assert_eq!(decode_utf16_until_nul(&raw), "rename");
    }

    #[test]
    fn same_parent_detection_checks_physical_and_virtual_paths() {
        assert!(!paths_share_same_shell_parent(&[]));
        assert!(paths_share_same_shell_parent(&[
            r"C:\Users\Admin\Desktop\a.txt".to_string(),
            r"c:\users\admin\desktop\b.lnk".to_string(),
        ]));
        assert!(!paths_share_same_shell_parent(&[
            r"C:\Users\Admin\Desktop\a.txt".to_string(),
            r"C:\Users\Public\Desktop\b.lnk".to_string(),
        ]));
        assert!(paths_share_same_shell_parent(&[
            "::{645FF040-5081-101B-9F08-00AA002F954E}".to_string(),
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string(),
        ]));
        assert!(!paths_share_same_shell_parent(&[
            "::{645FF040-5081-101B-9F08-00AA002F954E}".to_string(),
            r"C:\Users\Admin\Desktop\a.txt".to_string(),
        ]));
    }
}
