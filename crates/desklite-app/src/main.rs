#![windows_subsystem = "windows"]

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use desklite_core::{
    DEFAULT_BACKDROP_ALPHA, DisplayTopology, FenceItem, FenceKind, FenceRole, FenceSpec,
    MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, NativeLayoutConfig, Point, Rect, TopologyError,
    clamp_rect_to_monitors, load_native_layout, nearest_monitor, normalize_path_key,
    save_native_layout,
};
use desklite_legacy::{LegacyOptions, legacy_to_native, load_legacy_dir};
use desklite_ui::fence_view::{
    CollapsedMenuCommand, FenceCommand, FreeDesktopMenuCommand, FreeIconMenuCommand, RenamePrompt,
    apply_collapsed_menu_command, apply_free_desktop_menu_command, apply_free_icon_menu_command,
    format_fence_title,
};
use desklite_ui::state::lock_unpoisoned;
use desklite_ui::{
    COLLAPSED_HEIGHT, INSPECTOR_HEIGHT, INSPECTOR_TITLE, INSPECTOR_WIDTH, SharedFenceState,
    build_free_desktop_snapshot, configure_egui_theme, handle_free_desktop_event,
    install_chinese_fonts, pending_free_rename, render_fence_viewport, render_inspector_viewport,
};
use desklite_win32::{
    FolderWatcher, FreeDesktopController, FreeDesktopEvent, IconCache, TrayIcon,
    WM_APP_PORTAL_DIRTY, WM_APP_TRAY, all_desktop_dirs, clipboard_has_files,
    enable_dark_popup_menus, enable_per_monitor_dpi_v2, hide_all_fences_immediately,
    install_desktop_z_win_event_hooks, is_desktop_dir_path, is_display_wake_power_broadcast,
    is_vault_path, launch_item, migrate_vault_item_to_desktop, pick_folder_dialog,
    post_reassert_all_fences_z, probe_display_topology, register_power_and_session_notifications,
    restore_all_desktop_icons, scan_portal_directory, set_external_foreground_notify_target,
    set_shell_desktop_icons_visible, set_window_rect_no_z, strip_alt_tab_from_host,
    unregister_power_and_session_notifications,
};
use egui::{Pos2, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GWLP_USERDATA,
    KillTimer, MB_ICONINFORMATION, MB_OK, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
    MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SPI_SETWORKAREA, SW_SHOWNORMAL, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, TrackPopupMenuEx,
    TranslateMessage, WM_APP, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP, WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, w};

const DISPLAY_DEBOUNCE_TIMER: usize = 101;
const DISPLAY_DEBOUNCE_MS: u32 = 150;
const PORTAL_DEBOUNCE_TIMER: usize = 102;
const PORTAL_DEBOUNCE_MS: u32 = 160;
const SHELL_VERIFY_TIMER_1: usize = 103;
const SHELL_VERIFY_DELAY_1_MS: u32 = 250;
const SHELL_VERIFY_TIMER_2: usize = 104;
const SHELL_VERIFY_DELAY_2_MS: u32 = 700;
const TRANSIENT_RETRY_TIMER: usize = 105;
const TRANSIENT_RETRY_MS: u32 = 350;
const MAX_TRANSIENT_RETRIES: u8 = 8;

const WM_APP_SYNC_WATCHERS: u32 = WM_APP + 20;
const WM_APP_SHUTDOWN: u32 = WM_APP + 21;
const WM_APP_SHOW_FENCE_MENU: u32 = WM_APP + 22;
const WM_APP_SHOW_FENCE_MENU_BODY: u32 = WM_APP + 23;
const WM_APP_DRAG_DONE: u32 = WM_APP + 24;
const WM_APP_SHOW_FREE_DESKTOP_MENU: u32 = WM_APP + 25;
const WM_APP_SHOW_FREE_ICON_MENU: u32 = WM_APP + 26;
const WM_APP_SCHEDULE_SHELL_VERIFY: u32 = WM_APP + 27;
const WM_APP_FREE_DESKTOP_EVENT: u32 = WM_APP + 28;
const WM_APP_REFRESH_FREE_DESKTOP: u32 = WM_APP + 29;
const WM_APP_EXTERNAL_FOREGROUND: u32 = WM_APP + 30;

const TRAY_CMD_NEW_BLANK: u32 = 1001;
const TRAY_CMD_NEW_PORTAL: u32 = 1003;
const TRAY_CMD_TOGGLE_HIDE_ALL: u32 = 1004;
const TRAY_CMD_ARRANGE_NAME: u32 = 1005;
const TRAY_CMD_ARRANGE_KIND: u32 = 1006;
const TRAY_CMD_ARRANGE_MODIFIED: u32 = 1007;
const TRAY_CMD_REALIGN: u32 = 1008;
const TRAY_CMD_OPEN_CONFIG_DIR: u32 = 1009;
const TRAY_CMD_EXIT: u32 = 1010;

const FENCE_CMD_TOGGLE_COLLAPSE: u32 = 2001;
const FENCE_CMD_RENAME: u32 = 2002;
const FENCE_CMD_TOGGLE_KIND: u32 = 2003;
const FENCE_CMD_OPEN_PORTAL: u32 = 2004;
const FENCE_CMD_UNBIND_PORTAL: u32 = 2005;
const FENCE_CMD_BIND_PORTAL: u32 = 2006;
const FENCE_CMD_SORT_NAME: u32 = 2007;
const FENCE_CMD_SORT_KIND: u32 = 2008;
const FENCE_CMD_SORT_MODIFIED: u32 = 2009;
const FENCE_CMD_NEW_BLANK: u32 = 2010;
const FENCE_CMD_NEW_PORTAL: u32 = 2012;
const FENCE_CMD_DELETE: u32 = 2013;
const FENCE_CMD_EXIT: u32 = 2014;
const FENCE_CMD_TOGGLE_LOCK: u32 = 2015;
const FENCE_CMD_DUPLICATE_STYLE: u32 = 2016;
const FENCE_CMD_PARAMS_EDITOR: u32 = 2017;
const FENCE_CMD_PASTE: u32 = 2018;
const FENCE_CMD_UNDO: u32 = 2019;
const FENCE_CMD_SELECT_ALL: u32 = 2020;
const FENCE_CMD_REFRESH_PORTAL: u32 = 2021;

const FREE_CMD_NEW_BLANK: u32 = 3001;
const FREE_CMD_NEW_PORTAL: u32 = 3003;
const FREE_CMD_TOGGLE_HIDE_ALL: u32 = 3004;
const FREE_CMD_ARRANGE_NAME: u32 = 3005;
const FREE_CMD_ARRANGE_KIND: u32 = 3006;
const FREE_CMD_ARRANGE_MODIFIED: u32 = 3007;
const FREE_CMD_REFRESH_DESKTOP: u32 = 3008;
const FREE_CMD_REALIGN: u32 = 3009;
const FREE_CMD_PASTE: u32 = 3010;
const FREE_CMD_UNDO: u32 = 3011;
const FREE_CMD_OPEN_DESKTOP_DIR: u32 = 3012;
const FREE_CMD_OPEN_CONFIG_DIR: u32 = 3013;
const FREE_CMD_EXIT: u32 = 3014;

const FREE_ICON_CMD_OPEN: u32 = 4001;
const FREE_ICON_CMD_OPEN_ADMIN: u32 = 4002;
const FREE_ICON_CMD_REVEAL: u32 = 4003;
const FREE_ICON_CMD_COPY: u32 = 4004;
const FREE_ICON_CMD_CUT: u32 = 4005;
const FREE_ICON_CMD_COPY_PATHS: u32 = 4006;
const FREE_ICON_CMD_RENAME: u32 = 4007;
const FREE_ICON_CMD_TRASH: u32 = 4008;
const FREE_ICON_CMD_PROPERTIES: u32 = 4009;
const FREE_ICON_CMD_MOVE_BASE: u32 = 4100;

struct SingleInstanceGuard(HANDLE);
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct MenuPopupActiveGuard<'a>(&'a Arc<Mutex<SharedFenceState>>);
impl<'a> MenuPopupActiveGuard<'a> {
    fn enter(shared: &'a Arc<Mutex<SharedFenceState>>) -> Self {
        lock_unpoisoned(shared).menu_popup_active = true;
        Self(shared)
    }
}
impl Drop for MenuPopupActiveGuard<'_> {
    fn drop(&mut self) {
        lock_unpoisoned(self.0).menu_popup_active = false;
    }
}

fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    let handle =
        unsafe { CreateMutexW(None, false, w!("Local\\DeskLiteAppSingleInstance")) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(SingleInstanceGuard(handle))
}

fn install_panic_restore_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_all_desktop_icons();
        prev(info);
    }));
}

fn default_layout_path() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("DeskLite").join("layout.json")
}

fn default_vault_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("DeskLite").join("Vault")
}

fn default_deskgo_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("Tencent").join("DeskGo")
}

fn desktop_dir_strings() -> Vec<String> {
    all_desktop_dirs()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn scan_all_desktop_items(desktop_dirs: &[String]) -> Vec<FenceItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for dir in desktop_dirs {
        for mut item in scan_portal_directory(Path::new(dir)) {
            let key = normalize_path_key(&item.path);
            if !key.is_empty() && seen.insert(key) {
                item.order = items.len() as i32;
                items.push(item);
            }
        }
    }
    items
}

fn refresh_all_portals(cfg: &mut NativeLayoutConfig, desktop_dirs: &[String]) {
    for f in &mut cfg.fences {
        if let Some(dir) = f.external_portal_dir(desktop_dirs) {
            f.items = scan_portal_directory(Path::new(&dir));
        }
    }
}

fn reconcile_free_icons_in_config(
    cfg: &mut NativeLayoutConfig,
    topo: &DisplayTopology,
    desktop_dirs: &[String],
    scanned_desktop: &[FenceItem],
) -> usize {
    let cell = (
        cfg.preferences.icon_cell_w.clamp(48, 180),
        cfg.preferences.icon_cell_h.clamp(56, 200),
    );
    let origin = topo
        .monitors()
        .first()
        .map(|m| Point {
            x: m.work_area.left + 20,
            y: m.work_area.top + 20,
        })
        .unwrap_or(Point { x: 20, y: 20 });
    cfg.reconcile_free_icons(scanned_desktop, origin, cell, topo.monitors(), desktop_dirs)
        .unwrap_or(0)
}

fn create_starter_fence(topo: &DisplayTopology) -> FenceSpec {
    let work = topo.primary().work_area;
    let right = (work.right - 24).max(work.left + 320);
    let left = (right - 300).max(work.left);
    FenceSpec {
        id: 101,
        name: "桌面整理".to_string(),
        kind: FenceKind::IconGrid,
        rect: Rect::new(left, work.top + 24, right, (work.top + 360).min(work.bottom)),
        collapsed: false,
        locked: false,
        role: Some(FenceRole::DesktopBucket),
        portal_dir: None,
        backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
        layout_override: None,
        items: Vec::new(),
    }
}

/// One-time startup cleanup for legacy `%APPDATA%\DeskLite\Vault` residue.
/// Moves any remaining files back onto UserDesktop and removes the empty Vault folder.
fn cleanup_legacy_vault_residue(cfg: Option<&mut NativeLayoutConfig>) -> bool {
    let mut changed = false;
    if let Some(cfg) = cfg {
        for f in &mut cfg.fences {
            if f.portal_dir.is_some() {
                continue;
            }
            for it in &mut f.items {
                let p = Path::new(&it.path);
                if is_vault_path(p)
                    && let Some(restored) = migrate_vault_item_to_desktop(p)
                {
                    it.path = restored.to_string_lossy().into_owned();
                    changed = true;
                }
            }
        }
    }

    let vault_dir = default_vault_dir();
    if vault_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&vault_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if migrate_vault_item_to_desktop(&path).is_some() {
                    changed = true;
                }
            }
        }
        if let Ok(mut remaining) = std::fs::read_dir(&vault_dir)
            && remaining.next().is_none()
        {
            let _ = std::fs::remove_dir(&vault_dir);
        }
    }
    changed
}

fn save_config_logged(layout_path: &Path, cfg: &NativeLayoutConfig, context: &str) {
    if let Err(err) = save_native_layout(layout_path, cfg) {
        eprintln!(
            "[desklite] save_native_layout failed during {context} ({}): {err}",
            layout_path.display()
        );
    }
}

fn save_shared_now(layout_path: &Path, shared: &Arc<Mutex<SharedFenceState>>, context: &str) {
    let mut guard = lock_unpoisoned(shared);
    save_config_logged(layout_path, &guard.config, context);
    guard.dirty_layout = false;
    guard.pending_save_at = None;
}

fn load_or_bootstrap_layout(layout_path: &Path, topo: &DisplayTopology) -> NativeLayoutConfig {
    let desktop_dirs = desktop_dir_strings();
    let scanned_desktop = scan_all_desktop_items(&desktop_dirs);

    if layout_path.exists()
        && let Ok(mut cfg) = load_native_layout(layout_path)
    {
        let sanitized = cfg.sanitize_legacy_containers();
        let vault_cleaned = cleanup_legacy_vault_residue(Some(&mut cfg));
        cfg.normalize_roles(&desktop_dirs);
        let deduped = cfg.enforce_desktop_single_ownership(&desktop_dirs);
        if cfg.fences.is_empty() {
            cfg.fences.push(create_starter_fence(topo));
        }
        if let Err(err) = cfg.apply_topology(topo, (72, 72)) {
            eprintln!("[desklite] apply_topology failed at startup: {err}");
        }
        refresh_all_portals(&mut cfg, &desktop_dirs);
        let reconciled =
            reconcile_free_icons_in_config(&mut cfg, topo, &desktop_dirs, &scanned_desktop);
        if sanitized || vault_cleaned || deduped > 0 || reconciled > 0 {
            save_config_logged(layout_path, &cfg, "startup_load");
        }
        return cfg;
    }

    let _ = cleanup_legacy_vault_residue(None);

    let deskgo_dir = default_deskgo_dir();
    if deskgo_dir.join("FencesDataFile.dat").exists()
        && let Ok(bundle) = load_legacy_dir(&deskgo_dir, &LegacyOptions::default())
        && let Ok(mut cfg) = legacy_to_native(&bundle, Some(topo))
    {
        let _ = cfg.sanitize_legacy_containers();
        cfg.normalize_roles(&desktop_dirs);
        let _ = cfg.enforce_desktop_single_ownership(&desktop_dirs);
        if cfg.fences.is_empty() {
            cfg.fences.push(create_starter_fence(topo));
        }
        if let Err(err) = cfg.apply_topology(topo, (72, 72)) {
            eprintln!("[desklite] apply_topology failed during legacy import: {err}");
        }
        refresh_all_portals(&mut cfg, &desktop_dirs);
        let _ = reconcile_free_icons_in_config(&mut cfg, topo, &desktop_dirs, &scanned_desktop);
        save_config_logged(layout_path, &cfg, "legacy_import");
        return cfg;
    }

    let mut cfg = NativeLayoutConfig::default();
    cfg.fences.push(create_starter_fence(topo));
    if let Err(err) = cfg.apply_topology(topo, (72, 72)) {
        eprintln!("[desklite] apply_topology failed during bootstrap: {err}");
    }
    let _ = reconcile_free_icons_in_config(&mut cfg, topo, &desktop_dirs, &scanned_desktop);
    save_config_logged(layout_path, &cfg, "bootstrap");
    cfg
}

fn sync_topology_and_save(
    layout_path: &Path,
    shared: &Arc<Mutex<SharedFenceState>>,
    context: &str,
) {
    let probed = probe_display_topology();
    let mut guard = lock_unpoisoned(shared);
    if let Ok(topo) = probed {
        guard.topology = topo.clone();
        if let Err(err) = guard.config.apply_topology(&topo, (72, 72)) {
            eprintln!("[desklite] apply_topology failed during {context}: {err}");
        }
        guard.sync_builder_rects_from_config();
    }
    save_config_logged(layout_path, &guard.config, context);
    guard.dirty_layout = false;
    guard.pending_save_at = None;
}

fn persist_and_restore_on_exit(
    layout_path: &Path,
    shared: &Arc<Mutex<SharedFenceState>>,
    context: &str,
) {
    sync_topology_and_save(layout_path, shared, context);
    restore_all_desktop_icons();
}

fn move_live_windows_to_topology(
    topo: &DisplayTopology,
    fences: &[FenceSpec],
    hwnd_to_fence: &HashMap<isize, i32>,
) {
    let monitors = topo.monitors();
    for (&raw_hwnd, &fid) in hwnd_to_fence {
        if raw_hwnd == 0 {
            continue;
        }
        if let Some(spec) = fences.iter().find(|f| f.id == fid) {
            let mon_scale = if monitors.is_empty() {
                1.0
            } else {
                (nearest_monitor(spec.rect.center(), monitors).dpi_x as f32 / 96.0).max(0.5)
            };
            let w = spec.rect.width().max(MIN_FENCE_WIDTH);
            let h = if spec.collapsed {
                (COLLAPSED_HEIGHT * mon_scale).round() as i32
            } else {
                spec.rect.height().max(MIN_FENCE_HEIGHT)
            };
            set_window_rect_no_z(raw_hwnd, spec.rect.left, spec.rect.top, w, h);
        }
    }
}

fn open_dir_in_explorer(dir: &Path) {
    let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &wdir, None, None, SW_SHOWNORMAL);
    }
}

fn show_desktop_portal_forbidden_dialog() {
    unsafe {
        let _ = MessageBoxW(
            None,
            w!("桌面目录已由自由桌面与桌面格子直接管理，不能作为外部文件夹映射。"),
            w!("DeskLite"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn is_relevant_setting_change(wparam: WPARAM, lparam: LPARAM) -> bool {
    if wparam.0 == SPI_SETWORKAREA.0 as usize {
        return true;
    }
    if lparam.0 != 0 {
        let ptr = lparam.0 as *const u16;
        let mut len = 0usize;
        while len < 64 && unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
        return matches!(
            s.as_str(),
            "TraySettings" | "ShellState" | "WindowMetrics" | "Desktop"
        );
    }
    false
}

#[derive(Debug, Clone)]
enum BgEvent {
    TopologyChanged(DisplayTopology),
    PortalDirty(i32),
    DesktopDirty,
    TrayNewBlank,
    TrayNewPortal(String),
    TrayRealign,
    ToggleHideAllFences,
    BindPortalResult(i32, String),
    FenceMenuCommand(i32, CollapsedMenuCommand),
    FreeDesktopMenuCmd(FreeDesktopMenuCommand),
    FreeIconMenuCmd(Vec<String>, FreeIconMenuCommand),
    ExternalForeground,
    FlushAndExit,
}

static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

struct HelperState {
    hwnd: HWND,
    layout_path: PathBuf,
    shared: Arc<Mutex<SharedFenceState>>,
    free_desktop_ctrl: Arc<Mutex<Option<FreeDesktopController>>>,
    free_icon_menu_paths: Arc<Mutex<Vec<String>>>,
    tx: Sender<BgEvent>,
    egui_ctx: egui::Context,
    tray: Option<TrayIcon>,
    taskbar_created_msg: u32,
    watchers: HashMap<i32, FolderWatcher>,
    last_display_event: Instant,
    transient_retries: u8,
    dirty_desktop: bool,
    dirty_portals: HashSet<i32>,
    shutting_down: bool,
}

impl HelperState {
    fn rebuild_watchers(&mut self) {
        self.watchers.clear();
        let portals: Vec<(i32, String)> = {
            let guard = lock_unpoisoned(&self.shared);
            let desktop_dirs = &guard.desktop_dirs;
            guard
                .config
                .fences
                .iter()
                .filter_map(|f| f.external_portal_dir(desktop_dirs).map(|d| (f.id, d)))
                .collect()
        };
        for (id, dir) in portals {
            if let Some(w) = FolderWatcher::start(id, Path::new(&dir), self.hwnd) {
                self.watchers.insert(id, w);
            }
        }
        if let Some(w) = FolderWatcher::start_desktop(-100, self.hwnd) {
            self.watchers.insert(-100, w);
        }
    }

    fn push_free_desktop_snapshot_if_dirty(&self) {
        let snap_opt = {
            let mut guard = lock_unpoisoned(&self.shared);
            if std::mem::take(&mut guard.pending_repaint_free_desktop) {
                Some(build_free_desktop_snapshot(&guard))
            } else {
                None
            }
        };
        if let Some(snap) = snap_opt
            && let Some(ctrl) = lock_unpoisoned(&self.free_desktop_ctrl).as_ref().cloned()
        {
            ctrl.update_snapshot(snap);
        }
    }

    fn schedule_display_reprobe(&mut self) {
        self.last_display_event = Instant::now();
        self.transient_retries = 0;
        lock_unpoisoned(&self.shared).begin_topology_settling(Duration::from_millis(3500));
        let _ = unsafe { KillTimer(Some(self.hwnd), TRANSIENT_RETRY_TIMER) };
        unsafe { SetTimer(Some(self.hwnd), DISPLAY_DEBOUNCE_TIMER, DISPLAY_DEBOUNCE_MS, None) };
    }

    fn schedule_shell_reverify(&self) {
        if self.shutting_down {
            return;
        }
        unsafe {
            SetTimer(
                Some(self.hwnd),
                SHELL_VERIFY_TIMER_1,
                SHELL_VERIFY_DELAY_1_MS,
                None,
            );
            SetTimer(
                Some(self.hwnd),
                SHELL_VERIFY_TIMER_2,
                SHELL_VERIFY_DELAY_2_MS,
                None,
            );
        }
    }

    fn try_probe_topology_and_notify(&mut self) {
        let _ = unsafe { KillTimer(Some(self.hwnd), DISPLAY_DEBOUNCE_TIMER) };
        match probe_display_topology() {
            Ok(topo) => {
                self.transient_retries = 0;
                let _ = unsafe { KillTimer(Some(self.hwnd), TRANSIENT_RETRY_TIMER) };
                let _ = self.tx.send(BgEvent::TopologyChanged(topo));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            Err(desklite_win32::Win32Error::Topology(TopologyError::EmptyTransient)) => {
                if self.transient_retries < MAX_TRANSIENT_RETRIES {
                    self.transient_retries += 1;
                    unsafe {
                        SetTimer(
                            Some(self.hwnd),
                            TRANSIENT_RETRY_TIMER,
                            TRANSIENT_RETRY_MS,
                            None,
                        );
                    }
                }
            }
            Err(err) => {
                eprintln!("[desklite] probe_display_topology failed: {err}");
            }
        }
    }

    fn spawn_folder_picker_for_new_portal(&self) {
        let tx = self.tx.clone();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if let Ok(Some(dir)) = pick_folder_dialog(HWND::default()) {
                if is_desktop_dir_path(Path::new(&dir)) {
                    show_desktop_portal_forbidden_dialog();
                } else {
                    let _ = tx.send(BgEvent::TrayNewPortal(dir));
                    ctx.request_repaint_of(ViewportId::ROOT);
                }
            }
            unsafe {
                CoUninitialize();
            }
        });
    }

    fn show_tray_menu(&self) {
        let hide_all = lock_unpoisoned(&self.shared).config.hide_all_fences;
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_NEW_BLANK as usize, w!("新建空白格子"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_NEW_PORTAL as usize,
                w!("新建外部文件夹格子..."),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let hide_flags = if hide_all {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(
                menu,
                hide_flags,
                TRAY_CMD_TOGGLE_HIDE_ALL as usize,
                w!("隐藏全部桌面格子"),
            );
            if let Ok(arrange) = CreatePopupMenu() {
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    TRAY_CMD_ARRANGE_NAME as usize,
                    w!("按名称排列"),
                );
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    TRAY_CMD_ARRANGE_KIND as usize,
                    w!("按类型排列"),
                );
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    TRAY_CMD_ARRANGE_MODIFIED as usize,
                    w!("按修改时间排列"),
                );
                let _ = AppendMenuW(menu, MF_POPUP, arrange.0 as usize, w!("排列自由图标"));
            }
            let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_REALIGN as usize, w!("立即重排校准"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_OPEN_CONFIG_DIR as usize,
                w!("打开配置目录"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_EXIT as usize,
                w!("退出并恢复原生桌面"),
            );
        }

        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let cmd = {
            let _popup_guard = MenuPopupActiveGuard::enter(&self.shared);
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                TrackPopupMenuEx(
                    menu,
                    (TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD).0,
                    pt.x,
                    pt.y,
                    self.hwnd,
                    None,
                )
            }
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }

        match cmd.0 as u32 {
            TRAY_CMD_NEW_BLANK => {
                let _ = self.tx.send(BgEvent::TrayNewBlank);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_NEW_PORTAL => {
                self.spawn_folder_picker_for_new_portal();
            }
            TRAY_CMD_TOGGLE_HIDE_ALL => {
                let _ = self.tx.send(BgEvent::ToggleHideAllFences);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_ARRANGE_NAME => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::ArrangeByName));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_ARRANGE_KIND => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::ArrangeByKind));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_ARRANGE_MODIFIED => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::ArrangeByModified,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_REALIGN => {
                let _ = self.tx.send(BgEvent::TrayRealign);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_OPEN_CONFIG_DIR => {
                if let Some(dir) = self.layout_path.parent() {
                    open_dir_in_explorer(dir);
                }
            }
            TRAY_CMD_EXIT => {
                let _ = self.tx.send(BgEvent::FlushAndExit);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            _ => {}
        }
    }

    fn show_fence_menu(&self, fence_id: i32, x: i32, y: i32, include_body_actions: bool) {
        let info = {
            let guard = lock_unpoisoned(&self.shared);
            let has_undo = !guard.undo_stack.is_empty();
            let desktop_dirs = &guard.desktop_dirs;
            guard.config.fences.iter().find(|f| f.id == fence_id).map(|f| {
                let is_ext_portal = f.external_portal_dir(desktop_dirs).is_some();
                let is_bucket = f.is_desktop_bucket(desktop_dirs);
                (
                    f.collapsed,
                    f.locked,
                    is_ext_portal,
                    matches!(f.kind, FenceKind::FileList),
                    is_bucket && f.items.len() > 1,
                    !f.items.is_empty(),
                    has_undo,
                )
            })
        };
        let Some((collapsed, locked, is_ext_portal, is_list, can_sort, has_items, has_undo)) = info
        else {
            return;
        };
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            if include_body_actions {
                let paste_flags = if clipboard_has_files() {
                    MF_STRING
                } else {
                    MF_STRING | MF_GRAYED
                };
                let _ = AppendMenuW(menu, paste_flags, FENCE_CMD_PASTE as usize, w!("粘贴\tCtrl+V"));
                let undo_flags = if has_undo {
                    MF_STRING
                } else {
                    MF_STRING | MF_GRAYED
                };
                let _ = AppendMenuW(menu, undo_flags, FENCE_CMD_UNDO as usize, w!("撤销\tCtrl+Z"));
                if has_items {
                    let _ = AppendMenuW(
                        menu,
                        MF_STRING,
                        FENCE_CMD_SELECT_ALL as usize,
                        w!("全选\tCtrl+A"),
                    );
                }
                if is_ext_portal {
                    let _ = AppendMenuW(
                        menu,
                        MF_STRING,
                        FENCE_CMD_REFRESH_PORTAL as usize,
                        w!("刷新\tF5"),
                    );
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            }
            let collapse_txt = if collapsed { w!("展开格子") } else { w!("折叠格子") };
            let _ = AppendMenuW(menu, MF_STRING, FENCE_CMD_TOGGLE_COLLAPSE as usize, collapse_txt);
            let _ = AppendMenuW(menu, MF_STRING, FENCE_CMD_RENAME as usize, w!("重命名格子"));
            let view_txt = if is_list {
                w!("切换为图标网格")
            } else {
                w!("切换为紧凑列表")
            };
            let _ = AppendMenuW(menu, MF_STRING, FENCE_CMD_TOGGLE_KIND as usize, view_txt);
            let lock_flags = if locked {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(
                menu,
                lock_flags,
                FENCE_CMD_TOGGLE_LOCK as usize,
                w!("锁定位置与大小"),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FENCE_CMD_PARAMS_EDITOR as usize,
                w!("格子参数设置..."),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FENCE_CMD_DUPLICATE_STYLE as usize,
                w!("复制相同样式格子"),
            );
            if is_ext_portal {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FENCE_CMD_OPEN_PORTAL as usize,
                    w!("在资源管理器中打开文件夹"),
                );
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FENCE_CMD_UNBIND_PORTAL as usize,
                    w!("取消文件夹映射"),
                );
            } else {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FENCE_CMD_BIND_PORTAL as usize,
                    w!("绑定外部文件夹映射..."),
                );
            }
            if can_sort
                && let Ok(sub) = CreatePopupMenu()
            {
                let _ = AppendMenuW(sub, MF_STRING, FENCE_CMD_SORT_NAME as usize, w!("按名称排序"));
                let _ = AppendMenuW(sub, MF_STRING, FENCE_CMD_SORT_KIND as usize, w!("按类型排序"));
                let _ = AppendMenuW(
                    sub,
                    MF_STRING,
                    FENCE_CMD_SORT_MODIFIED as usize,
                    w!("按修改时间排序"),
                );
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("排序方式"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, FENCE_CMD_NEW_BLANK as usize, w!("新建空白格子"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FENCE_CMD_NEW_PORTAL as usize,
                w!("新建外部文件夹格子..."),
            );
            let _ = AppendMenuW(menu, MF_STRING, FENCE_CMD_DELETE as usize, w!("删除此格子"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FENCE_CMD_EXIT as usize,
                w!("退出并恢复原生桌面"),
            );
        }

        let cmd = {
            let _popup_guard = MenuPopupActiveGuard::enter(&self.shared);
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                TrackPopupMenuEx(
                    menu,
                    (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0,
                    x,
                    y,
                    self.hwnd,
                    None,
                )
            }
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }

        let action = match cmd.0 as u32 {
            FENCE_CMD_TOGGLE_COLLAPSE => Some(CollapsedMenuCommand::ToggleCollapse),
            FENCE_CMD_RENAME => Some(CollapsedMenuCommand::RenameFence),
            FENCE_CMD_TOGGLE_KIND => Some(CollapsedMenuCommand::ToggleKind),
            FENCE_CMD_TOGGLE_LOCK => Some(CollapsedMenuCommand::ToggleLock),
            FENCE_CMD_DUPLICATE_STYLE => Some(CollapsedMenuCommand::DuplicateStyle),
            FENCE_CMD_PARAMS_EDITOR => Some(CollapsedMenuCommand::OpenParamsEditor),
            FENCE_CMD_OPEN_PORTAL => Some(CollapsedMenuCommand::OpenPortalFolder),
            FENCE_CMD_UNBIND_PORTAL => Some(CollapsedMenuCommand::UnbindPortal),
            FENCE_CMD_BIND_PORTAL => Some(CollapsedMenuCommand::BindPortal),
            FENCE_CMD_SORT_NAME => Some(CollapsedMenuCommand::SortName),
            FENCE_CMD_SORT_KIND => Some(CollapsedMenuCommand::SortKind),
            FENCE_CMD_SORT_MODIFIED => Some(CollapsedMenuCommand::SortModified),
            FENCE_CMD_PASTE => Some(CollapsedMenuCommand::PasteFromClipboard),
            FENCE_CMD_UNDO => Some(CollapsedMenuCommand::UndoLast),
            FENCE_CMD_SELECT_ALL => Some(CollapsedMenuCommand::SelectAll),
            FENCE_CMD_REFRESH_PORTAL => Some(CollapsedMenuCommand::RefreshPortal),
            FENCE_CMD_NEW_BLANK => Some(CollapsedMenuCommand::CreateBlank),
            FENCE_CMD_NEW_PORTAL => Some(CollapsedMenuCommand::CreatePortal),
            FENCE_CMD_DELETE => Some(CollapsedMenuCommand::DeleteFence),
            FENCE_CMD_EXIT => Some(CollapsedMenuCommand::ExitApp),
            _ => None,
        };
        if let Some(action) = action {
            let _ = self.tx.send(BgEvent::FenceMenuCommand(fence_id, action));
            self.egui_ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    fn show_free_desktop_menu(&self, x: i32, y: i32) {
        let (hide_all, has_undo) = {
            let guard = lock_unpoisoned(&self.shared);
            (guard.config.hide_all_fences, !guard.undo_stack.is_empty())
        };
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, FREE_CMD_NEW_BLANK as usize, w!("新建空白格子"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_CMD_NEW_PORTAL as usize,
                w!("新建外部文件夹格子..."),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let hide_flags = if hide_all {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(
                menu,
                hide_flags,
                FREE_CMD_TOGGLE_HIDE_ALL as usize,
                w!("隐藏全部桌面格子"),
            );
            if let Ok(arrange) = CreatePopupMenu() {
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    FREE_CMD_ARRANGE_NAME as usize,
                    w!("按名称排列"),
                );
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    FREE_CMD_ARRANGE_KIND as usize,
                    w!("按类型排列"),
                );
                let _ = AppendMenuW(
                    arrange,
                    MF_STRING,
                    FREE_CMD_ARRANGE_MODIFIED as usize,
                    w!("按修改时间排列"),
                );
                let _ = AppendMenuW(menu, MF_POPUP, arrange.0 as usize, w!("排列自由图标"));
            }
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_CMD_REFRESH_DESKTOP as usize,
                w!("刷新桌面\tF5"),
            );
            let _ = AppendMenuW(menu, MF_STRING, FREE_CMD_REALIGN as usize, w!("立即重排校准"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let paste_flags = if clipboard_has_files() {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            };
            let _ = AppendMenuW(menu, paste_flags, FREE_CMD_PASTE as usize, w!("粘贴到桌面\tCtrl+V"));
            let undo_flags = if has_undo {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            };
            let _ = AppendMenuW(menu, undo_flags, FREE_CMD_UNDO as usize, w!("撤销\tCtrl+Z"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_CMD_OPEN_DESKTOP_DIR as usize,
                w!("打开桌面目录"),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_CMD_OPEN_CONFIG_DIR as usize,
                w!("打开配置目录"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_CMD_EXIT as usize,
                w!("退出并恢复原生桌面"),
            );
        }

        let cmd = {
            let _popup_guard = MenuPopupActiveGuard::enter(&self.shared);
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                TrackPopupMenuEx(
                    menu,
                    (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0,
                    x,
                    y,
                    self.hwnd,
                    None,
                )
            }
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }

        match cmd.0 as u32 {
            FREE_CMD_NEW_BLANK => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::CreateBlank));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_NEW_PORTAL => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::CreatePortal));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_TOGGLE_HIDE_ALL => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::ToggleHideAllFences,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_ARRANGE_NAME => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::ArrangeByName,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_ARRANGE_KIND => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::ArrangeByKind,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_ARRANGE_MODIFIED => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::ArrangeByModified,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_REFRESH_DESKTOP => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::RefreshDesktop,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_REALIGN => {
                let _ = self.tx.send(BgEvent::TrayRealign);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_PASTE => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::PasteToDesktop,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_UNDO => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::UndoLast));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_OPEN_DESKTOP_DIR => {
                let _ = self.tx.send(BgEvent::FreeDesktopMenuCmd(
                    FreeDesktopMenuCommand::OpenDesktopFolder,
                ));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            FREE_CMD_OPEN_CONFIG_DIR => {
                if let Some(dir) = self.layout_path.parent() {
                    open_dir_in_explorer(dir);
                }
            }
            FREE_CMD_EXIT => {
                let _ = self
                    .tx
                    .send(BgEvent::FreeDesktopMenuCmd(FreeDesktopMenuCommand::ExitApp));
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            _ => {}
        }
    }

    fn show_free_icon_menu(&self, paths: Vec<String>, x: i32, y: i32) {
        if paths.is_empty() {
            return;
        }
        let single = paths.len() == 1;
        let bucket_targets: Vec<(i32, String)> = {
            let guard = lock_unpoisoned(&self.shared);
            let desktop_dirs = &guard.desktop_dirs;
            guard
                .config
                .fences
                .iter()
                .filter(|f| f.is_desktop_bucket(desktop_dirs))
                .map(|f| (f.id, format_fence_title(f)))
                .collect()
        };

        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, FREE_ICON_CMD_OPEN as usize, w!("打开\tEnter"));
            if single {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FREE_ICON_CMD_OPEN_ADMIN as usize,
                    w!("以管理员身份运行"),
                );
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FREE_ICON_CMD_REVEAL as usize,
                    w!("在资源管理器中显示"),
                );
            }
            if !bucket_targets.is_empty()
                && let Ok(sub) = CreatePopupMenu()
            {
                for (idx, (_, title)) in bucket_targets.iter().enumerate() {
                    let wtitle = HSTRING::from(title.as_str());
                    let _ = AppendMenuW(
                        sub,
                        MF_STRING,
                        (FREE_ICON_CMD_MOVE_BASE as usize) + idx,
                        &wtitle,
                    );
                }
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("移动到格子"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, FREE_ICON_CMD_COPY as usize, w!("复制\tCtrl+C"));
            let _ = AppendMenuW(menu, MF_STRING, FREE_ICON_CMD_CUT as usize, w!("剪切\tCtrl+X"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_ICON_CMD_COPY_PATHS as usize,
                w!("复制路径\tCtrl+Shift+C"),
            );
            if single {
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FREE_ICON_CMD_RENAME as usize,
                    w!("重命名\tF2"),
                );
            }
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                FREE_ICON_CMD_TRASH as usize,
                w!("删除到回收站\tDel"),
            );
            if single {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    FREE_ICON_CMD_PROPERTIES as usize,
                    w!("属性\tAlt+Enter"),
                );
            }
        }

        let cmd = {
            let _popup_guard = MenuPopupActiveGuard::enter(&self.shared);
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
                TrackPopupMenuEx(
                    menu,
                    (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0,
                    x,
                    y,
                    self.hwnd,
                    None,
                )
            }
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }

        let raw_cmd = cmd.0 as u32;
        let action = match raw_cmd {
            FREE_ICON_CMD_OPEN => Some(FreeIconMenuCommand::Open),
            FREE_ICON_CMD_OPEN_ADMIN => Some(FreeIconMenuCommand::OpenAsAdmin),
            FREE_ICON_CMD_REVEAL => Some(FreeIconMenuCommand::RevealInExplorer),
            FREE_ICON_CMD_COPY => Some(FreeIconMenuCommand::Copy { cut: false }),
            FREE_ICON_CMD_CUT => Some(FreeIconMenuCommand::Copy { cut: true }),
            FREE_ICON_CMD_COPY_PATHS => Some(FreeIconMenuCommand::CopyPaths),
            FREE_ICON_CMD_RENAME => Some(FreeIconMenuCommand::Rename),
            FREE_ICON_CMD_TRASH => Some(FreeIconMenuCommand::Trash),
            FREE_ICON_CMD_PROPERTIES => Some(FreeIconMenuCommand::Properties),
            n if n >= FREE_ICON_CMD_MOVE_BASE => {
                let idx = (n - FREE_ICON_CMD_MOVE_BASE) as usize;
                bucket_targets
                    .get(idx)
                    .map(|(fid, _)| FreeIconMenuCommand::MoveToFence(*fid))
            }
            _ => None,
        };
        if let Some(action) = action {
            let _ = self.tx.send(BgEvent::FreeIconMenuCmd(paths, action));
            self.egui_ctx.request_repaint_of(ViewportId::ROOT);
        }
    }
}

unsafe extern "system" fn helper_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_msg != 0 {
            TASKBAR_CREATED_MSG.store(taskbar_msg, Ordering::Release);
        }
        register_power_and_session_notifications(hwnd);
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        unregister_power_and_session_notifications(hwnd);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut HelperState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let st = unsafe { &mut *ptr };

    match msg {
        WM_APP_SYNC_WATCHERS => {
            st.rebuild_watchers();
            LRESULT(0)
        }
        WM_APP_SCHEDULE_SHELL_VERIFY => {
            st.schedule_shell_reverify();
            LRESULT(0)
        }
        WM_APP_FREE_DESKTOP_EVENT => {
            st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            LRESULT(0)
        }
        WM_APP_REFRESH_FREE_DESKTOP => {
            st.push_free_desktop_snapshot_if_dirty();
            LRESULT(0)
        }
        WM_APP_EXTERNAL_FOREGROUND => {
            let _ = st.tx.send(BgEvent::ExternalForeground);
            st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            LRESULT(0)
        }
        WM_APP_DRAG_DONE => {
            let (changed, fids, free_snap) = {
                let mut guard = lock_unpoisoned(&st.shared);
                let changed = guard.poll_async_drag();
                let fids: Vec<i32> = guard.pending_repaint_fences.drain().collect();
                let repaint_free = std::mem::take(&mut guard.pending_repaint_free_desktop);
                let free_snap = repaint_free.then(|| build_free_desktop_snapshot(&guard));
                for &fid in &fids {
                    guard.request_repaint_fence(&st.egui_ctx, fid);
                }
                (changed, fids, free_snap)
            };
            if let Some(snap) = free_snap
                && let Some(ctrl) = lock_unpoisoned(&st.free_desktop_ctrl).as_ref().cloned()
            {
                ctrl.update_snapshot(snap);
            }
            if changed || !fids.is_empty() {
                st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            LRESULT(0)
        }
        WM_APP_SHUTDOWN => {
            st.shutting_down = true;
            st.watchers.clear();
            st.tray = None;
            let _ = unsafe { KillTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), PORTAL_DEBOUNCE_TIMER) };
            let _ = unsafe { KillTimer(Some(hwnd), SHELL_VERIFY_TIMER_1) };
            let _ = unsafe { KillTimer(Some(hwnd), SHELL_VERIFY_TIMER_2) };
            let _ = unsafe { KillTimer(Some(hwnd), TRANSIENT_RETRY_TIMER) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_APP_PORTAL_DIRTY => {
            let fid = wparam.0 as i32;
            if fid < 0 {
                st.dirty_desktop = true;
            } else {
                st.dirty_portals.insert(fid);
            }
            unsafe { SetTimer(Some(hwnd), PORTAL_DEBOUNCE_TIMER, PORTAL_DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_APP_SHOW_FENCE_MENU | WM_APP_SHOW_FENCE_MENU_BODY => {
            let fid = wparam.0 as i32;
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            st.show_fence_menu(fid, x, y, msg == WM_APP_SHOW_FENCE_MENU_BODY);
            LRESULT(0)
        }
        WM_APP_SHOW_FREE_DESKTOP_MENU => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            st.show_free_desktop_menu(x, y);
            LRESULT(0)
        }
        WM_APP_SHOW_FREE_ICON_MENU => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let paths = std::mem::take(&mut *lock_unpoisoned(&st.free_icon_menu_paths));
            st.show_free_icon_menu(paths, x, y);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let ev = (lparam.0 & 0xffff) as u32;
            if ev == WM_RBUTTONUP {
                st.show_tray_menu();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            st.schedule_display_reprobe();
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if is_relevant_setting_change(wparam, lparam) {
                st.schedule_display_reprobe();
            }
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            if wparam.0 as u32 == WTS_SESSION_UNLOCK {
                st.schedule_display_reprobe();
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if is_display_wake_power_broadcast(wparam, lparam) {
                st.schedule_display_reprobe();
            }
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == DISPLAY_DEBOUNCE_TIMER => {
            if st.last_display_event.elapsed()
                >= Duration::from_millis(u64::from(DISPLAY_DEBOUNCE_MS / 2))
            {
                st.try_probe_topology_and_notify();
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TRANSIENT_RETRY_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), TRANSIENT_RETRY_TIMER) };
            st.try_probe_topology_and_notify();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == PORTAL_DEBOUNCE_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), PORTAL_DEBOUNCE_TIMER) };
            let mut sent = false;
            if std::mem::take(&mut st.dirty_desktop) {
                let _ = st.tx.send(BgEvent::DesktopDirty);
                sent = true;
            }
            for fid in st.dirty_portals.drain() {
                let _ = st.tx.send(BgEvent::PortalDirty(fid));
                sent = true;
            }
            if sent {
                st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == SHELL_VERIFY_TIMER_1 || wparam.0 == SHELL_VERIFY_TIMER_2 => {
            let _ = unsafe { KillTimer(Some(hwnd), wparam.0) };
            if !st.shutting_down {
                let _ = set_shell_desktop_icons_visible(false);
                post_reassert_all_fences_z();
            }
            LRESULT(0)
        }
        WM_QUERYENDSESSION => {
            sync_topology_and_save(&st.layout_path, &st.shared, "WM_QUERYENDSESSION");
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                st.shutting_down = true;
                hide_all_fences_immediately();
                if let Some(ctrl) = lock_unpoisoned(&st.free_desktop_ctrl).as_ref().cloned() {
                    ctrl.shutdown();
                }
                persist_and_restore_on_exit(&st.layout_path, &st.shared, "WM_ENDSESSION");
            } else if !st.shutting_down {
                let _ = set_shell_desktop_icons_visible(false);
                post_reassert_all_fences_z();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            st.watchers.clear();
            st.tray = None;
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ if st.taskbar_created_msg != 0 && msg == st.taskbar_created_msg => {
            if !st.shutting_down {
                st.tray = None;
                st.tray = TrayIcon::new(hwnd, 1, "DeskLite 桌面整理").ok();
                let _ = set_shell_desktop_icons_visible(false);
                st.schedule_shell_reverify();
                post_reassert_all_fences_z();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn spawn_win32_helper(
    layout_path: PathBuf,
    shared: Arc<Mutex<SharedFenceState>>,
    free_desktop_ctrl: Arc<Mutex<Option<FreeDesktopController>>>,
    free_icon_menu_paths: Arc<Mutex<Vec<String>>>,
    tx: Sender<BgEvent>,
    egui_ctx: egui::Context,
) -> isize {
    let (hwnd_tx, hwnd_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let Ok(hinst) = (unsafe { GetModuleHandleW(None) }) else {
            let _ = hwnd_tx.send(0);
            return;
        };
        let cls = w!("DeskLiteBgHelperWnd");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(helper_wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: cls,
            ..Default::default()
        };
        let _ = unsafe { RegisterClassExW(&wc) };

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                cls,
                w!("DeskLite Helper"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        };
        let Ok(hwnd) = hwnd else {
            let _ = hwnd_tx.send(0);
            return;
        };

        let mut helper = Box::new(HelperState {
            hwnd,
            layout_path,
            shared,
            free_desktop_ctrl,
            free_icon_menu_paths,
            tx,
            egui_ctx,
            tray: TrayIcon::new(hwnd, 1, "DeskLite 桌面整理").ok(),
            taskbar_created_msg: TASKBAR_CREATED_MSG.load(Ordering::Acquire),
            watchers: HashMap::new(),
            last_display_event: Instant::now(),
            transient_retries: 0,
            dirty_desktop: false,
            dirty_portals: HashSet::new(),
            shutting_down: false,
        });
        helper.rebuild_watchers();

        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&raw mut *helper) as isize);
        }
        set_external_foreground_notify_target(hwnd.0 as isize, WM_APP_EXTERNAL_FOREGROUND);
        let _z_hooks = install_desktop_z_win_event_hooks();
        let _ = hwnd_tx.send(hwnd.0 as isize);

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        drop(_z_hooks);
        helper.tray = None;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
            CoUninitialize();
        }
    });
    hwnd_rx.recv().unwrap_or(0)
}

struct DeskLiteEguiApp {
    layout_path: PathBuf,
    shared: Arc<Mutex<SharedFenceState>>,
    free_desktop_ctrl: FreeDesktopController,
    free_desktop_rx: Receiver<FreeDesktopEvent>,
    free_icon_menu_paths: Arc<Mutex<Vec<String>>>,
    rx: Receiver<BgEvent>,
    tx: Sender<BgEvent>,
    helper_hwnd: isize,
    host_attach_passes: u8,
    prev_hide_all_fences: bool,
    shutting_down: bool,
}

impl DeskLiteEguiApp {
    fn save_now(&self) {
        save_shared_now(&self.layout_path, &self.shared, "save_now");
    }

    fn post_helper(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) {
        if self.helper_hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.helper_hwnd as *mut _)),
                    msg,
                    wparam,
                    lparam,
                );
            }
        }
    }

    fn sync_watchers(&self) {
        self.post_helper(WM_APP_SYNC_WATCHERS, WPARAM(0), LPARAM(0));
    }

    fn schedule_shell_reverify(&self) {
        self.post_helper(WM_APP_SCHEDULE_SHELL_VERIFY, WPARAM(0), LPARAM(0));
    }

    fn push_free_desktop_snapshot(&self) {
        let snapshot = {
            let mut guard = lock_unpoisoned(&self.shared);
            guard.pending_repaint_free_desktop = false;
            build_free_desktop_snapshot(&guard)
        };
        self.free_desktop_ctrl.update_snapshot(snapshot);
    }

    fn begin_shutdown(&mut self, ctx: &egui::Context) {
        if !self.shutting_down {
            self.shutting_down = true;
            hide_all_fences_immediately();
            self.free_desktop_ctrl.shutdown();
            persist_and_restore_on_exit(&self.layout_path, &self.shared, "shutdown");
            self.post_helper(WM_APP_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn spawn_folder_picker_cmd(&self, ctx: &egui::Context, fence_id: Option<i32>) {
        let tx = self.tx.clone();
        let ctx_clone = ctx.clone();
        thread::spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            if let Ok(Some(dir)) = pick_folder_dialog(HWND::default()) {
                if is_desktop_dir_path(Path::new(&dir)) {
                    show_desktop_portal_forbidden_dialog();
                } else {
                    let ev = match fence_id {
                        Some(fid) => BgEvent::BindPortalResult(fid, dir),
                        None => BgEvent::TrayNewPortal(dir),
                    };
                    let _ = tx.send(ev);
                    ctx_clone.request_repaint_of(ViewportId::ROOT);
                }
            }
            unsafe {
                CoUninitialize();
            }
        });
    }

    fn create_blank_fence(&self) {
        let mut guard = lock_unpoisoned(&self.shared);
        let id = guard.config.next_fence_id();
        let work = guard.topology.primary().work_area;
        let offset = ((guard.config.fences.len() as i32) % 6) * 28;
        let raw_rect = Rect::new(
            (work.left + 90 + offset).min(work.right - 300),
            (work.top + 90 + offset).min(work.bottom - 260),
            (work.left + 390 + offset).min(work.right),
            (work.top + 350 + offset).min(work.bottom),
        );
        let rect = clamp_rect_to_monitors(raw_rect, guard.topology.monitors())
            .map(|(r, _)| r)
            .unwrap_or(raw_rect);
        guard.config.upsert_fence(FenceSpec {
            id,
            name: "新建格子".to_string(),
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            layout_override: None,
            items: Vec::new(),
        });
        guard.config.update_active_snapshot_rect(id, rect);
        guard.sync_builder_rects_from_config();
        guard.rename_prompt = Some(RenamePrompt {
            fence_id: id,
            draft: "新建格子".to_string(),
            needs_focus: true,
        });
        guard.dirty_layout = true;
    }

    fn create_portal_fence(&self, dir: String) {
        if is_desktop_dir_path(Path::new(&dir)) {
            show_desktop_portal_forbidden_dialog();
            return;
        }
        let folder_name = Path::new(&dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dir.clone());
        let items = scan_portal_directory(Path::new(&dir));
        let mut guard = lock_unpoisoned(&self.shared);
        let id = guard.config.next_fence_id();
        let work = guard.topology.primary().work_area;
        let offset = ((guard.config.fences.len() as i32) % 6) * 28;
        let raw_rect = Rect::new(
            (work.left + 110 + offset).min(work.right - 320),
            (work.top + 110 + offset).min(work.bottom - 280),
            (work.left + 430 + offset).min(work.right),
            (work.top + 390 + offset).min(work.bottom),
        );
        let rect = clamp_rect_to_monitors(raw_rect, guard.topology.monitors())
            .map(|(r, _)| r)
            .unwrap_or(raw_rect);
        guard.config.upsert_fence(FenceSpec {
            id,
            name: folder_name,
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            locked: false,
            role: Some(FenceRole::ExternalPortal { dir: dir.clone() }),
            portal_dir: Some(dir),
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            layout_override: None,
            items,
        });
        guard.config.update_active_snapshot_rect(id, rect);
        guard.sync_builder_rects_from_config();
        guard.dirty_layout = true;
    }

    fn apply_topology_update(
        &mut self,
        topo: DisplayTopology,
        refresh_portals: bool,
        ctx: &egui::Context,
    ) {
        let desktop_dirs = { lock_unpoisoned(&self.shared).desktop_dirs.clone() };
        let scanned_desktop = scan_all_desktop_items(&desktop_dirs);

        let (fences_snap, hwnd_to_fence_snap, fence_ids) = {
            let mut guard = lock_unpoisoned(&self.shared);
            guard.topology = topo.clone();
            if let Err(err) = guard.config.apply_topology(&topo, (72, 72)) {
                eprintln!("[desklite] apply_topology failed: {err}");
            }
            if refresh_portals {
                refresh_all_portals(&mut guard.config, &desktop_dirs);
            }
            let _ = reconcile_free_icons_in_config(
                &mut guard.config,
                &topo,
                &desktop_dirs,
                &scanned_desktop,
            );
            guard.sync_builder_rects_from_config();
            let ids: Vec<i32> = guard.config.fences.iter().map(|f| f.id).collect();
            (
                guard.config.fences.clone(),
                guard.hwnd_to_fence.clone(),
                ids,
            )
        };

        move_live_windows_to_topology(&topo, &fences_snap, &hwnd_to_fence_snap);
        self.free_desktop_ctrl.set_bounds(topo.virtual_bounds());
        self.push_free_desktop_snapshot();
        lock_unpoisoned(&self.shared).finish_topology_settling(Duration::from_millis(250));

        if !self.shutting_down {
            let _ = set_shell_desktop_icons_visible(false);
            post_reassert_all_fences_z();
            self.schedule_shell_reverify();
        }

        let guard = lock_unpoisoned(&self.shared);
        for fid in fence_ids {
            guard.request_repaint_fence(ctx, fid);
        }
    }

    fn refresh_desktop_views(&self) -> bool {
        let desktop_dirs = { lock_unpoisoned(&self.shared).desktop_dirs.clone() };
        let scanned_desktop = scan_all_desktop_items(&desktop_dirs);
        let reconciled = {
            let mut guard = lock_unpoisoned(&self.shared);
            let topo = guard.topology.clone();
            reconcile_free_icons_in_config(
                &mut guard.config,
                &topo,
                &desktop_dirs,
                &scanned_desktop,
            )
        };
        self.push_free_desktop_snapshot();
        reconciled > 0
    }

    fn toggle_hide_all_fences(&self, ctx: &egui::Context) {
        let (now_hidden, fence_ids) = {
            let mut guard = lock_unpoisoned(&self.shared);
            guard.config.hide_all_fences = !guard.config.hide_all_fences;
            guard.dirty_layout = true;
            let ids: Vec<i32> = guard.config.fences.iter().map(|f| f.id).collect();
            (guard.config.hide_all_fences, ids)
        };
        self.push_free_desktop_snapshot();
        if !now_hidden {
            let guard = lock_unpoisoned(&self.shared);
            for fid in fence_ids {
                guard.request_repaint_fence(ctx, fid);
            }
            drop(guard);
            post_reassert_all_fences_z();
        }
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    fn drain_events_and_commands(&mut self, ctx: &egui::Context) -> bool {
        let mut needs_save = false;
        let mut needs_watcher_sync = false;
        let mut needs_free_snapshot = false;

        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::TopologyChanged(topo) => {
                    self.apply_topology_update(topo, false, ctx);
                    needs_save = true;
                }
                BgEvent::PortalDirty(fid) => {
                    let ext_dir = {
                        let guard = lock_unpoisoned(&self.shared);
                        guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == fid)
                            .and_then(|f| f.external_portal_dir(&guard.desktop_dirs))
                    };
                    if let Some(dir) = ext_dir {
                        let items = scan_portal_directory(Path::new(&dir));
                        let mut guard = lock_unpoisoned(&self.shared);
                        if let Some(f) = guard.config.fence_mut(fid) {
                            f.items = items;
                        }
                        guard.request_repaint_fence(ctx, fid);
                    }
                }
                BgEvent::DesktopDirty => {
                    if self.refresh_desktop_views() {
                        needs_save = true;
                    }
                }
                BgEvent::TrayNewBlank => {
                    self.create_blank_fence();
                    needs_save = true;
                }
                BgEvent::TrayNewPortal(dir) => {
                    self.create_portal_fence(dir);
                    needs_save = true;
                    needs_watcher_sync = true;
                }
                BgEvent::TrayRealign => {
                    if let Ok(topo) = probe_display_topology() {
                        self.apply_topology_update(topo, true, ctx);
                        needs_save = true;
                    }
                }
                BgEvent::ToggleHideAllFences => {
                    self.toggle_hide_all_fences(ctx);
                    needs_save = true;
                }
                BgEvent::BindPortalResult(fid, dir) => {
                    if is_desktop_dir_path(Path::new(&dir)) {
                        show_desktop_portal_forbidden_dialog();
                        continue;
                    }
                    let ext_items = scan_portal_directory(Path::new(&dir));
                    {
                        let mut guard = lock_unpoisoned(&self.shared);
                        if let Some(f) = guard.config.fence_mut(fid) {
                            f.role = Some(FenceRole::ExternalPortal { dir: dir.clone() });
                            f.portal_dir = Some(dir);
                            f.items = ext_items;
                            needs_save = true;
                            needs_watcher_sync = true;
                        }
                        guard.request_repaint_fence(ctx, fid);
                    }
                    needs_free_snapshot = true;
                }
                BgEvent::FenceMenuCommand(fid, action) => {
                    if apply_collapsed_menu_command(fid, action, &self.shared, ctx) {
                        needs_save = true;
                    }
                }
                BgEvent::FreeDesktopMenuCmd(action) => {
                    if apply_free_desktop_menu_command(action, &self.shared, ctx) {
                        needs_save = true;
                    }
                    needs_free_snapshot = true;
                }
                BgEvent::FreeIconMenuCmd(paths, action) => {
                    let is_rename = matches!(action, FreeIconMenuCommand::Rename);
                    if apply_free_icon_menu_command(&paths, action, &self.shared, ctx) {
                        needs_save = true;
                    }
                    if is_rename
                        && let Some((path, cur_name, rect)) =
                            pending_free_rename(&lock_unpoisoned(&self.shared))
                    {
                        self.free_desktop_ctrl
                            .begin_inline_rename(path, cur_name, rect);
                    }
                    needs_free_snapshot = true;
                }
                BgEvent::ExternalForeground => {
                    let cleared = {
                        let mut guard = lock_unpoisoned(&self.shared);
                        if !guard.is_focus_clear_exempt(desklite_ui::SelectionScope::FreeDesktop, Some(ctx))
                            && !guard.free_selected_paths.is_empty()
                        {
                            guard.free_selected_paths.clear();
                            guard.free_selection_anchor = None;
                            true
                        } else {
                            false
                        }
                    };
                    if cleared {
                        needs_free_snapshot = true;
                    }
                }
                BgEvent::FlushAndExit => {
                    self.begin_shutdown(ctx);
                    return false;
                }
            }
        }

        let mut cmds: Vec<FenceCommand> = Vec::new();
        while let Ok(free_ev) = self.free_desktop_rx.try_recv() {
            let res = handle_free_desktop_event(free_ev, &self.shared);
            if res.needs_save {
                needs_save = true;
            }
            if res.needs_snapshot_refresh {
                needs_free_snapshot = true;
            }
            if let Some(path) = res.launch_path
                && let Err(err) = launch_item(&path)
            {
                lock_unpoisoned(&self.shared).free_toast =
                    Some((format!("打开失败: {err}"), Instant::now()));
                needs_free_snapshot = true;
            }
            if let Some((path, cur_name, rect)) = res.begin_rename {
                self.free_desktop_ctrl
                    .begin_inline_rename(path, cur_name, rect);
            }
            if !res.repaint_fences.is_empty() {
                let guard = lock_unpoisoned(&self.shared);
                for fid in res.repaint_fences {
                    guard.request_repaint_fence(ctx, fid);
                }
            }
            cmds.extend(res.commands);
        }

        let (shared_cmds, async_save, pending_free) = {
            let mut guard = lock_unpoisoned(&self.shared);
            (
                std::mem::take(&mut guard.pending_commands),
                std::mem::take(&mut guard.pending_immediate_save_from_async),
                std::mem::take(&mut guard.pending_repaint_free_desktop),
            )
        };
        if async_save {
            needs_save = true;
        }
        if pending_free {
            needs_free_snapshot = true;
        }
        cmds.extend(shared_cmds);

        for cmd in cmds {
            match cmd {
                FenceCommand::RequestCreateBlank => {
                    self.create_blank_fence();
                    needs_save = true;
                }
                FenceCommand::RequestCreateUnfiledDesktop => {}
                FenceCommand::RequestCreatePortal => {
                    self.spawn_folder_picker_cmd(ctx, None);
                }
                FenceCommand::RequestBindPortal(fid) => {
                    self.spawn_folder_picker_cmd(ctx, Some(fid));
                }
                FenceCommand::RequestDelete(fid) => {
                    let desktop_dirs = { lock_unpoisoned(&self.shared).desktop_dirs.clone() };
                    let scanned_desktop = scan_all_desktop_items(&desktop_dirs);
                    {
                        let mut guard = lock_unpoisoned(&self.shared);
                        let topo = guard.topology.clone();
                        if guard.config.remove_fence(fid) {
                            guard.prune_removed_fences();
                            let _ = reconcile_free_icons_in_config(
                                &mut guard.config,
                                &topo,
                                &desktop_dirs,
                                &scanned_desktop,
                            );
                            needs_save = true;
                            needs_watcher_sync = true;
                            needs_free_snapshot = true;
                        }
                    }
                }
                FenceCommand::SyncPortals => {
                    needs_watcher_sync = true;
                }
                FenceCommand::RefreshDesktop => {
                    if self.refresh_desktop_views() {
                        needs_save = true;
                    }
                    needs_free_snapshot = false;
                }
                FenceCommand::ToggleHideAllFences => {
                    self.toggle_hide_all_fences(ctx);
                    needs_save = true;
                    needs_free_snapshot = false;
                }
                FenceCommand::ShowFenceMenu {
                    fence_id,
                    screen_x,
                    screen_y,
                    include_body_actions,
                } => {
                    let packed = (((screen_y as i16 as u16) as usize) << 16)
                        | ((screen_x as i16 as u16) as usize);
                    let msg = if include_body_actions {
                        WM_APP_SHOW_FENCE_MENU_BODY
                    } else {
                        WM_APP_SHOW_FENCE_MENU
                    };
                    self.post_helper(msg, WPARAM(fence_id as usize), LPARAM(packed as isize));
                }
                FenceCommand::ShowFreeDesktopMenu { screen_x, screen_y } => {
                    let packed = (((screen_y as i16 as u16) as usize) << 16)
                        | ((screen_x as i16 as u16) as usize);
                    self.post_helper(
                        WM_APP_SHOW_FREE_DESKTOP_MENU,
                        WPARAM(0),
                        LPARAM(packed as isize),
                    );
                }
                FenceCommand::ShowFreeIconMenu {
                    paths,
                    screen_x,
                    screen_y,
                } => {
                    *lock_unpoisoned(&self.free_icon_menu_paths) = paths;
                    let packed = (((screen_y as i16 as u16) as usize) << 16)
                        | ((screen_x as i16 as u16) as usize);
                    self.post_helper(
                        WM_APP_SHOW_FREE_ICON_MENU,
                        WPARAM(0),
                        LPARAM(packed as isize),
                    );
                }
                FenceCommand::FreeDesktopMenuAction(action) => {
                    if apply_free_desktop_menu_command(action, &self.shared, ctx) {
                        needs_save = true;
                    }
                    needs_free_snapshot = true;
                }
                FenceCommand::RequestExitApp => {
                    self.begin_shutdown(ctx);
                    return false;
                }
            }
        }

        if needs_watcher_sync {
            self.sync_watchers();
        }
        let pending_due = { lock_unpoisoned(&self.shared).pending_save_at };
        if needs_save {
            self.save_now();
        } else if let Some(deadline) = pending_due {
            let now = Instant::now();
            if now >= deadline {
                self.save_now();
            } else {
                ctx.request_repaint_after(deadline - now);
            }
        }
        if needs_free_snapshot {
            self.push_free_desktop_snapshot();
        }
        true
    }
}

impl eframe::App for DeskLiteEguiApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.host_attach_passes < 2
            && let Ok(handle) = frame.window_handle()
            && let RawWindowHandle::Win32(h) = handle.as_raw()
        {
            let _ = strip_alt_tab_from_host(h.hwnd.get());
            self.host_attach_passes += 1;
            if self.host_attach_passes == 1 {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }

        let ctx = ui.ctx().clone();
        if !self.drain_events_and_commands(&ctx) {
            return;
        }

        let (
            fences,
            builder_rects,
            topology,
            hide_all_fences,
            attached_fences,
            pending_reveal,
            inspector_meta,
        ) = {
            let guard = lock_unpoisoned(&self.shared);
            let insp = if guard.inspector_spawned {
                guard.inspector.as_ref().map(|ins| {
                    let scale = if guard.topology.monitors().is_empty() {
                        1.0
                    } else {
                        (nearest_monitor(
                            Point {
                                x: ins.initial_pos.0,
                                y: ins.initial_pos.1,
                            },
                            guard.topology.monitors(),
                        )
                        .dpi_x as f32
                            / 96.0)
                            .max(0.5)
                    };
                    (
                        ins.open,
                        Pos2::new(
                            ins.initial_pos.0 as f32 / scale,
                            ins.initial_pos.1 as f32 / scale,
                        ),
                    )
                })
            } else {
                None
            };
            (
                guard.config.fences.clone(),
                guard.builder_rects.clone(),
                guard.topology.clone(),
                guard.config.hide_all_fences,
                guard.attached_hwnds.keys().copied().collect::<HashSet<i32>>(),
                guard.pending_reveal_fences.keys().copied().collect::<HashSet<i32>>(),
                insp,
            )
        };

        let monitors = topology.monitors();
        for spec in fences {
            let fid = spec.id;
            let vid = ViewportId::from_hash_of(("desklite_fence", fid));
            let (brect, bcollapsed) = builder_rects
                .get(&fid)
                .copied()
                .unwrap_or((spec.rect, spec.collapsed));
            let mon_scale = if monitors.is_empty() {
                1.0
            } else {
                (nearest_monitor(brect.center(), monitors).dpi_x as f32 / 96.0).max(0.5)
            };
            let init_h = if bcollapsed {
                COLLAPSED_HEIGHT
            } else {
                (brect.height().max(MIN_FENCE_HEIGHT) as f32) / mon_scale
            };
            let wait_reveal = !attached_fences.contains(&fid) || pending_reveal.contains(&fid);
            let init_pos = if wait_reveal {
                Pos2::new(-32000.0, -32000.0)
            } else {
                Pos2::new(brect.left as f32 / mon_scale, brect.top as f32 / mon_scale)
            };
            let vb = ViewportBuilder::default()
                .with_title(format!("DeskLite#{fid}"))
                .with_decorations(false)
                .with_transparent(true)
                .with_taskbar(false)
                .with_visible(!hide_all_fences)
                .with_window_level(WindowLevel::AlwaysOnBottom)
                .with_position(init_pos)
                .with_inner_size(Vec2::new(
                    (brect.width().max(MIN_FENCE_WIDTH) as f32) / mon_scale,
                    init_h,
                ))
                .with_min_inner_size(Vec2::new(MIN_FENCE_WIDTH as f32, COLLAPSED_HEIGHT));

            let shared = Arc::clone(&self.shared);
            let layout_path = self.layout_path.clone();
            ctx.show_viewport_deferred(vid, vb, move |sub_ui, _class| {
                let res = render_fence_viewport(sub_ui, fid, &shared);
                if res.needs_immediate_save {
                    save_shared_now(&layout_path, &shared, "fence_viewport");
                }
            });
        }

        if self.prev_hide_all_fences != hide_all_fences {
            if self.prev_hide_all_fences && !hide_all_fences {
                post_reassert_all_fences_z();
            }
            self.push_free_desktop_snapshot();
            self.prev_hide_all_fences = hide_all_fences;
        }

        if let Some((insp_open, init_pos)) = inspector_meta {
            let ivb = ViewportBuilder::default()
                .with_title(INSPECTOR_TITLE)
                .with_decorations(false)
                .with_transparent(true)
                .with_taskbar(false)
                .with_resizable(true)
                .with_visible(insp_open)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_position(init_pos)
                .with_inner_size(Vec2::new(INSPECTOR_WIDTH, INSPECTOR_HEIGHT))
                .with_min_inner_size(Vec2::new(370.0, 440.0));
            let shared = Arc::clone(&self.shared);
            let layout_path = self.layout_path.clone();
            ctx.show_viewport_deferred(
                ViewportId::from_hash_of("desklite_inspector"),
                ivb,
                move |sub_ui, _class| {
                    let res = render_inspector_viewport(sub_ui, &shared);
                    if res.needs_immediate_save {
                        save_shared_now(&layout_path, &shared, "inspector_viewport");
                    }
                },
            );
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if !self.shutting_down {
            self.shutting_down = true;
            hide_all_fences_immediately();
            self.free_desktop_ctrl.shutdown();
            persist_and_restore_on_exit(&self.layout_path, &self.shared, "on_exit");
            self.post_helper(WM_APP_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
    }
}

fn main() {
    let Some(_guard) = acquire_single_instance() else {
        return;
    };
    install_panic_restore_hook();

    enable_per_monitor_dpi_v2();
    enable_dark_popup_menus();
    let Ok(topology) = probe_display_topology() else {
        return;
    };
    let layout_path = default_layout_path();
    let config = load_or_bootstrap_layout(&layout_path, &topology);
    let initial_hide_all = config.hide_all_fences;
    let desktop_dirs = desktop_dir_strings();

    let Ok(icon_cache) = IconCache::new() else {
        return;
    };
    let shared = Arc::new(Mutex::new(SharedFenceState::new(
        config,
        topology,
        icon_cache,
    )));
    {
        lock_unpoisoned(&shared).set_desktop_dirs(desktop_dirs);
    }

    let _ = set_shell_desktop_icons_visible(false);

    let free_desktop_ctrl_slot = Arc::new(Mutex::new(None));
    let free_icon_menu_paths = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();

    let mut native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("DeskLite Host")
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_position(Pos2::new(-32000.0, -32000.0))
            .with_inner_size(Vec2::new(1.0, 1.0)),
        clipboard_shortcuts: false,
        ..Default::default()
    };
    native_options.glow_options.vsync = true;

    let _ = eframe::run_native(
        "DeskLite",
        native_options,
        Box::new(move |cc| {
            install_chinese_fonts(&cc.egui_ctx);
            configure_egui_theme(&cc.egui_ctx);
            let helper_hwnd = spawn_win32_helper(
                layout_path.clone(),
                Arc::clone(&shared),
                Arc::clone(&free_desktop_ctrl_slot),
                Arc::clone(&free_icon_menu_paths),
                tx.clone(),
                cc.egui_ctx.clone(),
            );
            let (free_desktop_ctrl, free_desktop_rx) =
                match FreeDesktopController::spawn(helper_hwnd, WM_APP_FREE_DESKTOP_EVENT) {
                    Ok(pair) => pair,
                    Err(err) => {
                        restore_all_desktop_icons();
                        if helper_hwnd != 0 {
                            unsafe {
                                let _ = PostMessageW(
                                    Some(HWND(helper_hwnd as *mut _)),
                                    WM_APP_SHUTDOWN,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                        }
                        return Err(Box::new(err));
                    }
                };
            {
                *lock_unpoisoned(&free_desktop_ctrl_slot) = Some(free_desktop_ctrl.clone());
            }
            let initial_snapshot = {
                let mut guard = lock_unpoisoned(&shared);
                guard.free_desktop_hwnd = free_desktop_ctrl.hwnd();
                guard.set_helper_hwnd(helper_hwnd);
                guard.set_drag_notify_target(helper_hwnd, WM_APP_DRAG_DONE);
                guard.set_free_desktop_notify_target(helper_hwnd, WM_APP_REFRESH_FREE_DESKTOP);
                build_free_desktop_snapshot(&guard)
            };
            free_desktop_ctrl.update_snapshot(initial_snapshot);

            if helper_hwnd != 0 {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(helper_hwnd as *mut _)),
                        WM_APP_SCHEDULE_SHELL_VERIFY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            Ok(Box::new(DeskLiteEguiApp {
                layout_path,
                shared,
                free_desktop_ctrl,
                free_desktop_rx,
                free_icon_menu_paths,
                rx,
                tx,
                helper_hwnd,
                host_attach_passes: 0,
                prev_hide_all_fences: initial_hide_all,
                shutting_down: false,
            }))
        }),
    );
    restore_all_desktop_icons();
}
