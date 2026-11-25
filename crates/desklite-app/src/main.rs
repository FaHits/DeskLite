#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use desklite_core::{
    DEFAULT_BACKDROP_ALPHA, DisplayTopology, FenceKind, FenceSpec, NativeLayoutConfig, Rect,
    TopologyError, load_native_layout, save_native_layout,
};
use desklite_legacy::{LegacyOptions, legacy_to_native, load_legacy_dir};
use desklite_ui::fence_window::{LayoutQueryContext, WM_APP_PROCESS_ACTIONS};
use desklite_ui::{FenceAction, FenceWindow, SharedUiResources, prompt_fence_name};
use desklite_win32::{
    FolderWatcher, TrayIcon, WM_APP_PORTAL_DIRTY, WM_APP_TRAY, enable_per_monitor_dpi_v2,
    launch_item, pick_folder_dialog, probe_display_topology, resolve_dropped_item,
    reveal_in_explorer, scan_portal_directory,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    WPARAM,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    GWLP_USERDATA, KillTimer, MSG, MF_SEPARATOR, MF_STRING, PBT_APMRESUMEAUTOMATIC,
    PostQuitMessage, RegisterClassExW, SW_SHOWNORMAL, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenuEx,
    TranslateMessage, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION,
    WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, w};

const DISPLAY_DEBOUNCE_TIMER: usize = 101;
const DISPLAY_DEBOUNCE_MS: u32 = 150;

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
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\DeskLiteAppSingleInstance")) }.ok()?;
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

fn load_or_bootstrap_layout(
    layout_path: &Path,
    topo: &DisplayTopology,
) -> NativeLayoutConfig {
    if layout_path.exists()
        && let Ok(mut cfg) = load_native_layout(layout_path)
    {
        let _ = cfg.apply_topology(topo, (72, 72));
        return cfg;
    }

    let deskgo_dir = default_deskgo_dir();
    if deskgo_dir.join("FencesDataFile.dat").exists()
        && let Ok(bundle) = load_legacy_dir(&deskgo_dir, &LegacyOptions::default())
        && let Ok(mut cfg) = legacy_to_native(&bundle, Some(topo))
    {
        let _ = cfg.apply_topology(topo, (72, 72));
        let _ = save_native_layout(layout_path, &cfg);
        return cfg;
    }

    let work = topo.primary().work_area;
    let right = (work.right - 24).max(work.left + 320);
    let left = (right - 300).max(work.left);
    let mut cfg = NativeLayoutConfig::default();
    cfg.fences.push(FenceSpec {
        id: 101,
        name: "桌面整理".to_string(),
        kind: FenceKind::IconGrid,
        rect: Rect::new(left, work.top + 24, right, (work.top + 360).min(work.bottom)),
        collapsed: false,
        portal_dir: None,
        backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
        items: Vec::new(),
    });
    let _ = cfg.apply_topology(topo, (72, 72));
    let _ = save_native_layout(layout_path, &cfg);
    cfg
}

struct AppController {
    hwnd: HWND,
    layout_path: PathBuf,
    config: NativeLayoutConfig,
    topology: DisplayTopology,
    shared_ui: Rc<RefCell<SharedUiResources>>,
    pending_actions: Rc<RefCell<VecDeque<FenceAction>>>,
    query_ctx: Rc<RefCell<LayoutQueryContext>>,
    windows: HashMap<i32, FenceWindow>,
    watchers: HashMap<i32, FolderWatcher>,
    last_display_event: Instant,
}

impl AppController {
    fn sync_query_context(&self) {
        let mut q = self.query_ctx.borrow_mut();
        q.monitors = self.topology.monitors().to_vec();
        q.fence_rects = self
            .config
            .fences
            .iter()
            .map(|f| (f.id, f.rect))
            .collect();
        q.snap_threshold_px = self.config.preferences.snap_threshold_px;
        q.cell_w = self.config.preferences.icon_cell_w;
        q.cell_h = self.config.preferences.icon_cell_h;
    }

    fn persist_now(&self) {
        let _ = save_native_layout(&self.layout_path, &self.config);
    }

    fn refresh_portal_items(&mut self, fence_id: i32) {
        if let Some(f) = self.config.fence_mut(fence_id)
            && let Some(ref dir) = f.portal_dir
        {
            f.items = scan_portal_directory(Path::new(dir));
        }
    }

    fn rebuild_watchers(&mut self) {
        self.watchers.clear();
        for f in &self.config.fences {
            if let Some(ref dir) = f.portal_dir
                && let Some(w) = FolderWatcher::start(f.id, Path::new(dir), self.hwnd)
            {
                self.watchers.insert(f.id, w);
            }
        }
    }

    fn sync_all_windows(&mut self) {
        let valid_ids: Vec<i32> = self.config.fences.iter().map(|f| f.id).collect();
        self.windows.retain(|id, _| valid_ids.contains(id));

        for f in &mut self.config.fences {
            if let Some(ref dir) = f.portal_dir {
                f.items = scan_portal_directory(Path::new(dir));
            }
        }
        self.sync_query_context();

        for f in &self.config.fences {
            if let Some(win) = self.windows.get_mut(&f.id) {
                win.update_spec(f.clone());
            } else if let Ok(win) = FenceWindow::create(f.clone(), Rc::clone(&self.shared_ui)) {
                self.windows.insert(f.id, win);
            }
        }
        self.rebuild_watchers();
    }

    fn handle_topology_refresh(&mut self) {
        match probe_display_topology() {
            Ok(topo) => {
                self.topology = topo;
                let _ = self.config.apply_topology(&self.topology, (72, 72));
                self.sync_all_windows();
                self.persist_now();
            }
            Err(desklite_win32::Win32Error::Topology(TopologyError::EmptyTransient)) => {}
            Err(_) => {}
        }
    }

    fn create_blank_fence(&mut self) {
        let Some(name) = prompt_fence_name(self.hwnd, "新建空白格子", "新建格子") else {
            return;
        };
        let id = self.config.next_fence_id();
        let work = self.topology.primary().work_area;
        let offset = ((self.config.fences.len() as i32) % 6) * 28;
        let rect = Rect::new(
            (work.left + 80 + offset).min(work.right - 300),
            (work.top + 80 + offset).min(work.bottom - 260),
            (work.left + 380 + offset).min(work.right),
            (work.top + 340 + offset).min(work.bottom),
        );
        self.config.upsert_fence(FenceSpec {
            id,
            name,
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            portal_dir: None,
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            items: Vec::new(),
        });
        let _ = self.config.apply_topology(&self.topology, (72, 72));
        self.sync_all_windows();
        self.persist_now();
    }

    fn create_portal_fence(&mut self) {
        let Ok(Some(dir)) = pick_folder_dialog(self.hwnd) else {
            return;
        };
        let folder_name = Path::new(&dir)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dir.clone());
        let id = self.config.next_fence_id();
        let work = self.topology.primary().work_area;
        let offset = ((self.config.fences.len() as i32) % 6) * 28;
        let rect = Rect::new(
            (work.left + 100 + offset).min(work.right - 320),
            (work.top + 100 + offset).min(work.bottom - 280),
            (work.left + 420 + offset).min(work.right),
            (work.top + 380 + offset).min(work.bottom),
        );
        self.config.upsert_fence(FenceSpec {
            id,
            name: folder_name,
            kind: FenceKind::IconGrid,
            rect,
            collapsed: false,
            portal_dir: Some(dir),
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            items: Vec::new(),
        });
        let _ = self.config.apply_topology(&self.topology, (72, 72));
        self.sync_all_windows();
        self.persist_now();
    }

    fn process_pending_actions(&mut self) {
        loop {
            let next = self.pending_actions.borrow_mut().pop_front();
            let Some(action) = next else {
                break;
            };
            match action {
                FenceAction::MovedOrResized { fence_id, rect } => {
                    if let Some(f) = self.config.fence_mut(fence_id) {
                        f.rect = rect;
                    }
                    self.config.update_active_snapshot_rect(fence_id, rect);
                    self.sync_query_context();
                    self.persist_now();
                }
                FenceAction::ToggleCollapse { fence_id } => {
                    if let Some(f) = self.config.fence_mut(fence_id) {
                        f.collapsed = !f.collapsed;
                        let clone = f.clone();
                        if let Some(win) = self.windows.get_mut(&fence_id) {
                            win.update_spec(clone);
                        }
                    }
                    self.persist_now();
                }
                FenceAction::ToggleViewKind { fence_id } => {
                    if let Some(f) = self.config.fence_mut(fence_id) {
                        f.kind = match f.kind {
                            FenceKind::FileList => FenceKind::IconGrid,
                            _ => FenceKind::FileList,
                        };
                        let clone = f.clone();
                        if let Some(win) = self.windows.get_mut(&fence_id) {
                            win.update_spec(clone);
                        }
                    }
                    self.persist_now();
                }
                FenceAction::RenameFence { fence_id } => {
                    let current_name = self
                        .config
                        .fences
                        .iter()
                        .find(|f| f.id == fence_id)
                        .map(|f| f.name.clone())
                        .unwrap_or_default();
                    if let Some(new_name) =
                        prompt_fence_name(self.hwnd, "重命名格子", &current_name)
                        && let Some(f) = self.config.fence_mut(fence_id)
                    {
                        f.name = new_name;
                        let clone = f.clone();
                        if let Some(win) = self.windows.get_mut(&fence_id) {
                            win.update_spec(clone);
                        }
                        self.persist_now();
                    }
                }
                FenceAction::DeleteFence { fence_id } => {
                    if self.config.remove_fence(fence_id) {
                        self.sync_all_windows();
                        self.persist_now();
                    }
                }
                FenceAction::CreateBlankFence => self.create_blank_fence(),
                FenceAction::CreatePortalFence => self.create_portal_fence(),
                FenceAction::BindPortalDir { fence_id } => {
                    if let Ok(Some(dir)) = pick_folder_dialog(self.hwnd)
                        && let Some(f) = self.config.fence_mut(fence_id)
                    {
                        f.portal_dir = Some(dir);
                        self.sync_all_windows();
                        self.persist_now();
                    }
                }
                FenceAction::UnbindPortalDir { fence_id } => {
                    if let Some(f) = self.config.fence_mut(fence_id) {
                        f.portal_dir = None;
                        self.sync_all_windows();
                        self.persist_now();
                    }
                }
                FenceAction::DropFiles { fence_id, paths } => {
                    if let Some(f) = self.config.fence_mut(fence_id)
                        && f.portal_dir.is_none()
                    {
                        let mut changed = false;
                        for raw in paths {
                            let exists = f
                                .items
                                .iter()
                                .any(|it| it.path.eq_ignore_ascii_case(&raw));
                            if !exists {
                                let order = f.items.len() as i32;
                                f.items.push(resolve_dropped_item(Path::new(&raw), order));
                                changed = true;
                            }
                        }
                        if changed {
                            let clone = f.clone();
                            if let Some(win) = self.windows.get_mut(&fence_id) {
                                win.update_spec(clone);
                            }
                            self.persist_now();
                        }
                    }
                }
                FenceAction::ReorderOrTransferItem {
                    from_fence_id,
                    item_index,
                    screen_pt,
                } => {
                    let target_id = self.windows.iter().find_map(|(&id, win)| {
                        let mut wr = RECT::default();
                        if unsafe { GetWindowRect(win.hwnd(), &mut wr) }.is_ok()
                            && screen_pt.x >= wr.left
                            && screen_pt.x < wr.right
                            && screen_pt.y >= wr.top
                            && screen_pt.y < wr.bottom
                        {
                            Some(id)
                        } else {
                            None
                        }
                    });

                    if let Some(dst_id) = target_id {
                        if dst_id == from_fence_id {
                            let target_idx = self
                                .windows
                                .get(&dst_id)
                                .and_then(|w| w.item_at_screen_point(screen_pt));
                            if let Some(dst_idx) = target_idx
                                && dst_idx != item_index
                                && let Some(f) = self.config.fence_mut(dst_id)
                                && f.portal_dir.is_none()
                                && item_index < f.items.len()
                                && dst_idx < f.items.len()
                            {
                                let item = f.items.remove(item_index);
                                f.items.insert(dst_idx, item);
                                for (idx, it) in f.items.iter_mut().enumerate() {
                                    it.order = idx as i32;
                                }
                                let clone = f.clone();
                                if let Some(win) = self.windows.get_mut(&dst_id) {
                                    win.update_spec(clone);
                                }
                                self.persist_now();
                            }
                        } else {
                            let can_transfer = self
                                .config
                                .fences
                                .iter()
                                .find(|f| f.id == dst_id)
                                .is_some_and(|f| f.portal_dir.is_none());
                            let src_is_non_portal = self
                                .config
                                .fences
                                .iter()
                                .find(|f| f.id == from_fence_id)
                                .is_some_and(|f| f.portal_dir.is_none());
                            if can_transfer
                                && src_is_non_portal
                                && let Some(src) = self.config.fence_mut(from_fence_id)
                                && item_index < src.items.len()
                            {
                                let mut item = src.items.remove(item_index);
                                for (idx, it) in src.items.iter_mut().enumerate() {
                                    it.order = idx as i32;
                                }
                                if let Some(dst) = self.config.fence_mut(dst_id)
                                    && !dst
                                        .items
                                        .iter()
                                        .any(|it| it.path.eq_ignore_ascii_case(&item.path))
                                {
                                    item.order = dst.items.len() as i32;
                                    dst.items.push(item);
                                }
                                self.sync_all_windows();
                                self.persist_now();
                            }
                        }
                    }
                }
                FenceAction::RemoveItem {
                    fence_id,
                    item_index,
                } => {
                    if let Some(f) = self.config.fence_mut(fence_id)
                        && f.portal_dir.is_none()
                        && item_index < f.items.len()
                    {
                        f.items.remove(item_index);
                        for (idx, it) in f.items.iter_mut().enumerate() {
                            it.order = idx as i32;
                        }
                        let clone = f.clone();
                        if let Some(win) = self.windows.get_mut(&fence_id) {
                            win.update_spec(clone);
                        }
                        self.persist_now();
                    }
                }
                FenceAction::LaunchItem { path } => {
                    let _ = launch_item(&path);
                }
                FenceAction::RevealItem { path } => {
                    let _ = reveal_in_explorer(&path);
                }
            }
        }
    }

    fn show_tray_menu(&mut self) {
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
            TRAY_CMD_NEW_BLANK => self.create_blank_fence(),
            TRAY_CMD_NEW_PORTAL => self.create_portal_fence(),
            TRAY_CMD_REALIGN => self.handle_topology_refresh(),
            TRAY_CMD_OPEN_CONFIG_DIR => {
                if let Some(dir) = self.layout_path.parent() {
                    let wdir = HSTRING::from(dir.to_string_lossy().as_ref());
                    unsafe {
                        let _ = ShellExecuteW(
                            None,
                            w!("open"),
                            &wdir,
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
            TRAY_CMD_EXIT => {
                self.persist_now();
                unsafe {
                    PostQuitMessage(0);
                }
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn controller_wnd_proc(
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

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppController;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let ctrl = unsafe { &mut *ptr };

    match msg {
        WM_APP_PROCESS_ACTIONS => {
            ctrl.process_pending_actions();
            LRESULT(0)
        }
        WM_APP_PORTAL_DIRTY => {
            let fid = wparam.0 as i32;
            ctrl.refresh_portal_items(fid);
            if let Some(spec) = ctrl.config.fences.iter().find(|f| f.id == fid).cloned()
                && let Some(win) = ctrl.windows.get_mut(&fid)
            {
                win.update_spec(spec);
            }
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let event = (lparam.0 & 0xffff) as u32;
            if event == WM_RBUTTONUP {
                ctrl.show_tray_menu();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            ctrl.last_display_event = Instant::now();
            unsafe { SetTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER, DISPLAY_DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                ctrl.last_display_event = Instant::now();
                unsafe { SetTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER, DISPLAY_DEBOUNCE_MS, None) };
            }
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == DISPLAY_DEBOUNCE_TIMER => {
            if ctrl.last_display_event.elapsed() >= Duration::from_millis(u64::from(DISPLAY_DEBOUNCE_MS / 2)) {
                let _ = unsafe { KillTimer(Some(hwnd), DISPLAY_DEBOUNCE_TIMER) };
                ctrl.handle_topology_refresh();
            }
            LRESULT(0)
        }
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            ctrl.persist_now();
            LRESULT(1)
        }
        WM_DESTROY => {
            ctrl.persist_now();
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    let _instance_guard = match acquire_single_instance() {
        Some(g) => g,
        None => return Ok(()),
    };

    enable_per_monitor_dpi_v2();
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    FenceWindow::register_class()?;

    let hinst = unsafe { GetModuleHandleW(None) }?;
    let cls = w!("DeskLiteControllerWnd");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(controller_wnd_proc),
        hInstance: hinst.into(),
        lpszClassName: cls,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassExW(&wc) };

    let layout_path = default_layout_path();
    let topology = probe_display_topology()?;
    let config = load_or_bootstrap_layout(&layout_path, &topology);
    let pending_actions = Rc::new(RefCell::new(VecDeque::new()));
    let query_ctx = Rc::new(RefCell::new(LayoutQueryContext::default()));

    // Create controller window with null USERDATA first, then attach Box<AppController>
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            cls,
            w!("DeskLite Controller"),
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
    }?;

    let shared_ui = Rc::new(RefCell::new(SharedUiResources::new(
        hwnd,
        Rc::clone(&pending_actions),
        Rc::clone(&query_ctx),
    )?));

    let mut ctrl = Box::new(AppController {
        hwnd,
        layout_path,
        config,
        topology,
        shared_ui,
        pending_actions,
        query_ctx,
        windows: HashMap::new(),
        watchers: HashMap::new(),
        last_display_event: Instant::now(),
    });
    ctrl.sync_all_windows();

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&raw mut *ctrl) as isize);
    }

    let _tray = TrayIcon::new(hwnd, 1, "DeskLite 桌面整理")?;

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    ctrl.persist_now();
    ctrl.windows.clear();
    ctrl.watchers.clear();
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        CoUninitialize();
    }
    Ok(())
}

fn main() {
    let _ = run_app();
}
