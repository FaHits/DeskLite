#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use desklite_core::{
    DEFAULT_BACKDROP_ALPHA, DisplayTopology, FenceKind, FenceSpec, MIN_FENCE_HEIGHT,
    MIN_FENCE_WIDTH, NativeLayoutConfig, Rect, TopologyError, load_native_layout, nearest_monitor,
    save_native_layout,
};
use desklite_legacy::{LegacyOptions, legacy_to_native, load_legacy_dir};
use desklite_ui::fence_view::{FenceCommand, RenamePrompt, collect_managed_desktop_paths};
use desklite_ui::{
    COLLAPSED_HEIGHT, SharedFenceState, configure_egui_theme, install_chinese_fonts,
    render_fence_viewport,
};
use desklite_win32::{
    FolderWatcher, IconCache, TrayIcon, WM_APP_PORTAL_DIRTY, WM_APP_TRAY, all_desktop_dirs,
    enable_per_monitor_dpi_v2, hide_desktop_icons_for_paths, install_desktop_z_win_event_hooks,
    is_directly_on_desktop, is_vault_path, migrate_vault_item_to_desktop, pick_folder_dialog,
    probe_display_topology, restore_all_desktop_icons, restore_desktop_icon_async,
    scan_portal_directory, strip_alt_tab_from_host,
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
    KillTimer, MF_SEPARATOR, MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, PostMessageW, PostQuitMessage,
    RegisterClassExW, SW_SHOWNORMAL, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenuEx, TranslateMessage, WM_APP,
    WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_POWERBROADCAST,
    WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{HSTRING, w};

const DISPLAY_DEBOUNCE_TIMER: usize = 101;
const DISPLAY_DEBOUNCE_MS: u32 = 150;
const DESKTOP_DEBOUNCE_TIMER: usize = 102;
const DESKTOP_DEBOUNCE_MS: u32 = 160;
const WM_APP_SYNC_WATCHERS: u32 = WM_APP + 20;
const WM_APP_SHUTDOWN: u32 = WM_APP + 21;

const TRAY_CMD_NEW_BLANK: u32 = 1001;
const TRAY_CMD_NEW_PORTAL: u32 = 1002;
const TRAY_CMD_REALIGN: u32 = 1003;
const TRAY_CMD_OPEN_CONFIG_DIR: u32 = 1004;
const TRAY_CMD_EXIT: u32 = 1005;

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

fn default_layout_path() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("DeskLite").join("layout.json")
}

fn default_deskgo_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("Tencent").join("DeskGo")
}

fn refresh_all_portals(cfg: &mut NativeLayoutConfig) {
    for f in &mut cfg.fences {
        if let Some(ref dir) = f.portal_dir {
            f.items = scan_portal_directory(Path::new(dir));
        }
    }
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
        portal_dir: None,
        backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
        items: Vec::new(),
    }
}

fn migrate_legacy_vault_items(cfg: &mut NativeLayoutConfig) -> bool {
    let mut changed = false;
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
    changed
}

fn load_or_bootstrap_layout(layout_path: &Path, topo: &DisplayTopology) -> NativeLayoutConfig {
    if layout_path.exists()
        && let Ok(mut cfg) = load_native_layout(layout_path)
    {
        let sanitized = cfg.sanitize_legacy_containers();
        let migrated = migrate_legacy_vault_items(&mut cfg);
        if cfg.fences.is_empty() {
            cfg.fences.push(create_starter_fence(topo));
        }
        let _ = cfg.apply_topology(topo, (72, 72));
        refresh_all_portals(&mut cfg);
        if sanitized || migrated {
            let _ = save_native_layout(layout_path, &cfg);
        }
        return cfg;
    }

    let deskgo_dir = default_deskgo_dir();
    if deskgo_dir.join("FencesDataFile.dat").exists()
        && let Ok(bundle) = load_legacy_dir(&deskgo_dir, &LegacyOptions::default())
        && let Ok(mut cfg) = legacy_to_native(&bundle, Some(topo))
    {
        let _ = cfg.sanitize_legacy_containers();
        if cfg.fences.is_empty() {
            cfg.fences.push(create_starter_fence(topo));
        }
        let _ = cfg.apply_topology(topo, (72, 72));
        refresh_all_portals(&mut cfg);
        let _ = save_native_layout(layout_path, &cfg);
        return cfg;
    }

    let mut cfg = NativeLayoutConfig::default();
    cfg.fences.push(create_starter_fence(topo));
    let _ = cfg.apply_topology(topo, (72, 72));
    let _ = save_native_layout(layout_path, &cfg);
    cfg
}

#[derive(Debug, Clone)]
enum BgEvent {
    TopologyChanged(DisplayTopology),
    PortalDirty(i32),
    DesktopDirty,
    TrayNewBlank,
    TrayNewPortal(String),
    TrayRealign,
    BindPortalResult(i32, String),
    FlushAndExit,
}

struct HelperState {
    hwnd: HWND,
    layout_path: PathBuf,
    shared: Arc<Mutex<SharedFenceState>>,
    tx: Sender<BgEvent>,
    egui_ctx: egui::Context,
    watchers: HashMap<i32, FolderWatcher>,
    last_display_event: Instant,
}

impl HelperState {
    fn rebuild_watchers(&mut self) {
        self.watchers.clear();
        let portals: Vec<(i32, String)> = {
            let guard = self.shared.lock().unwrap();
            guard
                .config
                .fences
                .iter()
                .filter_map(|f| f.portal_dir.as_ref().map(|d| (f.id, d.clone())))
                .collect()
        };
        for (id, dir) in portals {
            if let Some(w) = FolderWatcher::start(id, Path::new(&dir), self.hwnd) {
                self.watchers.insert(id, w);
            }
        }
        for (idx, dir) in all_desktop_dirs().into_iter().enumerate() {
            let sentinel_id = -100 - (idx as i32);
            if let Some(w) = FolderWatcher::start(sentinel_id, &dir, self.hwnd) {
                self.watchers.insert(sentinel_id, w);
            }
        }
    }

    fn show_tray_menu(&self) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_NEW_BLANK as usize, w!("新建空白格子"));
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_NEW_PORTAL as usize,
                w!("新建文件夹映射格子..."),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_REALIGN as usize,
                w!("校验并归位全部格子"),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                TRAY_CMD_OPEN_CONFIG_DIR as usize,
                w!("打开配置目录"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_EXIT as usize, w!("退出 DeskLite"));
        }

        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let cmd = unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            TrackPopupMenuEx(
                menu,
                (TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD).0,
                pt.x,
                pt.y,
                self.hwnd,
                None,
            )
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
                // Run modal folder picker on a dedicated STA worker so helper_wnd_proc never re-enters &mut HelperState.
                let tx = self.tx.clone();
                let ctx = self.egui_ctx.clone();
                thread::spawn(move || {
                    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                    if let Ok(Some(dir)) = pick_folder_dialog(HWND::default()) {
                        let _ = tx.send(BgEvent::TrayNewPortal(dir));
                        ctx.request_repaint_of(ViewportId::ROOT);
                    }
                    unsafe {
                        CoUninitialize();
                    }
                });
            }
            TRAY_CMD_REALIGN => {
                let _ = self.tx.send(BgEvent::TrayRealign);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            TRAY_CMD_OPEN_CONFIG_DIR => {
                if let Some(dir) = self.layout_path.parent() {
                    let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
                    unsafe {
                        let _ =
                            ShellExecuteW(None, w!("open"), &wdir, None, None, SW_SHOWNORMAL);
                    }
                }
            }
            TRAY_CMD_EXIT => {
                let _ = self.tx.send(BgEvent::FlushAndExit);
                self.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            _ => {}
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
        return LRESULT(0);
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
        WM_APP_SHUTDOWN => {
            st.watchers.clear();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_APP_PORTAL_DIRTY => {
            let fid = wparam.0 as i32;
            if fid < 0 {
                unsafe { SetTimer(Some(hwnd), DESKTOP_DEBOUNCE_TIMER, DESKTOP_DEBOUNCE_MS, None) };
            } else {
                let _ = st.tx.send(BgEvent::PortalDirty(fid));
                st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            }
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let ev = (lparam.0 & 0xffff) as u32;
            if ev == WM_RBUTTONUP {
                st.show_tray_menu();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            st.last_display_event = Instant::now();
            unsafe { SetTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER, DISPLAY_DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                st.last_display_event = Instant::now();
                unsafe { SetTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER, DISPLAY_DEBOUNCE_MS, None) };
            }
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == DISPLAY_DEBOUNCE_TIMER => {
            if st.last_display_event.elapsed()
                >= Duration::from_millis(u64::from(DISPLAY_DEBOUNCE_MS / 2))
            {
                let _ = unsafe { KillTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER) };
                match probe_display_topology() {
                    Ok(topo) => {
                        let _ = st.tx.send(BgEvent::TopologyChanged(topo));
                        st.egui_ctx.request_repaint_of(ViewportId::ROOT);
                    }
                    Err(desklite_win32::Win32Error::Topology(TopologyError::EmptyTransient)) => {}
                    Err(_) => {}
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == DESKTOP_DEBOUNCE_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), DESKTOP_DEBOUNCE_TIMER) };
            let _ = st.tx.send(BgEvent::DesktopDirty);
            st.egui_ctx.request_repaint_of(ViewportId::ROOT);
            LRESULT(0)
        }
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            let guard = st.shared.lock().unwrap();
            let _ = save_native_layout(&st.layout_path, &guard.config);
            LRESULT(1)
        }
        WM_DESTROY => {
            st.watchers.clear();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn spawn_win32_helper(
    layout_path: PathBuf,
    shared: Arc<Mutex<SharedFenceState>>,
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
            tx,
            egui_ctx,
            watchers: HashMap::new(),
            last_display_event: Instant::now(),
        });
        helper.rebuild_watchers();

        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&raw mut *helper) as isize);
        }
        let _tray = TrayIcon::new(hwnd, 1, "DeskLite 桌面整理");
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
        drop(_tray);
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
    rx: Receiver<BgEvent>,
    tx: Sender<BgEvent>,
    helper_hwnd: isize,
    host_attach_passes: u8,
}

impl DeskLiteEguiApp {
    fn save_now(&self) {
        let mut guard = self.shared.lock().unwrap();
        let _ = save_native_layout(&self.layout_path, &guard.config);
        guard.dirty_layout = false;
        guard.pending_save_at = None;
    }

    fn sync_watchers(&self) {
        if self.helper_hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.helper_hwnd as *mut _)),
                    WM_APP_SYNC_WATCHERS,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    fn create_blank_fence(&self) {
        let mut guard = self.shared.lock().unwrap();
        let id = guard.config.next_fence_id();
        let work = guard.topology.primary().work_area;
        let offset = ((guard.config.fences.len() as i32) % 6) * 28;
        let rect = Rect::new(
            (work.left + 90 + offset).min(work.right - 300),
            (work.top + 90 + offset).min(work.bottom - 260),
            (work.left + 390 + offset).min(work.right),
            (work.top + 350 + offset).min(work.bottom),
        );
        guard.config.upsert_fence(FenceSpec {
            id,
            name: "新建格子".to_string(),
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            portal_dir: None,
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            items: Vec::new(),
        });
        let topo = guard.topology.clone();
        let _ = guard.config.apply_topology(&topo, (72, 72));
        guard.sync_builder_rects_from_config();
        guard.rename_prompt = Some(RenamePrompt {
            fence_id: id,
            draft: "新建格子".to_string(),
            needs_focus: true,
        });
        guard.dirty_layout = true;
    }

    fn create_portal_fence(&self, dir: String) {
        let folder_name = Path::new(&dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dir.clone());
        let mut guard = self.shared.lock().unwrap();
        let id = guard.config.next_fence_id();
        let work = guard.topology.primary().work_area;
        let offset = ((guard.config.fences.len() as i32) % 6) * 28;
        let rect = Rect::new(
            (work.left + 110 + offset).min(work.right - 320),
            (work.top + 110 + offset).min(work.bottom - 280),
            (work.left + 430 + offset).min(work.right),
            (work.top + 390 + offset).min(work.bottom),
        );
        let items = scan_portal_directory(Path::new(&dir));
        guard.config.upsert_fence(FenceSpec {
            id,
            name: folder_name,
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            portal_dir: Some(dir),
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            items,
        });
        let topo = guard.topology.clone();
        let _ = guard.config.apply_topology(&topo, (72, 72));
        guard.sync_builder_rects_from_config();
        guard.dirty_layout = true;
    }

    fn drain_events_and_commands(&self, ctx: &egui::Context) -> bool {
        let mut needs_save = false;
        let mut needs_watcher_sync = false;

        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::TopologyChanged(topo) => {
                    let mut guard = self.shared.lock().unwrap();
                    guard.topology = topo.clone();
                    let _ = guard.config.apply_topology(&topo, (72, 72));
                    guard.sync_builder_rects_from_config();
                    needs_save = true;
                }
                BgEvent::PortalDirty(fid) => {
                    let mut guard = self.shared.lock().unwrap();
                    if let Some(f) = guard.config.fence_mut(fid)
                        && let Some(ref dir) = f.portal_dir
                    {
                        f.items = scan_portal_directory(Path::new(dir));
                    }
                    ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
                }
                BgEvent::DesktopDirty => {
                    let managed = {
                        let guard = self.shared.lock().unwrap();
                        collect_managed_desktop_paths(&guard.config)
                    };
                    hide_desktop_icons_for_paths(&managed);
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
                        let mut guard = self.shared.lock().unwrap();
                        guard.topology = topo.clone();
                        let _ = guard.config.apply_topology(&topo, (72, 72));
                        guard.sync_builder_rects_from_config();
                        needs_save = true;
                    }
                }
                BgEvent::BindPortalResult(fid, dir) => {
                    let (old_paths, remaining) = {
                        let mut guard = self.shared.lock().unwrap();
                        let old_paths: Vec<String> = guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == fid && f.portal_dir.is_none())
                            .map(|f| f.items.iter().map(|it| it.path.clone()).collect())
                            .unwrap_or_default();
                        if let Some(f) = guard.config.fence_mut(fid) {
                            f.items = scan_portal_directory(Path::new(&dir));
                            f.portal_dir = Some(dir);
                            needs_save = true;
                            needs_watcher_sync = true;
                        }
                        (old_paths, collect_managed_desktop_paths(&guard.config))
                    };
                    for op in old_paths {
                        if is_directly_on_desktop(Path::new(&op)) {
                            restore_desktop_icon_async(op, None, remaining.clone());
                        }
                    }
                    ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
                }
                BgEvent::FlushAndExit => {
                    self.save_now();
                    restore_all_desktop_icons();
                    if self.helper_hwnd != 0 {
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(self.helper_hwnd as *mut _)),
                                WM_APP_SHUTDOWN,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return false;
                }
            }
        }

        let cmds: Vec<FenceCommand> = {
            let mut guard = self.shared.lock().unwrap();
            std::mem::take(&mut guard.pending_commands)
        };
        for cmd in cmds {
            match cmd {
                FenceCommand::RequestCreateBlank => {
                    self.create_blank_fence();
                    needs_save = true;
                }
                FenceCommand::RequestCreatePortal => {
                    let tx = self.tx.clone();
                    let ctx_clone = ctx.clone();
                    thread::spawn(move || {
                        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                        if let Ok(Some(dir)) = pick_folder_dialog(HWND::default()) {
                            let _ = tx.send(BgEvent::TrayNewPortal(dir));
                            ctx_clone.request_repaint_of(ViewportId::ROOT);
                        }
                        unsafe {
                            CoUninitialize();
                        }
                    });
                }
                FenceCommand::RequestBindPortal(fid) => {
                    let tx = self.tx.clone();
                    let ctx_clone = ctx.clone();
                    thread::spawn(move || {
                        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                        if let Ok(Some(dir)) = pick_folder_dialog(HWND::default()) {
                            let _ = tx.send(BgEvent::BindPortalResult(fid, dir));
                            ctx_clone.request_repaint_of(ViewportId::ROOT);
                        }
                        unsafe {
                            CoUninitialize();
                        }
                    });
                }
                FenceCommand::RequestDelete(fid) => {
                    let (restore_paths, remaining) = {
                        let mut guard = self.shared.lock().unwrap();
                        let restore_paths: Vec<String> = guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == fid && f.portal_dir.is_none())
                            .map(|f| f.items.iter().map(|it| it.path.clone()).collect())
                            .unwrap_or_default();
                        if guard.config.remove_fence(fid) {
                            guard.prune_removed_fences();
                            needs_save = true;
                            needs_watcher_sync = true;
                        }
                        (restore_paths, collect_managed_desktop_paths(&guard.config))
                    };
                    for rp in restore_paths {
                        if is_directly_on_desktop(Path::new(&rp)) {
                            restore_desktop_icon_async(rp, None, remaining.clone());
                        }
                    }
                }
                FenceCommand::SyncPortals => {
                    needs_watcher_sync = true;
                }
                FenceCommand::RequestExitApp => {
                    self.save_now();
                    restore_all_desktop_icons();
                    if self.helper_hwnd != 0 {
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(self.helper_hwnd as *mut _)),
                                WM_APP_SHUTDOWN,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return false;
                }
            }
        }

        if needs_watcher_sync {
            self.sync_watchers();
        }
        let pending_due = {
            let guard = self.shared.lock().unwrap();
            guard.pending_save_at
        };
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

        let (fences, builder_rects, monitors) = {
            let guard = self.shared.lock().unwrap();
            (
                guard.config.fences.clone(),
                guard.builder_rects.clone(),
                guard.topology.monitors().to_vec(),
            )
        };

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
                (nearest_monitor(brect.center(), &monitors).dpi_x as f32 / 96.0).max(0.5)
            };
            let init_h = if bcollapsed {
                COLLAPSED_HEIGHT
            } else {
                (brect.height().max(MIN_FENCE_HEIGHT) as f32) / mon_scale
            };
            let vb = ViewportBuilder::default()
                .with_title(format!("DeskLite#{fid}"))
                .with_decorations(false)
                .with_transparent(true)
                .with_taskbar(false)
                .with_window_level(WindowLevel::AlwaysOnBottom)
                .with_position(Pos2::new(
                    brect.left as f32 / mon_scale,
                    brect.top as f32 / mon_scale,
                ))
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
                    let mut guard = shared.lock().unwrap();
                    let _ = save_native_layout(&layout_path, &guard.config);
                    guard.dirty_layout = false;
                    guard.pending_save_at = None;
                }
            });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_now();
        restore_all_desktop_icons();
        if self.helper_hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.helper_hwnd as *mut _)),
                    WM_APP_SHUTDOWN,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

fn main() {
    let Some(_guard) = acquire_single_instance() else {
        return;
    };

    enable_per_monitor_dpi_v2();
    let Ok(topology) = probe_display_topology() else {
        return;
    };
    let layout_path = default_layout_path();
    let config = load_or_bootstrap_layout(&layout_path, &topology);
    let initial_managed = collect_managed_desktop_paths(&config);
    hide_desktop_icons_for_paths(&initial_managed);
    if !initial_managed.is_empty() {
        let retry_managed = initial_managed.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            hide_desktop_icons_for_paths(&retry_managed);
        });
    }

    let Ok(icon_cache) = IconCache::new() else {
        return;
    };
    let shared = Arc::new(Mutex::new(SharedFenceState::new(
        config,
        topology,
        icon_cache,
    )));
    let (tx, rx) = mpsc::channel();

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("DeskLite Host")
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_position(Pos2::new(-32000.0, -32000.0))
            .with_inner_size(Vec2::new(1.0, 1.0)),
        ..Default::default()
    };

    let _ = eframe::run_native(
        "DeskLite",
        native_options,
        Box::new(move |cc| {
            install_chinese_fonts(&cc.egui_ctx);
            configure_egui_theme(&cc.egui_ctx);
            let helper_hwnd = spawn_win32_helper(
                layout_path.clone(),
                Arc::clone(&shared),
                tx.clone(),
                cc.egui_ctx.clone(),
            );
            Ok(Box::new(DeskLiteEguiApp {
                layout_path,
                shared,
                rx,
                tx,
                helper_hwnd,
                host_attach_passes: 0,
            }))
        }),
    );
}
