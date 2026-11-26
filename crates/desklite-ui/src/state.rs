use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use desklite_core::{
    DesktopIconPos, DisplayTopology, FenceItem, FenceRole, FenceSpec, MIN_FENCE_HEIGHT,
    MIN_FENCE_WIDTH, NativeLayoutConfig, Point, Rect, clamp_point_to_monitors,
    clamp_rect_to_monitors, normalize_path_key,
};
use desklite_win32::{
    DragTargetOutcome, IconCache, all_desktop_dirs, is_ctrl_key_down, is_directly_on_desktop,
    paste_files_to_directory_detailed, resolve_dropped_item, scan_portal_directory,
    set_window_rect_no_z, trash_files, user_desktop_dir,
};
use egui::{
    Color32, ColorImage, CornerRadius, Pos2, Stroke, TextureHandle, TextureOptions, Vec2,
    ViewportCommand, ViewportId,
};

pub const COLLAPSED_HEIGHT: f32 = 38.0;
pub const INSPECTOR_WIDTH: f32 = 408.0;
pub const INSPECTOR_HEIGHT: f32 = 568.0;
pub const INSPECTOR_TITLE: &str = "DeskLite 参数设置";
pub const FREE_DESKTOP_TITLE: &str = "DeskLite_FreeDesktop";
pub const RESIZE_EDGE: f32 = 6.0;

/// Acquires a mutex guard, recovering inner data cleanly if a prior holder panicked.
pub fn lock_unpoisoned<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceCommand {
    RequestCreateBlank,
    RequestCreateUnfiledDesktop,
    RequestCreatePortal,
    RequestBindPortal(i32),
    RequestDelete(i32),
    RequestExitApp,
    SyncPortals,
    RefreshDesktop,
    ToggleHideAllFences,
    ShowFenceMenu {
        fence_id: i32,
        screen_x: i32,
        screen_y: i32,
        include_body_actions: bool,
    },
    ShowFreeDesktopMenu {
        screen_x: i32,
        screen_y: i32,
    },
    ShowFreeIconMenu {
        paths: Vec<String>,
        screen_x: i32,
        screen_y: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapsedMenuCommand {
    ToggleCollapse,
    RenameFence,
    ToggleKind,
    ToggleLock,
    DuplicateStyle,
    OpenParamsEditor,
    OpenPortalFolder,
    UnbindPortal,
    BindPortal,
    SortName,
    SortKind,
    SortModified,
    PasteFromClipboard,
    UndoLast,
    SelectAll,
    RefreshPortal,
    CreateBlank,
    CreateUnfiledDesktop,
    CreatePortal,
    DeleteFence,
    ExitApp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeDesktopMenuCommand {
    CreateBlank,
    CreateUnfiledDesktop,
    CreatePortal,
    ToggleHideAllFences,
    ArrangeByName,
    ArrangeByKind,
    ArrangeByModified,
    RefreshDesktop,
    PasteToDesktop,
    UndoLast,
    OpenDesktopFolder,
    ExitApp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeIconMenuCommand {
    Open,
    OpenAsAdmin,
    RevealInExplorer,
    Copy { cut: bool },
    CopyPaths,
    Rename,
    MoveToFence(i32),
    Trash,
    Properties,
}

#[derive(Debug, Clone)]
pub enum UndoAction {
    RenameItem {
        fence_id: Option<i32>,
        old_path: String,
        new_path: String,
    },
    RenameFence {
        fence_id: i32,
        old_name: String,
    },
    RemoveItems {
        fence_id: i32,
        items: Vec<FenceItem>,
    },
    AddItems {
        fence_id: i32,
        added_paths: Vec<String>,
    },
    MoveBetweenBuckets {
        src_fence_id: i32,
        dst_fence_id: i32,
        items: Vec<FenceItem>,
    },
    MoveFreeIconsToBucket {
        dst_fence_id: i32,
        items: Vec<FenceItem>,
        prev_free_icons: Vec<DesktopIconPos>,
    },
    ReleaseBucketToFree {
        src_fence_id: i32,
        items: Vec<FenceItem>,
    },
    PasteFiles {
        fence_id: Option<i32>,
        created_paths: Vec<String>,
        moved_pairs: Vec<(String, String)>,
        removed_from_fences: Vec<(i32, Vec<FenceItem>)>,
        added_paths: Vec<String>,
        is_portal: bool,
    },
    ReorderItems {
        fence_id: i32,
        old_items: Vec<FenceItem>,
    },
}

#[derive(Debug, Clone)]
pub struct RenamePrompt {
    pub fence_id: i32,
    pub draft: String,
    pub needs_focus: bool,
}

#[derive(Debug, Clone)]
pub struct ItemRenamePrompt {
    pub fence_id: i32,
    pub item_idx: usize,
    pub original_path: String,
    pub draft: String,
    pub needs_focus: bool,
}

#[derive(Debug, Clone)]
pub struct FreeItemRenamePrompt {
    pub original_path: String,
    pub draft: String,
    pub needs_focus: bool,
}

#[derive(Debug, Clone)]
pub struct ReorderDragState {
    pub trigger_idx: usize,
    pub indices: Vec<usize>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DragSourceState {
    pub fence_id: Option<i32>,
    pub paths: Vec<String>,
    pub is_desktop_resident: bool,
    pub finished_at: Option<Instant>,
}

pub struct AsyncDragSession {
    pub source_fence_id: Option<i32>,
    pub paths: Vec<String>,
    pub is_desktop_resident: bool,
    pub rx: Receiver<DragTargetOutcome>,
}

#[derive(Debug, Clone)]
pub struct MarqueeState {
    pub origin: Pos2,
    pub current: Pos2,
    pub additive: bool,
    pub base_selection: HashSet<usize>,
}

#[derive(Debug, Clone)]
pub struct FreeMarqueeState {
    pub origin: Pos2,
    pub current: Pos2,
    pub additive: bool,
    pub base_selection: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct FreeIconDragState {
    pub origin_screen: (i32, i32),
    pub last_screen: (i32, i32),
    pub paths: Vec<String>,
    pub start_positions: HashMap<String, Point>,
    pub moved: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PopupOwnerState {
    pub popup_id: egui::Id,
    pub saw_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorState {
    pub open: bool,
    pub target_fence_id: i32,
    pub initial_pos: (i32, i32),
    pub hwnd: isize,
    pub styled: bool,
    pub drag_grab: Option<(i32, i32)>,
    pub draft_name: Option<(i32, String)>,
    pub name_edit_initial: Option<(i32, String)>,
}

#[derive(Debug, Clone, Copy)]
pub struct TitleDragState {
    pub fence_id: i32,
    pub grab_dx: i32,
    pub grab_dy: i32,
    pub width: i32,
    pub height: i32,
}

pub struct SharedFenceState {
    pub config: NativeLayoutConfig,
    pub topology: DisplayTopology,
    pub desktop_dirs: Vec<String>,
    pub icon_cache: IconCache,
    pub textures: HashMap<String, Option<TextureHandle>>,
    pub attached_hwnds: HashMap<i32, isize>,
    pub hwnd_to_fence: HashMap<isize, i32>,
    pub free_desktop_hwnd: isize,
    pub builder_rects: HashMap<i32, (Rect, bool)>,
    pub rename_prompt: Option<RenamePrompt>,
    pub item_rename_prompt: Option<ItemRenamePrompt>,
    pub free_rename_prompt: Option<FreeItemRenamePrompt>,
    pub inspector: Option<InspectorState>,
    pub inspector_spawned: bool,
    pub selected_items: HashMap<i32, HashSet<usize>>,
    pub selection_anchor: HashMap<i32, usize>,
    pub marquee_drag: HashMap<i32, MarqueeState>,
    pub item_reorder: HashMap<i32, ReorderDragState>,
    pub free_selected_paths: HashSet<String>,
    pub free_selection_anchor: Option<String>,
    pub free_marquee: Option<FreeMarqueeState>,
    pub free_icon_drag: Option<FreeIconDragState>,
    pub free_type_ahead: Option<(String, Instant)>,
    pub free_last_bg_click: Option<Instant>,
    pub free_toast: Option<(String, Instant)>,
    pub title_drag: Option<TitleDragState>,
    pub pending_toggle_collapse: HashSet<i32>,
    pub pending_collapse_until: HashMap<i32, Instant>,
    pub topology_settling_until: Option<Instant>,
    pub undo_stack: Vec<UndoAction>,
    pub cut_paths: HashSet<String>,
    pub scroll_to_item: HashMap<i32, usize>,
    pub type_ahead: HashMap<i32, (String, Instant)>,
    pub toast_msg: HashMap<i32, (String, Instant)>,
    pub last_title_click: HashMap<i32, Instant>,
    pub dragging_from_fence: Option<DragSourceState>,
    pub active_async_drag: Option<AsyncDragSession>,
    pub drag_notify_hwnd: Option<isize>,
    pub drag_notify_msg: u32,
    pub pending_immediate_save_from_async: bool,
    pub pending_repaint_fences: HashSet<i32>,
    pub pending_repaint_free_desktop: bool,
    pub popup_owners: HashMap<i32, PopupOwnerState>,
    pub pending_commands: Vec<FenceCommand>,
    pub dirty_layout: bool,
    pub pending_save_at: Option<Instant>,
}

impl SharedFenceState {
    pub fn new(config: NativeLayoutConfig, topology: DisplayTopology, icon_cache: IconCache) -> Self {
        let desktop_dirs: Vec<String> = all_desktop_dirs()
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let builder_rects = config
            .fences
            .iter()
            .map(|f| (f.id, (f.rect, f.collapsed)))
            .collect();
        Self {
            config,
            topology,
            desktop_dirs,
            icon_cache,
            textures: HashMap::new(),
            attached_hwnds: HashMap::new(),
            hwnd_to_fence: HashMap::new(),
            free_desktop_hwnd: 0,
            builder_rects,
            rename_prompt: None,
            item_rename_prompt: None,
            free_rename_prompt: None,
            inspector: None,
            inspector_spawned: false,
            selected_items: HashMap::new(),
            selection_anchor: HashMap::new(),
            marquee_drag: HashMap::new(),
            item_reorder: HashMap::new(),
            free_selected_paths: HashSet::new(),
            free_selection_anchor: None,
            free_marquee: None,
            free_icon_drag: None,
            free_type_ahead: None,
            free_last_bg_click: None,
            free_toast: None,
            title_drag: None,
            pending_toggle_collapse: HashSet::new(),
            pending_collapse_until: HashMap::new(),
            topology_settling_until: None,
            undo_stack: Vec::new(),
            cut_paths: HashSet::new(),
            scroll_to_item: HashMap::new(),
            type_ahead: HashMap::new(),
            toast_msg: HashMap::new(),
            last_title_click: HashMap::new(),
            dragging_from_fence: None,
            active_async_drag: None,
            drag_notify_hwnd: None,
            drag_notify_msg: 0,
            pending_immediate_save_from_async: false,
            pending_repaint_fences: HashSet::new(),
            pending_repaint_free_desktop: false,
            popup_owners: HashMap::new(),
            pending_commands: Vec::new(),
            dirty_layout: false,
            pending_save_at: None,
        }
    }

    pub fn set_desktop_dirs(&mut self, dirs: Vec<String>) {
        self.desktop_dirs = dirs;
        self.config.normalize_roles(&self.desktop_dirs);
    }

    pub fn set_drag_notify_target(&mut self, helper_hwnd: isize, notify_msg: u32) {
        self.drag_notify_hwnd = (helper_hwnd != 0).then_some(helper_hwnd);
        self.drag_notify_msg = notify_msg;
    }

    pub fn begin_topology_settling(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        self.topology_settling_until = Some(
            self.topology_settling_until
                .map_or(deadline, |existing| existing.max(deadline)),
        );
    }

    pub fn finish_topology_settling(&mut self, cooldown: Duration) {
        if cooldown.is_zero() {
            self.topology_settling_until = None;
        } else {
            self.topology_settling_until = Some(Instant::now() + cooldown);
        }
    }

    pub fn is_topology_settling(&self) -> bool {
        self.topology_settling_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn register_fence_hwnd(&mut self, fence_id: i32, raw_hwnd: isize) {
        if let Some(prev) = self.attached_hwnds.insert(fence_id, raw_hwnd)
            && prev != raw_hwnd
        {
            self.hwnd_to_fence.remove(&prev);
        }
        self.hwnd_to_fence.insert(raw_hwnd, fence_id);
    }

    pub fn push_undo(&mut self, action: UndoAction) {
        self.undo_stack.push(action);
        if self.undo_stack.len() > 30 {
            let drain_n = self.undo_stack.len() - 30;
            self.undo_stack.drain(..drain_n);
        }
    }

    pub fn push_command(&mut self, ctx: &egui::Context, cmd: FenceCommand) {
        self.pending_commands.push(cmd);
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    pub fn mark_debounced_save(&mut self, ctx: &egui::Context) {
        self.dirty_layout = true;
        if ctx.input(|i| i.pointer.any_down()) {
            let delay = Duration::from_millis(500);
            self.pending_save_at = Some(Instant::now() + delay);
            ctx.request_repaint_after_for(delay, ViewportId::ROOT);
        } else {
            self.pending_save_at = Some(Instant::now() + Duration::from_millis(300));
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    pub fn invalidate_icon_texture(&mut self, path: &str) {
        let key = normalize_path_key(path);
        if !key.is_empty() {
            self.textures.remove(&key);
        }
        self.textures.remove(&path.to_ascii_lowercase());
        self.icon_cache.invalidate_path(Path::new(path));
    }

    pub fn get_or_load_texture(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        let key = path.to_ascii_lowercase();
        if let Some(Some(handle)) = self.textures.get(&key) {
            return Some(handle.clone());
        }
        let decoded = self.icon_cache.get_or_load(path)?.clone();
        let mut rgba = decoded.pbgra;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let img = ColorImage::from_rgba_premultiplied(
            [decoded.width as usize, decoded.height as usize],
            &rgba,
        );
        let handle = ctx.load_texture(key.clone(), img, TextureOptions::LINEAR);
        if self.textures.len() > 512 {
            self.textures.clear();
        }
        self.textures.insert(key, Some(handle.clone()));
        Some(handle)
    }

    pub fn resolve_item_textures(
        &mut self,
        ctx: &egui::Context,
        items: &[FenceItem],
    ) -> Vec<Option<TextureHandle>> {
        items
            .iter()
            .map(|it| self.get_or_load_texture(ctx, &it.path))
            .collect()
    }

    pub fn sync_builder_rects_from_config(&mut self) {
        self.builder_rects = self
            .config
            .fences
            .iter()
            .map(|f| (f.id, (f.rect, f.collapsed)))
            .collect();
    }

    pub fn prune_removed_fences(&mut self) {
        let active_ids: HashSet<i32> = self.config.fences.iter().map(|f| f.id).collect();
        self.attached_hwnds.retain(|id, _| active_ids.contains(id));
        self.hwnd_to_fence.retain(|_, id| active_ids.contains(id));
        self.builder_rects.retain(|id, _| active_ids.contains(id));
        self.selected_items.retain(|id, _| active_ids.contains(id));
        self.selection_anchor.retain(|id, _| active_ids.contains(id));
        self.marquee_drag.retain(|id, _| active_ids.contains(id));
        self.item_reorder.retain(|id, _| active_ids.contains(id));
        self.scroll_to_item.retain(|id, _| active_ids.contains(id));
        self.type_ahead.retain(|id, _| active_ids.contains(id));
        self.toast_msg.retain(|id, _| active_ids.contains(id));
        self.popup_owners.retain(|id, _| active_ids.contains(id));
        self.pending_toggle_collapse.retain(|id| active_ids.contains(id));
        self.pending_collapse_until.retain(|id, _| active_ids.contains(id));
        if self
            .item_rename_prompt
            .as_ref()
            .is_some_and(|r| !active_ids.contains(&r.fence_id))
        {
            self.item_rename_prompt = None;
        }
        if self
            .inspector
            .as_ref()
            .is_some_and(|ins| !active_ids.contains(&ins.target_fence_id))
        {
            if let Some(&first_id) = active_ids.iter().next() {
                if let Some(ref mut ins) = self.inspector {
                    ins.target_fence_id = first_id;
                    ins.draft_name = None;
                    ins.name_edit_initial = None;
                }
            } else if let Some(ref mut ins) = self.inspector {
                ins.open = false;
                ins.draft_name = None;
                ins.name_edit_initial = None;
            }
        }
    }

    /// Reconciles `free_icons` against all scanned desktop items, preserving existing free-icon
    /// coordinates and assigning non-overlapping grid slots for newly unfiled items.
    pub fn reconcile_free_icons_from_desktop(&mut self, scanned_desktop: &[FenceItem]) {
        let cell = (
            self.config.preferences.icon_cell_w.clamp(48, 180),
            self.config.preferences.icon_cell_h.clamp(56, 200),
        );
        let origin = self
            .topology
            .monitors()
            .first()
            .map(|m| Point {
                x: m.work_area.left + 20,
                y: m.work_area.top + 20,
            })
            .unwrap_or(Point { x: 20, y: 20 });
        let monitors = self.topology.monitors().to_vec();
        let desktop_dirs = self.desktop_dirs.clone();
        let _ = self.config.reconcile_free_icons(
            scanned_desktop,
            origin,
            cell,
            &monitors,
            &desktop_dirs,
        );
    }

    /// Refreshes any `UnfiledDesktop` fences so they mirror `(all desktop items) \ (bucket items)`.
    pub fn refresh_unfiled_desktop_views(&mut self, scanned_desktop: &[FenceItem]) {
        let owned = self.config.all_bucket_desktop_paths(&self.desktop_dirs);
        let desktop_dirs = self.desktop_dirs.clone();
        for f in &mut self.config.fences {
            if f.is_unfiled_desktop(&desktop_dirs) {
                let mut next_items = Vec::new();
                for it in scanned_desktop {
                    let key = normalize_path_key(&it.path);
                    if !key.is_empty() && !owned.contains(&key) {
                        let mut item = it.clone();
                        item.order = next_items.len() as i32;
                        next_items.push(item);
                    }
                }
                f.items = next_items;
            }
        }
    }

    /// Polls any active non-blocking OLE `DoDragDrop` session and applies its target outcome.
    /// Returns `true` if configuration/layout state changed and needs saving/repaint.
    pub fn poll_async_drag(&mut self) -> bool {
        let Some(session) = self.active_async_drag.take() else {
            return false;
        };
        let outcome = match session.rx.try_recv() {
            Ok(out) => out,
            Err(TryRecvError::Empty) => {
                self.active_async_drag = Some(session);
                return false;
            }
            Err(TryRecvError::Disconnected) => DragTargetOutcome::Cancelled,
        };
        self.dragging_from_fence = None;
        let changed = self.apply_drag_target_outcome(
            session.source_fence_id,
            session.paths,
            session.is_desktop_resident,
            outcome,
        );
        if changed {
            self.dirty_layout = true;
            self.pending_immediate_save_from_async = true;
        }
        changed
    }

    fn apply_drag_target_outcome(
        &mut self,
        source_fence_id: Option<i32>,
        paths: Vec<String>,
        is_desktop_resident: bool,
        outcome: DragTargetOutcome,
    ) -> bool {
        if paths.is_empty() {
            return false;
        }
        let desktop_dirs = self.desktop_dirs.clone();
        match outcome {
            DragTargetOutcome::Cancelled => false,
            DragTargetOutcome::DroppedOnFence { target_hwnd, .. } => {
                let Some(&target_fid) = self.hwnd_to_fence.get(&target_hwnd) else {
                    return false;
                };
                if source_fence_id == Some(target_fid) {
                    return false;
                }
                let Some(target_role) = self
                    .config
                    .fences
                    .iter()
                    .find(|f| f.id == target_fid)
                    .map(|f| f.effective_role(&desktop_dirs))
                else {
                    return false;
                };
                let src_role = source_fence_id.and_then(|sid| {
                    self.config
                        .fences
                        .iter()
                        .find(|f| f.id == sid)
                        .map(|f| f.effective_role(&desktop_dirs))
                });

                match target_role {
                    FenceRole::DesktopBucket => {
                        let mut changed = false;
                        let mut items: Vec<FenceItem> = Vec::with_capacity(paths.len());
                        match src_role {
                            Some(FenceRole::ExternalPortal { .. }) => {
                                if let Some(desktop) = user_desktop_dir()
                                    && let Ok(pairs) = paste_files_to_directory_detailed(
                                        &paths,
                                        &desktop,
                                        false,
                                    )
                                {
                                    let mut created = Vec::new();
                                    let mut added_paths = Vec::new();
                                    for (_, dst) in pairs {
                                        let ds = dst.to_string_lossy().into_owned();
                                        created.push(ds.clone());
                                        added_paths.push(ds.clone());
                                        items.push(resolve_dropped_item(&dst, items.len() as i32));
                                    }
                                    if self.config.assign_desktop_items_to_bucket(
                                        target_fid,
                                        items,
                                        &desktop_dirs,
                                    ) {
                                        self.push_undo(UndoAction::PasteFiles {
                                            fence_id: Some(target_fid),
                                            created_paths: created,
                                            moved_pairs: Vec::new(),
                                            removed_from_fences: Vec::new(),
                                            added_paths,
                                            is_portal: false,
                                        });
                                        changed = true;
                                    }
                                }
                            }
                            Some(FenceRole::DesktopBucket) => {
                                let src_fid = source_fence_id.unwrap();
                                for p in &paths {
                                    let existing_item = self
                                        .config
                                        .fences
                                        .iter()
                                        .find(|f| f.id == src_fid)
                                        .and_then(|f| {
                                            f.items.iter().find(|it| {
                                                normalize_path_key(&it.path) == normalize_path_key(p)
                                            })
                                        })
                                        .cloned()
                                        .unwrap_or_else(|| {
                                            resolve_dropped_item(Path::new(p), items.len() as i32)
                                        });
                                    items.push(existing_item);
                                }
                                if self.config.assign_desktop_items_to_bucket(
                                    target_fid,
                                    items.clone(),
                                    &desktop_dirs,
                                ) {
                                    self.selected_items.remove(&src_fid);
                                    self.selection_anchor.remove(&src_fid);
                                    self.push_undo(UndoAction::MoveBetweenBuckets {
                                        src_fence_id: src_fid,
                                        dst_fence_id: target_fid,
                                        items,
                                    });
                                    self.pending_repaint_fences.insert(src_fid);
                                    changed = true;
                                }
                            }
                            Some(FenceRole::UnfiledDesktop) | None => {
                                let prev_free_icons: Vec<DesktopIconPos> = self
                                    .config
                                    .active_free_icons()
                                    .iter()
                                    .filter(|ic| {
                                        paths
                                            .iter()
                                            .any(|p| normalize_path_key(p) == normalize_path_key(&ic.id))
                                    })
                                    .cloned()
                                    .collect();
                                for p in &paths {
                                    items.push(resolve_dropped_item(Path::new(p), items.len() as i32));
                                }
                                let added_paths: Vec<String> =
                                    items.iter().map(|it| it.path.clone()).collect();
                                if self.config.assign_desktop_items_to_bucket(
                                    target_fid,
                                    items.clone(),
                                    &desktop_dirs,
                                ) {
                                    for p in &paths {
                                        self.free_selected_paths.remove(&normalize_path_key(p));
                                    }
                                    if source_fence_id.is_none() {
                                        self.push_undo(UndoAction::MoveFreeIconsToBucket {
                                            dst_fence_id: target_fid,
                                            items,
                                            prev_free_icons,
                                        });
                                    } else {
                                        self.push_undo(UndoAction::AddItems {
                                            fence_id: target_fid,
                                            added_paths,
                                        });
                                    }
                                    changed = true;
                                }
                            }
                        }
                        if changed {
                            self.refresh_all_unfiled_from_disk_if_present();
                            self.pending_repaint_fences.insert(target_fid);
                            self.pending_repaint_free_desktop = true;
                        }
                        changed
                    }
                    FenceRole::UnfiledDesktop => {
                        if let Some(src_fid) = source_fence_id {
                            match src_role {
                                Some(FenceRole::DesktopBucket) => {
                                    let released_items: Vec<FenceItem> = self
                                        .config
                                        .fences
                                        .iter()
                                        .find(|f| f.id == src_fid)
                                        .map(|f| {
                                            f.items
                                                .iter()
                                                .filter(|it| {
                                                    paths.iter().any(|p| {
                                                        normalize_path_key(p)
                                                            == normalize_path_key(&it.path)
                                                    })
                                                })
                                                .cloned()
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    if self.config.release_desktop_items(
                                        src_fid,
                                        &paths,
                                        &desktop_dirs,
                                    ) > 0
                                    {
                                        self.selected_items.remove(&src_fid);
                                        self.selection_anchor.remove(&src_fid);
                                        self.place_released_icons_on_free_desktop(&paths, None);
                                        if !released_items.is_empty() {
                                            self.push_undo(UndoAction::ReleaseBucketToFree {
                                                src_fence_id: src_fid,
                                                items: released_items,
                                            });
                                        }
                                        self.refresh_all_unfiled_from_disk_if_present();
                                        self.pending_repaint_fences.insert(src_fid);
                                        self.pending_repaint_fences.insert(target_fid);
                                        self.pending_repaint_free_desktop = true;
                                        return true;
                                    }
                                }
                                Some(FenceRole::ExternalPortal { .. }) => {
                                    if let Some(desktop) = user_desktop_dir()
                                        && let Ok(pairs) = paste_files_to_directory_detailed(
                                            &paths,
                                            &desktop,
                                            false,
                                        )
                                        && !pairs.is_empty()
                                    {
                                        let created: Vec<String> = pairs
                                            .iter()
                                            .map(|(_, d)| d.to_string_lossy().into_owned())
                                            .collect();
                                        self.place_released_icons_on_free_desktop(&created, None);
                                        self.refresh_all_unfiled_from_disk_if_present();
                                        self.pending_repaint_fences.insert(target_fid);
                                        self.pending_repaint_free_desktop = true;
                                        return true;
                                    }
                                }
                                _ => {}
                            }
                        }
                        false
                    }
                    FenceRole::ExternalPortal { dir } => {
                        let is_move = is_desktop_resident && !is_ctrl_key_down();
                        if let Ok(pairs) =
                            paste_files_to_directory_detailed(&paths, Path::new(&dir), is_move)
                            && !pairs.is_empty()
                        {
                            let mut moved_pairs = Vec::new();
                            let mut created_paths = Vec::new();
                            for (s, d) in &pairs {
                                if is_move {
                                    moved_pairs.push((
                                        s.to_string_lossy().into_owned(),
                                        d.to_string_lossy().into_owned(),
                                    ));
                                } else {
                                    created_paths.push(d.to_string_lossy().into_owned());
                                }
                            }
                            let mut removed_from_fences = Vec::new();
                            if is_move {
                                for (s, _) in &pairs {
                                    let ss = s.to_string_lossy().into_owned();
                                    let _ = self.config.remove_free_icon(&ss);
                                    for f in &mut self.config.fences {
                                        if f.is_desktop_bucket(&desktop_dirs) {
                                            let mut rem = Vec::new();
                                            let mut kept = Vec::with_capacity(f.items.len());
                                            for it in f.items.drain(..) {
                                                if normalize_path_key(&it.path)
                                                    == normalize_path_key(&ss)
                                                {
                                                    rem.push(it);
                                                } else {
                                                    kept.push(it);
                                                }
                                            }
                                            if !rem.is_empty() {
                                                for (i, it) in kept.iter_mut().enumerate() {
                                                    it.order = i as i32;
                                                }
                                                removed_from_fences.push((f.id, rem));
                                                self.pending_repaint_fences.insert(f.id);
                                            }
                                            f.items = kept;
                                        }
                                    }
                                }
                            }
                            if let Some(tf) = self.config.fence_mut(target_fid) {
                                tf.items = scan_portal_directory(Path::new(&dir));
                            }
                            self.push_undo(UndoAction::PasteFiles {
                                fence_id: Some(target_fid),
                                created_paths,
                                moved_pairs,
                                removed_from_fences,
                                added_paths: Vec::new(),
                                is_portal: true,
                            });
                            self.pending_repaint_fences.insert(target_fid);
                            self.pending_repaint_free_desktop = true;
                            return true;
                        }
                        false
                    }
                }
            }
            DragTargetOutcome::DroppedOnFreeDesktop { screen_x, screen_y } => {
                let mut changed = false;
                if let Some(src_fid) = source_fence_id {
                    let src_role = self
                        .config
                        .fences
                        .iter()
                        .find(|f| f.id == src_fid)
                        .map(|f| f.effective_role(&desktop_dirs));
                    match src_role {
                        Some(FenceRole::DesktopBucket) => {
                            let released_items: Vec<FenceItem> = self
                                .config
                                .fences
                                .iter()
                                .find(|f| f.id == src_fid)
                                .map(|f| {
                                    f.items
                                        .iter()
                                        .filter(|it| {
                                            paths.iter().any(|p| {
                                                normalize_path_key(p)
                                                    == normalize_path_key(&it.path)
                                            })
                                        })
                                        .cloned()
                                        .collect()
                                })
                                .unwrap_or_default();
                            if self
                                .config
                                .release_desktop_items(src_fid, &paths, &desktop_dirs)
                                > 0
                            {
                                self.selected_items.remove(&src_fid);
                                self.selection_anchor.remove(&src_fid);
                                self.place_released_icons_on_free_desktop(
                                    &paths,
                                    Some((screen_x, screen_y)),
                                );
                                if !released_items.is_empty() {
                                    self.push_undo(UndoAction::ReleaseBucketToFree {
                                        src_fence_id: src_fid,
                                        items: released_items,
                                    });
                                }
                                self.pending_repaint_fences.insert(src_fid);
                                changed = true;
                            }
                        }
                        Some(FenceRole::UnfiledDesktop) => {
                            self.place_released_icons_on_free_desktop(
                                &paths,
                                Some((screen_x, screen_y)),
                            );
                            changed = true;
                        }
                        Some(FenceRole::ExternalPortal { .. }) => {
                            if let Some(desktop) = user_desktop_dir()
                                && let Ok(pairs) =
                                    paste_files_to_directory_detailed(&paths, &desktop, false)
                                && !pairs.is_empty()
                            {
                                let created: Vec<String> = pairs
                                    .iter()
                                    .map(|(_, d)| d.to_string_lossy().into_owned())
                                    .collect();
                                self.place_released_icons_on_free_desktop(
                                    &created,
                                    Some((screen_x, screen_y)),
                                );
                                self.push_undo(UndoAction::PasteFiles {
                                    fence_id: None,
                                    created_paths: created,
                                    moved_pairs: Vec::new(),
                                    removed_from_fences: Vec::new(),
                                    added_paths: Vec::new(),
                                    is_portal: false,
                                });
                                changed = true;
                            }
                        }
                        None => {}
                    }
                } else {
                    self.place_released_icons_on_free_desktop(&paths, Some((screen_x, screen_y)));
                    changed = true;
                }
                if changed {
                    self.refresh_all_unfiled_from_disk_if_present();
                    self.pending_repaint_free_desktop = true;
                }
                changed
            }
            DragTargetOutcome::DroppedExternal { moved } => {
                if !moved {
                    return false;
                }
                let mut changed = false;
                for p in &paths {
                    if !Path::new(p).exists() {
                        if self.config.remove_desktop_path_everywhere(p, &desktop_dirs) {
                            changed = true;
                        }
                        self.free_selected_paths.remove(&normalize_path_key(p));
                    }
                }
                if let Some(src_fid) = source_fence_id {
                    self.selected_items.remove(&src_fid);
                    self.selection_anchor.remove(&src_fid);
                    self.pending_repaint_fences.insert(src_fid);
                }
                if changed {
                    self.refresh_all_unfiled_from_disk_if_present();
                    self.pending_repaint_free_desktop = true;
                }
                changed
            }
        }
    }

    pub fn place_released_icons_on_free_desktop(
        &mut self,
        paths: &[String],
        drop_pos: Option<(i32, i32)>,
    ) {
        let monitors = self.topology.monitors().to_vec();
        let desktop_dirs = self.desktop_dirs.clone();
        let step_x = self.config.preferences.icon_cell_w.clamp(48, 180);
        let step_y = self.config.preferences.icon_cell_h.clamp(56, 200);
        let (base_x, base_y) = drop_pos.unwrap_or_else(|| {
            monitors
                .first()
                .map(|m| (m.work_area.left + 28, m.work_area.top + 28))
                .unwrap_or((28, 28))
        });
        for (i, p) in paths.iter().enumerate() {
            let path = Path::new(p);
            let in_configured_desktop = path.parent().is_some_and(|par| {
                let par_key = normalize_path_key(&par.to_string_lossy());
                !par_key.is_empty()
                    && desktop_dirs
                        .iter()
                        .any(|d| normalize_path_key(d) == par_key)
            });
            if !is_directly_on_desktop(path) && !in_configured_desktop {
                continue;
            }
            let item = resolve_dropped_item(Path::new(p), i as i32);
            let ox = (i as i32 % 4) * step_x;
            let oy = (i as i32 / 4) * step_y;
            let raw_pt = Point {
                x: base_x - step_x / 2 + ox,
                y: base_y - step_y / 2 + oy,
            };
            let pos = if monitors.is_empty() {
                raw_pt
            } else {
                clamp_point_to_monitors(raw_pt, (step_x, step_y), &monitors)
                    .map(|(pt, _)| pt)
                    .unwrap_or(raw_pt)
            };
            let _ = self.config.upsert_free_icon(
                DesktopIconPos {
                    id: item.path,
                    display_name: item.display_name,
                    pos,
                },
                &desktop_dirs,
            );
        }
    }

    pub fn refresh_all_unfiled_from_disk_if_present(&mut self) {
        let has_unfiled = self
            .config
            .fences
            .iter()
            .any(|f| f.is_unfiled_desktop(&self.desktop_dirs));
        if !has_unfiled {
            return;
        }
        let mut scanned = Vec::new();
        for dir in &self.desktop_dirs {
            scanned.extend(scan_portal_directory(Path::new(dir)));
        }
        self.refresh_unfiled_desktop_views(&scanned);
    }
}

/// Collects normalized paths of desktop-resident items owned exclusively by `DesktopBucket` fences
/// (excludes `UnfiledDesktop` and `ExternalPortal`).
pub fn collect_managed_desktop_paths(config: &NativeLayoutConfig) -> Vec<String> {
    let dirs: Vec<String> = all_desktop_dirs()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    collect_managed_desktop_paths_for_dirs(config, &dirs)
}

pub fn collect_managed_desktop_paths_for_dirs(
    config: &NativeLayoutConfig,
    desktop_dirs: &[String],
) -> Vec<String> {
    config
        .fences
        .iter()
        .filter(|f| f.is_desktop_bucket(desktop_dirs))
        .flat_map(|f| f.items.iter())
        .filter(|it| is_directly_on_desktop(Path::new(&it.path)))
        .map(|it| it.path.clone())
        .collect()
}

#[derive(Debug, Default)]
pub struct FenceUiOutput {
    pub needs_immediate_save: bool,
}

pub fn configure_egui_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.window_fill = Color32::from_rgba_unmultiplied(30, 34, 44, 246);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 55));
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    ctx.set_visuals(visuals);

    ctx.global_style_mut(|style| {
        style.spacing.scroll = egui::style::ScrollStyle::floating();
        style.spacing.scroll.fade.strength = 0.0;
        style.spacing.scroll.fade.size = 0.0;
        style.spacing.scroll.bar_width = 5.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
    });
}

pub fn resolve_fence_layout(
    spec: &FenceSpec,
    prefs: &desklite_core::LayoutPreferences,
) -> (f32, f32, f32, f32) {
    let ov = spec.layout_override.as_ref();
    let cw = ov
        .and_then(|o| o.icon_cell_w)
        .unwrap_or(prefs.icon_cell_w)
        .clamp(64, 160) as f32;
    let ch = ov
        .and_then(|o| o.icon_cell_h)
        .unwrap_or(prefs.icon_cell_h)
        .clamp(72, 180) as f32;
    let rh = ov
        .and_then(|o| o.list_row_h)
        .unwrap_or(prefs.list_row_h)
        .clamp(24, 56) as f32;
    let li = ov
        .and_then(|o| o.list_icon_px)
        .unwrap_or(prefs.list_icon_px)
        .clamp(14, 36) as f32;
    (cw, ch, rh, li)
}

pub fn format_fence_title(spec: &FenceSpec) -> String {
    if spec.name.is_empty() {
        "未命名格子".to_string()
    } else {
        spec.name.clone()
    }
}

pub fn duplicate_fence_with_style(
    src_fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> Option<i32> {
    let mut guard = lock_unpoisoned(shared);
    let src = guard
        .config
        .fences
        .iter()
        .find(|f| f.id == src_fence_id)?
        .clone();
    let new_id = guard.config.next_fence_id();
    let w = src.rect.width().max(MIN_FENCE_WIDTH);
    let h = src.rect.height().max(MIN_FENCE_HEIGHT);
    let candidate = Rect::new(
        src.rect.left + 28,
        src.rect.top + 28,
        src.rect.left + 28 + w,
        src.rect.top + 28 + h,
    );
    let monitors = guard.topology.monitors().to_vec();
    let rect = clamp_rect_to_monitors(candidate, &monitors)
        .map(|(r, _)| r)
        .unwrap_or(candidate);
    let base_name = if src.name.trim().is_empty() {
        "格子".to_string()
    } else {
        src.name.trim().to_string()
    };
    guard.config.upsert_fence(FenceSpec {
        id: new_id,
        name: format!("{base_name} 副本"),
        kind: src.kind,
        rect,
        collapsed: false,
        locked: false,
        role: Some(FenceRole::DesktopBucket),
        portal_dir: None,
        backdrop_alpha: src.backdrop_alpha,
        layout_override: src.layout_override.clone(),
        items: Vec::new(),
    });
    guard.config.update_active_snapshot_rect(new_id, rect);
    guard.sync_builder_rects_from_config();
    guard.dirty_layout = true;
    ctx.request_repaint_of(ViewportId::ROOT);
    Some(new_id)
}

#[allow(clippy::too_many_arguments)]
pub fn apply_param_editor_rect(
    fence_id: i32,
    my_hwnd: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) -> Rect {
    let cw = w.max(MIN_FENCE_WIDTH);
    let ch = h.max(MIN_FENCE_HEIGHT);
    let candidate = Rect::new(x, y, x + cw, y + ch);
    let (final_rect, is_collapsed) = {
        let mut guard = lock_unpoisoned(shared);
        let monitors = guard.topology.monitors().to_vec();
        let final_rect = clamp_rect_to_monitors(candidate, &monitors)
            .map(|(r, _)| r)
            .unwrap_or(candidate);
        let is_collapsed = guard
            .config
            .fences
            .iter()
            .find(|f| f.id == fence_id)
            .is_some_and(|f| f.collapsed);
        if let Some(f) = guard.config.fence_mut(fence_id) {
            f.rect = final_rect;
        }
        guard.config.update_active_snapshot_rect(fence_id, final_rect);
        guard
            .pending_collapse_until
            .insert(fence_id, Instant::now() + Duration::from_millis(500));
        guard.mark_debounced_save(ctx);
        out.needs_immediate_save = !ctx.input(|i| i.pointer.any_down());
        (final_rect, is_collapsed)
    };
    let ppp = ctx.pixels_per_point().max(0.5);
    if my_hwnd != 0 {
        let apply_h = if is_collapsed {
            (COLLAPSED_HEIGHT * ppp).round() as i32
        } else {
            final_rect.height()
        };
        set_window_rect_no_z(
            my_hwnd,
            final_rect.left,
            final_rect.top,
            final_rect.width(),
            apply_h,
        );
    } else {
        ctx.send_viewport_cmd_to(
            ViewportId::from_hash_of(("desklite_fence", fence_id)),
            ViewportCommand::OuterPosition(Pos2::new(
                final_rect.left as f32 / ppp,
                final_rect.top as f32 / ppp,
            )),
        );
        if !is_collapsed {
            ctx.send_viewport_cmd_to(
                ViewportId::from_hash_of(("desklite_fence", fence_id)),
                ViewportCommand::InnerSize(Vec2::new(
                    final_rect.width() as f32 / ppp,
                    final_rect.height() as f32 / ppp,
                )),
            );
        }
    }
    ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fence_id)));
    final_rect
}

pub fn request_repaint_all_fences(shared: &Arc<Mutex<SharedFenceState>>, ctx: &egui::Context) {
    let ids: Vec<i32> = lock_unpoisoned(shared)
        .config
        .fences
        .iter()
        .map(|f| f.id)
        .collect();
    for fid in ids {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
}

pub fn request_repaint_free_desktop(ctx: &egui::Context) {
    ctx.request_repaint_of(ViewportId::from_hash_of("desklite_free_desktop"));
}

pub fn flush_pending_repaints(shared: &Arc<Mutex<SharedFenceState>>, ctx: &egui::Context) {
    let (fids, free) = {
        let mut guard = lock_unpoisoned(shared);
        let fids: Vec<i32> = guard.pending_repaint_fences.drain().collect();
        let free = std::mem::take(&mut guard.pending_repaint_free_desktop);
        (fids, free)
    };
    for fid in fids {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
    if free {
        request_repaint_free_desktop(ctx);
    }
}

pub fn trash_files_silent_error(paths: &[String]) -> Result<bool, String> {
    trash_files(paths).map_err(|e| e.to_string())
}
