use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    DisplayTopology, FenceItem, FenceKind, FenceSpec, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH,
    NativeLayoutConfig, Rect, clamp_rect_to_monitors,
};
use desklite_win32::{
    DragOutOutcome, IconCache, Win32Hotkey, allow_foreground_self,
    attach_desktop_owner_and_rounded_corners, clipboard_has_files, copy_text_to_clipboard,
    find_window_by_exact_title, focus_window_now, foreground_window_handle,
    hide_desktop_icons_for_paths, hide_desktop_icons_for_paths_async,
    hide_desktop_icons_for_paths_delayed, is_ctrl_key_down, is_directly_on_desktop,
    launch_as_admin, launch_item, live_sizemove_rect_for_hwnd, move_path_in_place,
    paste_files_to_directory_detailed, read_clipboard_files, read_clipboard_text,
    rename_file_in_place, resolve_dropped_item, restore_desktop_icon_async,
    restore_desktop_icons_async, reveal_in_explorer, scan_portal_directory,
    set_active_text_edit_hwnd, set_clipboard_files_for_window, set_hwnd_locked,
    set_window_rect_no_z, show_file_properties, start_file_drag_out,
    style_floating_inspector_window, take_win32_hotkeys, trash_files, user_desktop_dir,
};
use egui::{
    Align, Color32, ColorImage, CornerRadius, CursorIcon, Frame, Key, Layout, Margin,
    PointerButton, Pos2, ResizeDirection, ScrollArea, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2, ViewportCommand, ViewportId, text::LayoutJob,
};

use crate::snap::snap_move_rect;

pub const COLLAPSED_HEIGHT: f32 = 38.0;
pub const INSPECTOR_WIDTH: f32 = 408.0;
pub const INSPECTOR_HEIGHT: f32 = 568.0;
pub const INSPECTOR_TITLE: &str = "DeskLite 参数设置";
const RESIZE_EDGE: f32 = 6.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceCommand {
    RequestCreateBlank,
    RequestCreatePortal,
    RequestBindPortal(i32),
    RequestDelete(i32),
    RequestExitApp,
    SyncPortals,
    ShowFenceMenu {
        fence_id: i32,
        screen_x: i32,
        screen_y: i32,
        include_body_actions: bool,
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
    CreatePortal,
    DeleteFence,
    ExitApp,
}

#[derive(Debug, Clone)]
pub enum UndoAction {
    RenameItem {
        fence_id: i32,
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
    PasteFiles {
        fence_id: i32,
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
pub struct ReorderDragState {
    pub trigger_idx: usize,
    pub indices: Vec<usize>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DragSourceState {
    pub fence_id: i32,
    pub paths: Vec<String>,
    pub finished_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct MarqueeState {
    pub origin: Pos2,
    pub current: Pos2,
    pub additive: bool,
    pub base_selection: HashSet<usize>,
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
    pub icon_cache: IconCache,
    pub textures: HashMap<String, Option<TextureHandle>>,
    pub attached_hwnds: HashMap<i32, isize>,
    pub builder_rects: HashMap<i32, (Rect, bool)>,
    pub rename_prompt: Option<RenamePrompt>,
    pub item_rename_prompt: Option<ItemRenamePrompt>,
    pub inspector: Option<InspectorState>,
    pub inspector_spawned: bool,
    pub selected_items: HashMap<i32, HashSet<usize>>,
    pub selection_anchor: HashMap<i32, usize>,
    pub marquee_drag: HashMap<i32, MarqueeState>,
    pub item_reorder: HashMap<i32, ReorderDragState>,
    pub title_drag: Option<TitleDragState>,
    pub pending_toggle_collapse: HashSet<i32>,
    pub pending_collapse_until: HashMap<i32, Instant>,
    pub undo_stack: Vec<UndoAction>,
    pub cut_paths: HashSet<String>,
    pub scroll_to_item: HashMap<i32, usize>,
    pub type_ahead: HashMap<i32, (String, Instant)>,
    pub toast_msg: HashMap<i32, (String, Instant)>,
    pub last_title_click: HashMap<i32, Instant>,
    pub dragging_from_fence: Option<DragSourceState>,
    pub popup_owners: HashMap<i32, PopupOwnerState>,
    pub pending_commands: Vec<FenceCommand>,
    pub dirty_layout: bool,
    pub pending_save_at: Option<Instant>,
}

impl SharedFenceState {
    pub fn new(config: NativeLayoutConfig, topology: DisplayTopology, icon_cache: IconCache) -> Self {
        let builder_rects = config
            .fences
            .iter()
            .map(|f| (f.id, (f.rect, f.collapsed)))
            .collect();
        Self {
            config,
            topology,
            icon_cache,
            textures: HashMap::new(),
            attached_hwnds: HashMap::new(),
            builder_rects,
            rename_prompt: None,
            item_rename_prompt: None,
            inspector: None,
            inspector_spawned: false,
            selected_items: HashMap::new(),
            selection_anchor: HashMap::new(),
            marquee_drag: HashMap::new(),
            item_reorder: HashMap::new(),
            title_drag: None,
            pending_toggle_collapse: HashSet::new(),
            pending_collapse_until: HashMap::new(),
            undo_stack: Vec::new(),
            cut_paths: HashSet::new(),
            scroll_to_item: HashMap::new(),
            type_ahead: HashMap::new(),
            toast_msg: HashMap::new(),
            last_title_click: HashMap::new(),
            dragging_from_fence: None,
            popup_owners: HashMap::new(),
            pending_commands: Vec::new(),
            dirty_layout: false,
            pending_save_at: None,
        }
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
            // Avoid waking ROOT every pixel drag tick; schedule deferred save wake.
            ctx.request_repaint_after_for(delay, ViewportId::ROOT);
        } else {
            self.pending_save_at = Some(Instant::now() + Duration::from_millis(300));
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    pub fn get_or_load_texture(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        let key = path.to_ascii_lowercase();
        if let Some(cached) = self.textures.get(&key) {
            return cached.clone();
        }
        let Some(decoded) = self.icon_cache.get_or_load(path).cloned() else {
            self.textures.insert(key, None);
            return None;
        };
        let mut rgba = decoded.pbgra;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let img = ColorImage::from_rgba_premultiplied(
            [decoded.width as usize, decoded.height as usize],
            &rgba,
        );
        let handle = ctx.load_texture(key.clone(), img, TextureOptions::LINEAR);
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
                }
            } else if let Some(ref mut ins) = self.inspector {
                ins.open = false;
                ins.draft_name = None;
            }
        }
    }
}

pub fn collect_managed_desktop_paths(config: &NativeLayoutConfig) -> Vec<String> {
    config
        .fences
        .iter()
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
    // Popup menus & tooltips use window_fill; viewports still clear to [0,0,0,0] via clear_color.
    visuals.window_fill = Color32::from_rgba_unmultiplied(30, 34, 44, 246);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 55));
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    ctx.set_visuals(visuals);

    // Disable ScrollArea edge fade gradients (which render harsh grey bars over translucent panels)
    // and use sleek floating scrollbars.
    ctx.global_style_mut(|style| {
        style.spacing.scroll = egui::style::ScrollStyle::floating();
        style.spacing.scroll.fade.strength = 0.0;
        style.spacing.scroll.fade.size = 0.0;
        style.spacing.scroll.bar_width = 5.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
    });
}

fn ensure_fence_attached(fence_id: i32, shared: &Arc<Mutex<SharedFenceState>>) {
    let already = shared.lock().unwrap().attached_hwnds.contains_key(&fence_id);
    if already {
        return;
    }
    let Some(raw_hwnd) = find_window_by_exact_title(&format!("DeskLite#{fence_id}")) else {
        return;
    };
    attach_desktop_owner_and_rounded_corners(raw_hwnd);
    shared.lock().unwrap().attached_hwnds.insert(fence_id, raw_hwnd);
}

pub fn render_fence_viewport(
    ui: &mut Ui,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();
    ensure_fence_attached(fence_id, shared);

    // Close this fence's open popup menu cleanly on focus loss (e.g. clicking Desktop or another window),
    // tracking popup_id per fence so multiple fences never clobber one another.
    let focused_now = ctx.input(|i| i.focused);
    let lost_focus_ev = ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::WindowFocused(false)))
    });
    let tracked_popup = {
        let guard = shared.lock().unwrap();
        guard.popup_owners.get(&fence_id).copied()
    };
    if let Some(mut owner) = tracked_popup {
        if !egui::Popup::is_id_open(&ctx, owner.popup_id) {
            shared.lock().unwrap().popup_owners.remove(&fence_id);
        } else {
            let fg_hwnd = foreground_window_handle();
            let my_hwnd = {
                let guard = shared.lock().unwrap();
                guard.attached_hwnds.get(&fence_id).copied().unwrap_or(0)
            };
            let is_fg = my_hwnd != 0 && fg_hwnd == my_hwnd;
            if focused_now || is_fg {
                owner.saw_focused = true;
            }
            if lost_focus_ev
                || (owner.saw_focused && !focused_now)
                || (owner.saw_focused && my_hwnd != 0 && fg_hwnd != 0 && fg_hwnd != my_hwnd)
            {
                egui::Popup::close_id(&ctx, owner.popup_id);
                shared.lock().unwrap().popup_owners.remove(&fence_id);
                ctx.request_repaint();
            } else {
                shared.lock().unwrap().popup_owners.insert(fence_id, owner);
            }
        }
    }

    // 1. Read fence snapshot & preferences once per frame
    let (
        spec_opt,
        cell_w,
        cell_h,
        list_row_h,
        list_icon_px,
        snap_px,
        monitors,
        sibling_rects,
        is_renaming_this,
        renaming_item_idx,
        selected_set,
        cut_paths_snap,
        scroll_to_idx,
        toast_text,
    ) = {
        let mut guard = shared.lock().unwrap();
        // Expire stale inter-fence drag source after 2 seconds
        if let Some(ref ds) = guard.dragging_from_fence
            && let Some(done_at) = ds.finished_at
            && done_at.elapsed() > Duration::from_secs(2)
        {
            guard.dragging_from_fence = None;
        }
        let toast_text = if let Some((msg, at)) = guard.toast_msg.get(&fence_id).cloned() {
            if at.elapsed() < Duration::from_millis(1200) {
                ctx.request_repaint_after(Duration::from_millis(80));
                Some(msg)
            } else {
                guard.toast_msg.remove(&fence_id);
                None
            }
        } else {
            None
        };
        let spec = guard.config.fences.iter().find(|f| f.id == fence_id).cloned();
        let (target_cw, target_ch, target_rh, target_li) = spec
            .as_ref()
            .map(|s| resolve_fence_layout(s, &guard.config.preferences))
            .unwrap_or((
                guard.config.preferences.icon_cell_w.clamp(64, 160) as f32,
                guard.config.preferences.icon_cell_h.clamp(72, 180) as f32,
                guard.config.preferences.list_row_h.clamp(24, 56) as f32,
                guard.config.preferences.list_icon_px.clamp(14, 36) as f32,
            ));
        let siblings: Vec<Rect> = guard
            .config
            .fences
            .iter()
            .filter(|f| f.id != fence_id)
            .map(|f| f.rect)
            .collect();
        let renaming = guard
            .rename_prompt
            .as_ref()
            .is_some_and(|r| r.fence_id == fence_id);
        let renaming_item_idx = guard
            .item_rename_prompt
            .as_ref()
            .filter(|r| r.fence_id == fence_id)
            .map(|r| r.item_idx);
        let sel = guard
            .selected_items
            .get(&fence_id)
            .cloned()
            .unwrap_or_default();
        let cut_paths_snap = guard.cut_paths.clone();
        let scroll_to_idx = guard.scroll_to_item.remove(&fence_id);
        (
            spec,
            target_cw,
            target_ch,
            target_rh,
            target_li,
            guard.config.preferences.snap_threshold_px,
            guard.topology.monitors().to_vec(),
            siblings,
            renaming,
            renaming_item_idx,
            sel,
            cut_paths_snap,
            scroll_to_idx,
            toast_text,
        )
    };

    let Some(mut spec) = spec_opt else {
        ctx.send_viewport_cmd(ViewportCommand::Close);
        return out;
    };

    let (target_cw, target_ch, target_rh, target_li) = (cell_w, cell_h, list_row_h, list_icon_px);
    let cell_w = ctx.animate_value_with_time(
        ui.make_persistent_id(("anim_cw", fence_id)),
        target_cw,
        0.14,
    );
    let cell_h = ctx.animate_value_with_time(
        ui.make_persistent_id(("anim_ch", fence_id)),
        target_ch,
        0.14,
    );
    let list_row_h = ctx.animate_value_with_time(
        ui.make_persistent_id(("anim_rh", fence_id)),
        target_rh,
        0.14,
    );
    let list_icon_px = ctx.animate_value_with_time(
        ui.make_persistent_id(("anim_li", fence_id)),
        target_li,
        0.14,
    );

    let want_toggle = shared.lock().unwrap().pending_toggle_collapse.remove(&fence_id);
    if want_toggle {
        toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
        spec.collapsed = !spec.collapsed;
    }

    // 2. Smooth custom titlebar drag (avoids Windows Aero Snap, edge overshoot rebound, and WS_CAPTION flashes)
    let my_hwnd = {
        let guard = shared.lock().unwrap();
        guard.attached_hwnds.get(&fence_id).copied().unwrap_or(0)
    };
    if my_hwnd != 0 {
        set_hwnd_locked(my_hwnd, spec.locked);
        let text_editing_now =
            is_renaming_this || renaming_item_idx.is_some() || ctx.egui_wants_keyboard_input();
        if text_editing_now {
            set_active_text_edit_hwnd(my_hwnd);
        } else if focused_now || foreground_window_handle() == my_hwnd {
            set_active_text_edit_hwnd(0);
        }
    }
    if spec.locked {
        let mut guard = shared.lock().unwrap();
        if guard.title_drag.is_some_and(|td| td.fence_id == fence_id) {
            guard.title_drag = None;
        }
    }
    let active_title_drag = if spec.locked {
        None
    } else {
        let guard = shared.lock().unwrap();
        guard.title_drag.filter(|td| td.fence_id == fence_id)
    };
    let mut title_drag_ended_this_frame = false;
    if let Some(td) = active_title_drag {
        if desklite_win32::is_primary_mouse_down() {
            if let Some((cx, cy)) = desklite_win32::cursor_screen_pos() {
                let raw_l = cx - td.grab_dx;
                let raw_t = cy - td.grab_dy;
                let (nx, ny) = clamp_drag_pos_to_monitors(
                    raw_l,
                    raw_t,
                    td.width,
                    td.height,
                    (cx, cy),
                    &monitors,
                );
                desklite_win32::move_window_no_z(my_hwnd, nx, ny);
                let mut guard = shared.lock().unwrap();
                let live_h = if spec.collapsed {
                    spec.rect.height().max(MIN_FENCE_HEIGHT)
                } else {
                    td.height.max(MIN_FENCE_HEIGHT)
                };
                let live_rect = Rect::new(nx, ny, nx + td.width.max(MIN_FENCE_WIDTH), ny + live_h);
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.rect = live_rect;
                }
                if guard
                    .inspector
                    .as_ref()
                    .is_some_and(|ins| ins.target_fence_id == fence_id)
                {
                    ctx.request_repaint_of(ViewportId::from_hash_of("desklite_inspector"));
                }
            }
            ctx.request_repaint();
        } else {
            title_drag_ended_this_frame = true;
            let mut guard = shared.lock().unwrap();
            guard.title_drag = None;
            if let Some((l, t, r, b)) = desklite_win32::window_outer_rect(my_hwnd) {
                let move_box = Rect::new(l, t, r, b);
                let alt_held = ctx.input(|i| i.modifiers.alt);
                let snapped =
                    snap_move_rect(move_box, &sibling_rects, &monitors, snap_px, alt_held);
                if (snapped.left - move_box.left).abs() > 0
                    || (snapped.top - move_box.top).abs() > 0
                {
                    desklite_win32::move_window_no_z(my_hwnd, snapped.left, snapped.top);
                }
                let final_rect = if spec.collapsed {
                    Rect::new(
                        snapped.left,
                        snapped.top,
                        snapped.right,
                        snapped.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    snapped
                };
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.rect = final_rect;
                }
                guard.config.update_active_snapshot_rect(fence_id, final_rect);
                guard.mark_debounced_save(&ctx);
                if guard
                    .inspector
                    .as_ref()
                    .is_some_and(|ins| ins.target_fence_id == fence_id)
                {
                    ctx.request_repaint_of(ViewportId::from_hash_of("desklite_inspector"));
                }
            }
        }
    }

    let is_title_dragging = shared
        .lock()
        .unwrap()
        .title_drag
        .is_some_and(|td| td.fence_id == fence_id);

    // 2b. Sync user move/resize back into physical pixels with DPI scaling & debounced save
    let ppp = ctx.pixels_per_point().max(0.5);
    let live_phys_rect = desklite_win32::window_outer_rect(my_hwnd)
        .map(|(l, t, r, b)| Rect::new(l, t, r, b))
        .or_else(|| {
            ctx.input(|i| i.viewport().outer_rect).map(|or| {
                Rect::new(
                    (or.min.x * ppp).round() as i32,
                    (or.min.y * ppp).round() as i32,
                    (or.max.x * ppp).round() as i32,
                    (or.max.y * ppp).round() as i32,
                )
            })
        });
    let any_pointer_down = ctx.input(|i| i.pointer.any_down());
    let alt_held = ctx.input(|i| i.modifiers.alt);
    let collapse_settling = {
        let mut guard = shared.lock().unwrap();
        let active = guard
            .pending_collapse_until
            .get(&fence_id)
            .is_some_and(|until| *until > Instant::now());
        if !active {
            guard.pending_collapse_until.remove(&fence_id);
        }
        active
    };
    if desklite_win32::is_in_size_move()
        && live_sizemove_rect_for_hwnd(my_hwnd).is_some()
        && shared
            .lock()
            .unwrap()
            .inspector
            .as_ref()
            .is_some_and(|ins| ins.open && ins.target_fence_id == fence_id)
    {
        ctx.request_repaint_of(ViewportId::from_hash_of("desklite_inspector"));
    }
    if let Some(cur_phys) = live_phys_rect
        && !any_pointer_down
        && !desklite_win32::is_in_size_move()
        && !is_title_dragging
        && !title_drag_ended_this_frame
    {
        let collapsed_phys_h = (COLLAPSED_HEIGHT * ppp).round() as i32;
        let expected_h = if spec.collapsed {
            collapsed_phys_h
        } else {
            spec.rect.height()
        };
        if collapse_settling && (cur_phys.height() - expected_h).abs() <= 2 {
            shared.lock().unwrap().pending_collapse_until.remove(&fence_id);
        } else if collapse_settling && (cur_phys.height() - expected_h).abs() > 2 {
            // Wait for InnerSize collapse/expand resize to settle so pre-toggle height never clobbers spec.rect
        } else {
        let resized = (cur_phys.width() - spec.rect.width()).abs() > 2
            || (!spec.collapsed && (cur_phys.height() - expected_h).abs() > 2);
        let moved = (cur_phys.left - spec.rect.left).abs() > 2
            || (cur_phys.top - spec.rect.top).abs() > 2;

        if (resized || moved) && cur_phys.width() >= MIN_FENCE_WIDTH {
            let final_rect = if resized {
                let candidate = if spec.collapsed {
                    Rect::new(
                        cur_phys.left,
                        cur_phys.top,
                        cur_phys.right,
                        cur_phys.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    cur_phys
                };
                clamp_rect_to_monitors(candidate, &monitors)
                    .map(|(r, _)| r)
                    .unwrap_or(candidate)
            } else {
                // Move-only: use collapsed bar height when collapsed so bar can reach bottom edge
                let move_box = if spec.collapsed {
                    Rect::new(
                        cur_phys.left,
                        cur_phys.top,
                        cur_phys.right,
                        cur_phys.top + collapsed_phys_h,
                    )
                } else {
                    cur_phys
                };
                let snapped = snap_move_rect(move_box, &sibling_rects, &monitors, snap_px, alt_held);
                if (snapped.left - cur_phys.left).abs() > 1 || (snapped.top - cur_phys.top).abs() > 1
                {
                    if my_hwnd != 0 {
                        desklite_win32::move_window_no_z(my_hwnd, snapped.left, snapped.top);
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
                            snapped.left as f32 / ppp,
                            snapped.top as f32 / ppp,
                        )));
                    }
                }
                if spec.collapsed {
                    Rect::new(
                        snapped.left,
                        snapped.top,
                        snapped.right,
                        snapped.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    snapped
                }
            };

            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.rect = final_rect;
            }
            guard.config.update_active_snapshot_rect(fence_id, final_rect);
            guard.mark_debounced_save(&ctx);
            if guard
                .inspector
                .as_ref()
                .is_some_and(|ins| ins.target_fence_id == fence_id)
            {
                ctx.request_repaint_of(ViewportId::from_hash_of("desklite_inspector"));
            }
        }
        }
    }

    // 3. Resize borders are handled natively via WM_NCHITTEST in window_attach.rs so
    // Windows drives SC_SIZE directly with exact screen coords (avoiding winit's BeginResize).
    let win_rect = ui.max_rect();
    let resize_dir = if spec.locked {
        None
    } else {
        ctx.input(|i| i.pointer.hover_pos())
            .and_then(|pos| detect_resize_edge(win_rect, pos, spec.collapsed))
    };
    if let Some(dir) = resize_dir {
        ctx.set_cursor_icon(resize_cursor(dir));
    }

    // 4. Handle files dragged over / dropped onto this fence
    let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let dropped_paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    if !dropped_paths.is_empty() && spec.portal_dir.is_none() {
        let managed = {
            let mut guard = shared.lock().unwrap();
            let matching_transfer = guard.dragging_from_fence.as_ref().is_some_and(|ds| {
                ds.paths
                    .iter()
                    .any(|sp| dropped_paths.iter().any(|dp| dp.eq_ignore_ascii_case(sp)))
            });
            let source_transfer = if matching_transfer {
                guard.dragging_from_fence.take()
            } else {
                None
            };
            if let Some(src_info) = source_transfer
                && src_info.fence_id != fence_id
                && let Some(src) = guard.config.fence_mut(src_info.fence_id)
                && src.portal_dir.is_none()
            {
                src.items.retain(|it| {
                    !src_info
                        .paths
                        .iter()
                        .any(|sp| it.path.eq_ignore_ascii_case(sp))
                });
                for (idx, it) in src.items.iter_mut().enumerate() {
                    it.order = idx as i32;
                }
                guard.selected_items.remove(&src_info.fence_id);
                guard.selection_anchor.remove(&src_info.fence_id);
                ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", src_info.fence_id)));
            }
            if let Some(f) = guard.config.fence_mut(fence_id) {
                let mut added_paths = Vec::new();
                for raw in dropped_paths {
                    if !f.items.iter().any(|it| it.path.eq_ignore_ascii_case(&raw)) {
                        let order = f.items.len() as i32;
                        let item = resolve_dropped_item(Path::new(&raw), order);
                        added_paths.push(item.path.clone());
                        f.items.push(item);
                    }
                }
                if !added_paths.is_empty() {
                    guard.push_undo(UndoAction::AddItems {
                        fence_id,
                        added_paths,
                    });
                }
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
            collect_managed_desktop_paths(&guard.config)
        };
        hide_desktop_icons_for_paths(&managed);
        ctx.request_repaint();
    }

    let is_portal = spec.portal_dir.is_some();
    let border_stroke = if hovering_files && !is_portal {
        Stroke::new(2.0, Color32::from_rgb(96, 165, 250))
    } else if hovering_files && is_portal {
        Stroke::new(2.0, Color32::from_rgb(245, 158, 11))
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20))
    };

    let bg_alpha = ctx
        .animate_value_with_time(
            ui.make_persistent_id(("anim_alpha", fence_id)),
            spec.backdrop_alpha.clamp(40, 245) as f32,
            0.12,
        )
        .round()
        .clamp(40.0, 245.0) as u8;
    let title_alpha = (bg_alpha as f32 + 45.0).min(235.0) as u8;
    let panel_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(22, 24, 30, bg_alpha))
        .stroke(border_stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(0));

    egui::CentralPanel::default().frame(panel_frame).show(ui, |ui| {
        // ---- Title bar ----
        let title_bar_frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(32, 36, 46, title_alpha))
            .corner_radius(CornerRadius {
                nw: 8,
                ne: 8,
                sw: if spec.collapsed { 8 } else { 0 },
                se: if spec.collapsed { 8 } else { 0 },
            })
            .inner_margin(Margin {
                left: 10,
                right: 9,
                top: 6,
                bottom: 6,
            });

        title_bar_frame.show(ui, |ui| {
            ui.set_height(COLLAPSED_HEIGHT - 12.0);
            // Lay out right-side buttons FIRST so they never overflow at narrow widths
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let menu_resp = draw_header_icon_btn(ui, "格子菜单", |painter, r, fg| {
                    let cx = r.center().x;
                    let cy = r.center().y;
                    for dy in [-4.5_f32, 0.0, 4.5] {
                        painter.circle_filled(Pos2::new(cx, cy + dy), 1.4, fg);
                    }
                });
                if menu_resp.clicked() {
                    open_fence_menu(
                        fence_id,
                        my_hwnd,
                        Some((menu_resp.rect.left(), menu_resp.rect.bottom(), ppp)),
                        false,
                        &monitors,
                        shared,
                        &ctx,
                    );
                }

                let is_list = matches!(spec.kind, FenceKind::FileList);
                if draw_header_icon_btn(ui, "切换网格 / 列表视图", |painter, r, fg| {
                    let c = r.center();
                    if is_list {
                        for dy in [-3.5_f32, 0.0, 3.5] {
                            painter.line_segment(
                                [Pos2::new(c.x - 5.0, c.y + dy), Pos2::new(c.x + 5.0, c.y + dy)],
                                Stroke::new(1.5, fg),
                            );
                        }
                    } else {
                        for dx in [-4.0_f32, 1.0] {
                            for dy in [-4.0_f32, 1.0] {
                                painter.rect_filled(
                                    egui::Rect::from_min_size(
                                        Pos2::new(c.x + dx, c.y + dy),
                                        Vec2::splat(3.5),
                                    ),
                                    CornerRadius::same(1),
                                    fg,
                                );
                            }
                        }
                    }
                })
                .clicked()
                {
                    let mut guard = shared.lock().unwrap();
                    if let Some(f) = guard.config.fence_mut(fence_id) {
                        f.kind = match f.kind {
                            FenceKind::FileList => FenceKind::IconGrid,
                            _ => FenceKind::FileList,
                        };
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                }

                let collapsed = spec.collapsed;
                if draw_header_icon_btn(
                    ui,
                    if collapsed { "展开格子" } else { "折叠格子" },
                    |painter, r, fg| {
                        let c = r.center();
                        let stroke = Stroke::new(1.6, fg);
                        if collapsed {
                            painter.line_segment(
                                [Pos2::new(c.x - 2.0, c.y - 4.0), Pos2::new(c.x + 2.5, c.y)],
                                stroke,
                            );
                            painter.line_segment(
                                [Pos2::new(c.x + 2.5, c.y), Pos2::new(c.x - 2.0, c.y + 4.0)],
                                stroke,
                            );
                        } else {
                            painter.line_segment(
                                [Pos2::new(c.x - 4.0, c.y - 2.0), Pos2::new(c.x, c.y + 2.5)],
                                stroke,
                            );
                            painter.line_segment(
                                [Pos2::new(c.x, c.y + 2.5), Pos2::new(c.x + 4.0, c.y - 2.0)],
                                stroke,
                            );
                        }
                    },
                )
                .clicked()
                {
                    toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
                }

                if spec.locked
                    && draw_header_icon_btn(ui, "已锁定位置与大小（点击解锁）", |painter, r, _| {
                        let c = r.center();
                        let lock_col = Color32::from_rgb(251, 191, 36);
                        let stroke = Stroke::new(1.4, lock_col);
                        painter.line(
                            vec![
                                Pos2::new(c.x - 3.0, c.y - 0.5),
                                Pos2::new(c.x - 3.0, c.y - 3.5),
                                Pos2::new(c.x, c.y - 5.0),
                                Pos2::new(c.x + 3.0, c.y - 3.5),
                                Pos2::new(c.x + 3.0, c.y - 0.5),
                            ],
                            stroke,
                        );
                        painter.rect_stroke(
                            egui::Rect::from_center_size(
                                Pos2::new(c.x, c.y + 2.5),
                                Vec2::new(8.5, 6.5),
                            ),
                            CornerRadius::same(1),
                            stroke,
                            egui::StrokeKind::Inside,
                        );
                    })
                    .clicked()
                {
                    let mut guard = shared.lock().unwrap();
                    if let Some(f) = guard.config.fence_mut(fence_id) {
                        f.locked = false;
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                    set_hwnd_locked(my_hwnd, false);
                }

                if let Some(ref msg) = toast_text {
                    ui.label(
                        egui::RichText::new(msg)
                            .size(11.0)
                            .color(Color32::from_rgb(138, 198, 255)),
                    );
                }

                // Remaining left width belongs to title / rename edit
                let rem_w = ui.available_width().max(24.0);
                if is_renaming_this {
                    let mut commit = None;
                    let mut cancel = false;
                    {
                        let mut guard = shared.lock().unwrap();
                        if let Some(ref mut rp) = guard.rename_prompt {
                            if rp.needs_focus {
                                focus_window_now(my_hwnd);
                            }
                            let resp = ui.add_sized(
                                [rem_w, 22.0],
                                egui::TextEdit::singleline(&mut rp.draft).desired_width(rem_w),
                            );
                            if rp.needs_focus {
                                resp.request_focus();
                                rp.needs_focus = false;
                            }
                            if resp.has_focus() {
                                handle_text_edit_clipboard(&ctx, resp.id, &mut rp.draft);
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let trimmed = rp.draft.trim().to_string();
                                if !trimmed.is_empty() {
                                    commit = Some(trimmed);
                                } else {
                                    cancel = true;
                                }
                            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                cancel = true;
                            } else if resp.lost_focus() {
                                let trimmed = rp.draft.trim().to_string();
                                if !trimmed.is_empty() {
                                    commit = Some(trimmed);
                                } else {
                                    cancel = true;
                                }
                            }
                        }
                    }
                    if let Some(new_name) = commit {
                        let mut guard = shared.lock().unwrap();
                        let old_name = guard
                            .config
                            .fence_mut(fence_id)
                            .and_then(|f| {
                                if f.name != new_name {
                                    let prev = std::mem::replace(&mut f.name, new_name);
                                    Some(prev)
                                } else {
                                    None
                                }
                            });
                        if let Some(old_name) = old_name {
                            guard.push_undo(UndoAction::RenameFence { fence_id, old_name });
                            guard.dirty_layout = true;
                            out.needs_immediate_save = true;
                        }
                        guard.rename_prompt = None;
                    } else if cancel {
                        shared.lock().unwrap().rename_prompt = None;
                    }
                } else {
                    let display_title = format_fence_title(&spec);

                    let (title_rect, _) =
                        ui.allocate_exact_size(Vec2::new(rem_w, 26.0), Sense::hover());
                    let title_resp = ui.interact(
                        title_rect.expand2(Vec2::new(0.0, 1.0)),
                        ui.make_persistent_id(("title_bar", fence_id)),
                        Sense::click_and_drag(),
                    );
                    let clipped_title = ui.painter().with_clip_rect(title_rect);
                    let title_font = egui::FontId::proportional(13.5);
                    let fitted_title = fit_single_line(
                        ui.painter(),
                        &display_title,
                        title_font.clone(),
                        (rem_w - 6.0).max(16.0),
                    );
                    clipped_title.text(
                        Pos2::new(title_rect.min.x + 2.0, title_rect.center().y - 0.5),
                        egui::Align2::LEFT_CENTER,
                        fitted_title,
                        title_font,
                        Color32::from_rgb(242, 245, 252),
                    );

                    if resize_dir.is_none() {
                        let pointer_dbl = ctx.input(|i| {
                            i.pointer.button_double_clicked(PointerButton::Primary)
                                && i.pointer
                                    .interact_pos()
                                    .is_some_and(|p| title_rect.expand(3.0).contains(p))
                        });
                        let mut quick_second_click = false;
                        if title_resp.clicked() {
                            let now = Instant::now();
                            let mut guard = shared.lock().unwrap();
                            if let Some(prev) = guard.last_title_click.get(&fence_id)
                                && now.duration_since(*prev) <= Duration::from_millis(550)
                            {
                                quick_second_click = true;
                                guard.last_title_click.remove(&fence_id);
                            } else {
                                guard.last_title_click.insert(fence_id, now);
                            }
                        }

                        if title_resp.double_clicked() || pointer_dbl || quick_second_click {
                            toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
                        } else if !spec.locked && title_resp.dragged_by(PointerButton::Primary) {
                            let press_origin = ctx.input(|i| {
                                i.pointer
                                    .press_origin()
                                    .zip(i.pointer.interact_pos())
                                    .filter(|(a, b)| (*b - *a).length() > 4.0)
                                    .map(|(a, _)| a)
                            });
                            if let Some(origin) = press_origin
                                && !is_title_dragging
                                && desklite_win32::is_primary_mouse_down()
                                && let Some((cx, cy)) = desklite_win32::cursor_screen_pos()
                                && let Some((l, t, r, b)) =
                                    desklite_win32::window_outer_rect(my_hwnd)
                            {
                                let width = (r - l).max(MIN_FENCE_WIDTH);
                                let height = (b - t).max(1);
                                let grab_dx = ((origin.x * ppp).round() as i32).clamp(0, width);
                                let grab_dy = ((origin.y * ppp).round() as i32).clamp(0, height);
                                let (nx, ny) = clamp_drag_pos_to_monitors(
                                    cx - grab_dx,
                                    cy - grab_dy,
                                    width,
                                    height,
                                    (cx, cy),
                                    &monitors,
                                );
                                desklite_win32::move_window_no_z(my_hwnd, nx, ny);
                                shared.lock().unwrap().title_drag = Some(TitleDragState {
                                    fence_id,
                                    grab_dx,
                                    grab_dy,
                                    width,
                                    height,
                                });
                                ctx.request_repaint();
                            }
                        }
                    }
                    if title_resp.clicked() && !focused_now {
                        focus_window_now(my_hwnd);
                    }
                    if title_resp.secondary_clicked() {
                        if !focused_now {
                            focus_window_now(my_hwnd);
                        }
                        open_fence_menu(
                            fence_id,
                            my_hwnd,
                            None,
                            false,
                            &monitors,
                            shared,
                            &ctx,
                        );
                    }
                }
            });
        });

        if spec.collapsed {
            if my_hwnd != 0 {
                let _ = take_win32_hotkeys(my_hwnd);
            }
            return;
        }

        // Subtle 1px divider below title bar for crisp glass separation without scroll fade shadows.
        let top_div_y = ui.cursor().top();
        ui.painter().hline(
            ui.max_rect().x_range(),
            top_div_y,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
        );

        // ---- Body area (zero vertical body_frame margin so scroll viewport aligns flush with edges) ----
        let body_frame = Frame::new().inner_margin(Margin::symmetric(8, 0));
        body_frame.show(ui, |ui| {
            if hovering_files {
                ui.add_space(8.0);
                if is_portal {
                    ui.colored_label(
                        Color32::from_rgb(251, 191, 36),
                        "文件夹映射格子由磁盘目录同步，不支持拖入",
                    );
                } else {
                    ui.colored_label(
                        Color32::from_rgb(130, 195, 255),
                        "松开鼠标收纳到此格子",
                    );
                }
            } else if spec.items.is_empty() {
                let mut blank_action: Option<ItemUiAction> = None;
                let empty_resp = ui.interact(
                    ui.available_rect_before_wrap(),
                    ui.make_persistent_id(("empty_body", fence_id)),
                    Sense::click(),
                );
                if (empty_resp.clicked() || empty_resp.secondary_clicked()) && !focused_now {
                    focus_window_now(my_hwnd);
                }
                if empty_resp.clicked() {
                    let mut guard = shared.lock().unwrap();
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                }
                if empty_resp.secondary_clicked() {
                    open_fence_menu(
                        fence_id,
                        my_hwnd,
                        None,
                        true,
                        &monitors,
                        shared,
                        &ctx,
                    );
                }
                let win32_hotkeys = if my_hwnd != 0 && !is_renaming_this {
                    take_win32_hotkeys(my_hwnd)
                } else {
                    if my_hwnd != 0 {
                        let _ = take_win32_hotkeys(my_hwnd);
                    }
                    Vec::new()
                };
                for hk in &win32_hotkeys {
                    dispatch_win32_hotkey(
                        *hk,
                        fence_id,
                        &spec,
                        &selected_set,
                        shared,
                        &mut blank_action,
                    );
                }
                let has_input_focus =
                    focused_now || (my_hwnd != 0 && foreground_window_handle() == my_hwnd);
                if blank_action.is_none()
                    && win32_hotkeys.is_empty()
                    && has_input_focus
                    && !is_renaming_this
                    && !egui::Popup::is_any_open(&ctx)
                {
                    let (ctrl_v, ctrl_z) = ctx.input(|i| {
                        let cmd = (i.modifiers.command || is_ctrl_key_down()) && !i.modifiers.alt;
                        (
                            cmd && !i.modifiers.shift && i.key_pressed(Key::V),
                            cmd && !i.modifiers.shift && i.key_pressed(Key::Z),
                        )
                    });
                    if ctrl_v {
                        blank_action = Some(ItemUiAction::PasteFromClipboard);
                    } else if ctrl_z {
                        blank_action = Some(ItemUiAction::UndoLast);
                    }
                }
                if let Some(act) = blank_action {
                    execute_item_action(
                        act,
                        fence_id,
                        my_hwnd,
                        &spec,
                        &selected_set,
                        shared,
                        &ctx,
                        &mut out,
                    );
                }
                ui.painter().text(
                    empty_resp.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "可从桌面或资源管理器拖入文件，或 Ctrl+V 粘贴\n也可以拖动条目放到桌面或其它格子",
                    egui::FontId::proportional(12.0),
                    Color32::from_rgba_unmultiplied(200, 210, 228, 175),
                );
                return;
            }

            let mut item_action: Option<ItemUiAction> = None;
            let mut opened_item_popup: Option<egui::Id> = None;
            let mut item_rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(spec.items.len());
            let viewport_clip_rect = ui.available_rect_before_wrap();
            // Register blank-body background BEFORE ScrollArea so items & scrollbar sit above it,
            // while every blank pixel remains clickable/right-clickable regardless of scroll offset.
            let blank_resp = ui.interact(
                viewport_clip_rect,
                ui.make_persistent_id(("blank_body", fence_id)),
                Sense::click_and_drag(),
            );
            if (blank_resp.clicked() || blank_resp.secondary_clicked()) && resize_dir.is_none() {
                if !focused_now {
                    focus_window_now(my_hwnd);
                }
                let mods = ctx.input(|i| i.modifiers);
                if !mods.ctrl && !mods.shift {
                    let mut guard = shared.lock().unwrap();
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                }
                if blank_resp.secondary_clicked() {
                    open_fence_menu(
                        fence_id,
                        my_hwnd,
                        None,
                        true,
                        &monitors,
                        shared,
                        &ctx,
                    );
                }
            }
            let body_blank_resp: Option<egui::Response> = Some(blank_resp);
            let mut grid_cols: usize = 1;

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .scroll_source(egui::scroll_area::ScrollSource {
                    scroll_bar: true,
                    drag: egui::scroll_area::DragScroll::Never,
                    mouse_wheel: true,
                })
                .show(ui, |ui| {

                    ui.add_space(6.0);
                    let avail_w = ui.available_width().max(cell_w);
                    let sel_count = selected_set.len();
                    let mut rename_commit: Option<(usize, String, String)> = None;
                    let mut rename_cancel = false;

                    let item_textures = {
                        let mut guard = shared.lock().unwrap();
                        guard.resolve_item_textures(&ctx, &spec.items)
                    };

                    match spec.kind {
                        FenceKind::FileList => {
                            for (idx, item) in spec.items.iter().enumerate() {
                                let tex = item_textures.get(idx).and_then(|t| t.as_ref());
                                let selected = selected_set.contains(&idx);
                                let is_cut = cut_paths_snap.contains(&item.path.to_ascii_lowercase());
                                let is_renaming_item = renaming_item_idx == Some(idx);
                                let row_resp = if is_renaming_item {
                                    let (r, cm, cn) = render_list_item_renaming(
                                        ui,
                                        tex,
                                        list_row_h,
                                        list_icon_px,
                                        viewport_clip_rect,
                                        fence_id,
                                        shared,
                                        &ctx,
                                    );
                                    if let Some(v) = cm {
                                        rename_commit = Some(v);
                                    }
                                    if cn {
                                        rename_cancel = true;
                                    }
                                    r
                                } else {
                                    render_list_item(
                                        ui,
                                        item,
                                        tex,
                                        selected,
                                        is_cut,
                                        list_row_h,
                                        list_icon_px,
                                    )
                                };
                                if scroll_to_idx == Some(idx) {
                                    row_resp.scroll_to_me(None);
                                }
                                item_rects.push((idx, row_resp.rect));
                                if !is_renaming_item
                                    && let Some(pid) = handle_item_interactions(
                                        &row_resp,
                                        my_hwnd,
                                        idx,
                                        item,
                                        selected,
                                        sel_count,
                                        is_portal,
                                        &mut item_action,
                                    )
                                {
                                    opened_item_popup = Some(pid);
                                }
                            }
                        }
                        FenceKind::IconGrid | FenceKind::Unknown(_) => {
                            let gap_x = 6.0_f32;
                            let gap_y = 6.0_f32;
                            let tile_w = (cell_w - 4.0).max(60.0);
                            let cols = (((avail_w + gap_x) / (tile_w + gap_x)).floor() as usize).max(1);
                            grid_cols = cols;
                            let used_cols = cols.min(spec.items.len().max(1));
                            let grid_w = used_cols as f32 * tile_w
                                + (used_cols.saturating_sub(1) as f32) * gap_x;
                            let left_pad = ((avail_w - grid_w) * 0.5).floor().max(0.0);

                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                if left_pad > 0.0 {
                                    ui.add_space(left_pad);
                                }
                                egui::Grid::new(("fence_grid", fence_id))
                                    .num_columns(cols)
                                    .spacing([gap_x, gap_y])
                                    .show(ui, |ui| {
                                        for (idx, item) in spec.items.iter().enumerate() {
                                            let tex = item_textures.get(idx).and_then(|t| t.as_ref());
                                            let selected = selected_set.contains(&idx);
                                            let is_cut =
                                                cut_paths_snap.contains(&item.path.to_ascii_lowercase());
                                            let is_renaming_item = renaming_item_idx == Some(idx);
                                            let tile_resp = if is_renaming_item {
                                                let (r, cm, cn) = render_grid_item_renaming(
                                                    ui,
                                                    tex,
                                                    Vec2::new(tile_w, cell_h),
                                                    viewport_clip_rect,
                                                    fence_id,
                                                    shared,
                                                    &ctx,
                                                );
                                                if let Some(v) = cm {
                                                    rename_commit = Some(v);
                                                }
                                                if cn {
                                                    rename_cancel = true;
                                                }
                                                r
                                            } else {
                                                render_grid_item(
                                                    ui,
                                                    item,
                                                    tex,
                                                    selected,
                                                    is_cut,
                                                    Vec2::new(tile_w, cell_h),
                                                )
                                            };
                                            if scroll_to_idx == Some(idx) {
                                                tile_resp.scroll_to_me(None);
                                            }
                                            item_rects.push((idx, tile_resp.rect));
                                            if !is_renaming_item
                                                && let Some(pid) = handle_item_interactions(
                                                    &tile_resp,
                                                    my_hwnd,
                                                    idx,
                                                    item,
                                                    selected,
                                                    sel_count,
                                                    is_portal,
                                                    &mut item_action,
                                                )
                                            {
                                                opened_item_popup = Some(pid);
                                            }
                                            if (idx + 1) % cols == 0 {
                                                ui.end_row();
                                            }
                                        }
                                    });
                            });
                        }
                    }

                    if let Some((idx, old_path, new_name)) = rename_commit {
                        commit_item_rename(fence_id, idx, &old_path, &new_name, shared, &ctx, &mut out);
                    } else if rename_cancel {
                        shared.lock().unwrap().item_rename_prompt = None;
                    }
                    ui.add_space(6.0);
                });

            // Rubber-band / marquee drag selection on blank space
            if let Some(ref b_resp) = body_blank_resp {
                let pointer_pos = ctx.input(|i| i.pointer.interact_pos());
                let primary_down = ctx.input(|i| i.pointer.primary_down());
                let reorder_busy = shared.lock().unwrap().item_reorder.contains_key(&fence_id);
                if !reorder_busy
                    && b_resp.drag_started_by(PointerButton::Primary)
                    && resize_dir.is_none()
                    && let Some(pos) = pointer_pos
                {
                    let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                    let additive = ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                    let mut guard = shared.lock().unwrap();
                    let base_selection = if additive {
                        guard
                            .selected_items
                            .get(&fence_id)
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        HashSet::new()
                    };
                    guard.marquee_drag.insert(
                        fence_id,
                        MarqueeState {
                            origin,
                            current: pos,
                            additive,
                            base_selection,
                        },
                    );
                }

                let active_marquee = {
                    let mut guard = shared.lock().unwrap();
                    if primary_down && !b_resp.drag_stopped() {
                        if let Some(mq) = guard.marquee_drag.get_mut(&fence_id) {
                            if let Some(pos) = pointer_pos {
                                mq.current = pos;
                            }
                            let sel_rect = egui::Rect::from_two_pos(mq.origin, mq.current);
                            let mut next_sel = if mq.additive {
                                mq.base_selection.clone()
                            } else {
                                HashSet::new()
                            };
                            for &(idx, r) in &item_rects {
                                if sel_rect.intersects(r) {
                                    next_sel.insert(idx);
                                }
                            }
                            let mq_clone = mq.clone();
                            if next_sel.is_empty() {
                                guard.selected_items.remove(&fence_id);
                            } else {
                                guard.selected_items.insert(fence_id, next_sel);
                            }
                            Some(mq_clone)
                        } else {
                            None
                        }
                    } else {
                        guard.marquee_drag.remove(&fence_id);
                        None
                    }
                };

                if let Some(mq) = active_marquee {
                    let draw_rect = egui::Rect::from_two_pos(mq.origin, mq.current)
                        .intersect(viewport_clip_rect);
                    if draw_rect.width() > 1.0 && draw_rect.height() > 1.0 {
                        let painter = ui.painter().with_clip_rect(viewport_clip_rect);
                        painter.rect_filled(
                            draw_rect,
                            CornerRadius::same(2),
                            Color32::from_rgba_unmultiplied(64, 152, 255, 56),
                        );
                        painter.rect_stroke(
                            draw_rect,
                            CornerRadius::same(2),
                            Stroke::new(1.0, Color32::from_rgba_unmultiplied(110, 185, 255, 220)),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
            }

            // Internal drag reorder while pointer stays inside fence; escalate to Windows DoDragDrop once
            // pointer exits fence boundaries.
            let active_reorder = {
                let guard = shared.lock().unwrap();
                guard.item_reorder.get(&fence_id).cloned()
            };
            if let Some(reorder) = active_reorder {
                let pointer_pos = ctx.input(|i| i.pointer.interact_pos());
                let primary_down = ctx.input(|i| i.pointer.primary_down());
                let inside_fence = pointer_pos.is_some_and(|p| win_rect.shrink(2.0).contains(p));
                if primary_down && !inside_fence {
                    shared.lock().unwrap().item_reorder.remove(&fence_id);
                    item_action = Some(ItemUiAction::StartDragOut(reorder.trigger_idx));
                } else if primary_down
                    && let Some(pos) = pointer_pos
                    && let Some(insert_idx) =
                        compute_reorder_target(&item_rects, pos, matches!(spec.kind, FenceKind::FileList))
                {
                    paint_reorder_indicator(
                        ui,
                        viewport_clip_rect,
                        &item_rects,
                        insert_idx,
                        matches!(spec.kind, FenceKind::FileList),
                    );
                } else if !primary_down {
                    shared.lock().unwrap().item_reorder.remove(&fence_id);
                    if let Some(pos) = pointer_pos
                        && let Some(insert_idx) = compute_reorder_target(
                            &item_rects,
                            pos,
                            matches!(spec.kind, FenceKind::FileList),
                        )
                    {
                        apply_reorder_items(
                            fence_id,
                            &reorder.indices,
                            insert_idx,
                            shared,
                            &mut out,
                        );
                    }
                }
            }

            if let Some(pid) = opened_item_popup {
                let mut guard = shared.lock().unwrap();
                guard.popup_owners.insert(
                    fence_id,
                    PopupOwnerState {
                        popup_id: pid,
                        saw_focused: focused_now,
                    },
                );
            }

            let marquee_active = shared.lock().unwrap().marquee_drag.contains_key(&fence_id);
            let reorder_active = shared.lock().unwrap().item_reorder.contains_key(&fence_id);
            let has_input_focus =
                focused_now || (my_hwnd != 0 && foreground_window_handle() == my_hwnd);
            if let Some(ItemUiAction::Select { .. } | ItemUiAction::ContextSelect(_)) = item_action
            {
                let sel_act = item_action.take().unwrap();
                execute_item_action(
                    sel_act,
                    fence_id,
                    my_hwnd,
                    &spec,
                    &selected_set,
                    shared,
                    &ctx,
                    &mut out,
                );
            }
            let current_selected = shared
                .lock()
                .unwrap()
                .selected_items
                .get(&fence_id)
                .cloned()
                .unwrap_or_default();
            let can_handle_hotkeys = item_action.is_none()
                && !is_renaming_this
                && renaming_item_idx.is_none()
                && !marquee_active
                && !reorder_active
                && !egui::Popup::is_any_open(&ctx);
            let win32_hotkeys = if my_hwnd != 0 {
                let list = take_win32_hotkeys(my_hwnd);
                if can_handle_hotkeys {
                    list
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let mut handled_ctrl_from_win32 = false;
            if can_handle_hotkeys {
                for hk in &win32_hotkeys {
                    dispatch_win32_hotkey(
                        *hk,
                        fence_id,
                        &spec,
                        &current_selected,
                        shared,
                        &mut item_action,
                    );
                    handled_ctrl_from_win32 = true;
                }
            }
            let current_selected = shared
                .lock()
                .unwrap()
                .selected_items
                .get(&fence_id)
                .cloned()
                .unwrap_or_default();
            if can_handle_hotkeys && item_action.is_none() && has_input_focus {
                handle_keyboard_shortcuts(
                    fence_id,
                    &spec,
                    grid_cols,
                    &current_selected,
                    handled_ctrl_from_win32 || my_hwnd != 0,
                    shared,
                    &ctx,
                    &mut item_action,
                );
            }

            if let Some(act) = item_action {
                execute_item_action(
                    act,
                    fence_id,
                    my_hwnd,
                    &spec,
                    &current_selected,
                    shared,
                    &ctx,
                    &mut out,
                );
            }
        });
    });

    out
}

enum ItemUiAction {
    Select { idx: usize, ctrl: bool, shift: bool },
    ContextSelect(usize),
    Launch(String),
    LaunchAsAdmin(String),
    LaunchSelected,
    CopySelected { cut: bool },
    CopyPaths,
    PasteFromClipboard,
    UndoLast,
    BeginRename(usize),
    ShowProperties(String),
    Reveal(String),
    RefreshPortal,
    RemoveSelected,
    TrashSelected,
    BeginItemDrag(usize),
    StartDragOut(usize),
}

#[allow(clippy::too_many_arguments)]
fn handle_item_interactions(
    resp: &egui::Response,
    my_hwnd: isize,
    idx: usize,
    item: &desklite_core::FenceItem,
    is_selected: bool,
    selected_count: usize,
    is_portal: bool,
    out_action: &mut Option<ItemUiAction>,
) -> Option<egui::Id> {
    let focused_now = resp.ctx.input(|i| i.focused);
    if resp.clicked() {
        if !focused_now {
            focus_window_now(my_hwnd);
        }
        let mods = resp.ctx.input(|i| i.modifiers);
        *out_action = Some(ItemUiAction::Select {
            idx,
            ctrl: mods.command || is_ctrl_key_down(),
            shift: mods.shift,
        });
    }
    if resp.secondary_clicked() {
        if !focused_now {
            focus_window_now(my_hwnd);
        }
        if !is_selected {
            *out_action = Some(ItemUiAction::ContextSelect(idx));
        }
    }
    if resp.double_clicked() {
        *out_action = Some(ItemUiAction::Launch(item.path.clone()));
    }
    if resp.drag_started_by(PointerButton::Primary) {
        if is_portal {
            *out_action = Some(ItemUiAction::StartDragOut(idx));
        } else {
            *out_action = Some(ItemUiAction::BeginItemDrag(idx));
        }
    }
    let multi = is_selected && selected_count > 1;
    let copy_lbl = if multi {
        format!("复制 ({selected_count} 项) (C)  Ctrl+C")
    } else {
        "复制 (C)  Ctrl+C".to_string()
    };
    let cut_lbl = if multi {
        format!("剪切 ({selected_count} 项) (T)  Ctrl+X")
    } else {
        "剪切 (T)  Ctrl+X".to_string()
    };
    let remove_lbl = if multi {
        format!("移出格子 ({selected_count} 项) (E)")
    } else if is_directly_on_desktop(Path::new(&item.path)) {
        "移出格子（放回桌面）(E)".to_string()
    } else {
        "从格子移除 (E)".to_string()
    };
    let is_exec = Path::new(&item.path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "exe" | "lnk" | "bat" | "cmd" | "msi"
            )
        });
    resp.context_menu(|ui| {
        let (k_o, k_a, k_i, k_t, k_c, k_l, k_m, k_e, k_d, k_r) = ui.input(|i| {
            let nm = !i.modifiers.any();
            (
                nm && i.key_pressed(Key::O),
                nm && i.key_pressed(Key::A),
                nm && i.key_pressed(Key::I),
                nm && i.key_pressed(Key::T),
                nm && i.key_pressed(Key::C),
                nm && i.key_pressed(Key::L),
                nm && i.key_pressed(Key::M),
                nm && i.key_pressed(Key::E),
                nm && i.key_pressed(Key::D),
                nm && i.key_pressed(Key::R),
            )
        });
        if ui.button("打开 (O)  Enter").clicked() || k_o {
            if multi {
                *out_action = Some(ItemUiAction::LaunchSelected);
            } else {
                *out_action = Some(ItemUiAction::Launch(item.path.clone()));
            }
            ui.close();
        }
        if !multi && is_exec && (ui.button("以管理员身份运行 (A)").clicked() || k_a) {
            *out_action = Some(ItemUiAction::LaunchAsAdmin(item.path.clone()));
            ui.close();
        }
        if ui.button("打开所在位置 (I)").clicked() || k_i {
            *out_action = Some(ItemUiAction::Reveal(item.path.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button(cut_lbl).clicked() || k_t {
            *out_action = Some(ItemUiAction::CopySelected { cut: true });
            ui.close();
        }
        if ui.button(copy_lbl).clicked() || k_c {
            *out_action = Some(ItemUiAction::CopySelected { cut: false });
            ui.close();
        }
        if ui.button("复制路径 (L)  Ctrl+Shift+C").clicked() || k_l {
            *out_action = Some(ItemUiAction::CopyPaths);
            ui.close();
        }
        if !multi && (ui.button("重命名 (M)  F2").clicked() || k_m) {
            *out_action = Some(ItemUiAction::BeginRename(idx));
            ui.close();
        }
        ui.separator();
        if !is_portal && (ui.button(remove_lbl).clicked() || k_e) {
            *out_action = Some(ItemUiAction::RemoveSelected);
            ui.close();
        }
        let trash_lbl = if multi {
            format!("删除到回收站 ({selected_count} 项) (D)")
        } else {
            "删除到回收站 (D)".to_string()
        };
        if ui.button(trash_lbl).clicked() || k_d {
            *out_action = Some(ItemUiAction::TrashSelected);
            ui.close();
        }
        if !multi && (ui.button("属性 (R)  Alt+Enter").clicked() || k_r) {
            *out_action = Some(ItemUiAction::ShowProperties(item.path.clone()));
            ui.close();
        }
    })
    .map(|_| egui::Popup::default_response_id(resp))
}

fn dispatch_win32_hotkey(
    hotkey: Win32Hotkey,
    fence_id: i32,
    spec: &FenceSpec,
    selected_set: &HashSet<usize>,
    shared: &Arc<Mutex<SharedFenceState>>,
    out_action: &mut Option<ItemUiAction>,
) {
    match hotkey {
        Win32Hotkey::SelectAll => {
            if !spec.items.is_empty() {
                let mut guard = shared.lock().unwrap();
                guard
                    .selected_items
                    .insert(fence_id, (0..spec.items.len()).collect());
                guard.selection_anchor.insert(fence_id, 0);
            }
        }
        Win32Hotkey::CopyPaths => {
            if !selected_set.is_empty() {
                *out_action = Some(ItemUiAction::CopyPaths);
            }
        }
        Win32Hotkey::Copy => {
            if !selected_set.is_empty() {
                *out_action = Some(ItemUiAction::CopySelected { cut: false });
            }
        }
        Win32Hotkey::Cut => {
            if !selected_set.is_empty() {
                *out_action = Some(ItemUiAction::CopySelected { cut: true });
            }
        }
        Win32Hotkey::Paste => {
            *out_action = Some(ItemUiAction::PasteFromClipboard);
        }
        Win32Hotkey::Undo => {
            *out_action = Some(ItemUiAction::UndoLast);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_keyboard_shortcuts(
    fence_id: i32,
    spec: &FenceSpec,
    grid_cols: usize,
    selected_set: &HashSet<usize>,
    skip_ctrl_actions: bool,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out_action: &mut Option<ItemUiAction>,
) {
    let is_portal = spec.portal_dir.is_some();
    let is_list = matches!(spec.kind, FenceKind::FileList);
    let cols = if is_list { 1 } else { grid_cols.max(1) };

    let (
        ctrl_a,
        ctrl_c,
        ctrl_shift_c,
        ctrl_x,
        ctrl_v,
        ctrl_z,
        key_f2,
        alt_enter,
        key_esc,
        key_enter,
        key_del,
        shift_del,
        nav_delta,
        shift_held,
        typed_text,
    ) = ctx.input(|i| {
        let ctrl_down = i.modifiers.command || is_ctrl_key_down();
        let cmd = !skip_ctrl_actions && ctrl_down && !i.modifiers.alt;
        let shift = i.modifiers.shift;
        let nav = if i.key_pressed(Key::ArrowLeft) {
            Some(-1_isize)
        } else if i.key_pressed(Key::ArrowRight) {
            Some(1)
        } else if i.key_pressed(Key::ArrowUp) {
            Some(-(cols as isize))
        } else if i.key_pressed(Key::ArrowDown) {
            Some(cols as isize)
        } else if i.key_pressed(Key::Home) {
            Some(isize::MIN / 2)
        } else if i.key_pressed(Key::End) {
            Some(isize::MAX / 2)
        } else {
            None
        };
        let mut typed = String::new();
        if !ctrl_down && !i.modifiers.alt {
            for ev in &i.events {
                if let egui::Event::Text(s) = ev {
                    typed.push_str(s);
                }
            }
        }
        (
            cmd && !shift && i.key_pressed(Key::A),
            cmd && !shift && i.key_pressed(Key::C),
            cmd && shift && i.key_pressed(Key::C),
            cmd && !shift && i.key_pressed(Key::X),
            cmd && !shift && i.key_pressed(Key::V),
            cmd && !shift && i.key_pressed(Key::Z),
            i.key_pressed(Key::F2),
            i.modifiers.alt && i.key_pressed(Key::Enter),
            i.key_pressed(Key::Escape),
            !i.modifiers.alt && i.key_pressed(Key::Enter),
            !shift && i.key_pressed(Key::Delete),
            shift && i.key_pressed(Key::Delete),
            nav,
            shift,
            typed,
        )
    });

    if ctrl_a {
        dispatch_win32_hotkey(
            Win32Hotkey::SelectAll,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctrl_shift_c {
        dispatch_win32_hotkey(
            Win32Hotkey::CopyPaths,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctrl_c {
        dispatch_win32_hotkey(
            Win32Hotkey::Copy,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctrl_x {
        dispatch_win32_hotkey(
            Win32Hotkey::Cut,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctrl_v {
        dispatch_win32_hotkey(
            Win32Hotkey::Paste,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctrl_z {
        dispatch_win32_hotkey(
            Win32Hotkey::Undo,
            fence_id,
            spec,
            selected_set,
            shared,
            out_action,
        );
    } else if ctx.input(|i| i.key_pressed(Key::F5)) && is_portal {
        *out_action = Some(ItemUiAction::RefreshPortal);
    } else if key_f2 && !selected_set.is_empty() {
        let first = selected_set.iter().copied().min().unwrap_or(0);
        *out_action = Some(ItemUiAction::BeginRename(first));
    } else if alt_enter && !selected_set.is_empty() {
        if let Some(first) = selected_set.iter().copied().min()
            && let Some(it) = spec.items.get(first)
        {
            *out_action = Some(ItemUiAction::ShowProperties(it.path.clone()));
        }
    } else if key_esc {
        let mut guard = shared.lock().unwrap();
        guard.selected_items.remove(&fence_id);
        guard.selection_anchor.remove(&fence_id);
        guard.cut_paths.clear();
    } else if key_enter && !selected_set.is_empty() {
        *out_action = Some(ItemUiAction::LaunchSelected);
    } else if shift_del && !selected_set.is_empty() {
        *out_action = Some(ItemUiAction::TrashSelected);
    } else if key_del && !selected_set.is_empty() {
        if is_portal {
            *out_action = Some(ItemUiAction::TrashSelected);
        } else {
            *out_action = Some(ItemUiAction::RemoveSelected);
        }
    } else if let Some(delta) = nav_delta
        && !spec.items.is_empty()
    {
        let len = spec.items.len();
        let mut guard = shared.lock().unwrap();
        let cur = guard
            .selection_anchor
            .get(&fence_id)
            .copied()
            .or_else(|| selected_set.iter().copied().min())
            .unwrap_or(0)
            .min(len - 1);
        let next = if delta <= -(isize::MAX / 4) {
            0
        } else if delta >= isize::MAX / 4 {
            len - 1
        } else {
            (cur as isize + delta).clamp(0, (len - 1) as isize) as usize
        };
        if shift_held {
            let set = guard.selected_items.entry(fence_id).or_default();
            set.clear();
            for i in cur.min(next)..=cur.max(next) {
                set.insert(i);
            }
        } else {
            let set = guard.selected_items.entry(fence_id).or_default();
            set.clear();
            set.insert(next);
            guard.selection_anchor.insert(fence_id, next);
        }
        guard.scroll_to_item.insert(fence_id, next);
    } else if !typed_text.trim().is_empty() && !spec.items.is_empty() {
        let mut guard = shared.lock().unwrap();
        let now = Instant::now();
        let next_q = match guard.type_ahead.get(&fence_id) {
            Some((prev, at)) if now.duration_since(*at) < Duration::from_millis(850) => {
                format!("{prev}{}", typed_text.to_lowercase())
            }
            _ => typed_text.to_lowercase(),
        };
        guard.type_ahead.insert(fence_id, (next_q.clone(), now));
        let start = guard
            .selected_items
            .get(&fence_id)
            .and_then(|s| s.iter().copied().min())
            .map(|i| (i + 1) % spec.items.len())
            .unwrap_or(0);
        let hit = (0..spec.items.len())
            .map(|off| (start + off) % spec.items.len())
            .find(|&idx| spec.items[idx].display_name.to_lowercase().starts_with(&next_q))
            .or_else(|| {
                let ch_q = typed_text.to_lowercase();
                (0..spec.items.len())
                    .map(|off| (start + off) % spec.items.len())
                    .find(|&idx| spec.items[idx].display_name.to_lowercase().starts_with(&ch_q))
            });
        if let Some(idx) = hit {
            let set = guard.selected_items.entry(fence_id).or_default();
            set.clear();
            set.insert(idx);
            guard.selection_anchor.insert(fence_id, idx);
            guard.scroll_to_item.insert(fence_id, idx);
        }
    }
}

fn sorted_selected_paths(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> Vec<String> {
    let mut sorted: Vec<usize> = {
        let guard = shared.lock().unwrap();
        guard
            .selected_items
            .get(&fence_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    };
    sorted.sort_unstable();
    sorted
        .into_iter()
        .filter_map(|idx| spec.items.get(idx).map(|it| it.path.clone()))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn execute_item_action(
    act: ItemUiAction,
    fence_id: i32,
    my_hwnd: isize,
    spec: &FenceSpec,
    selected_set: &HashSet<usize>,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    match act {
        ItemUiAction::Select { idx, ctrl, shift } => {
            let mut guard = shared.lock().unwrap();
            let anchor = guard.selection_anchor.get(&fence_id).copied();
            let set = guard.selected_items.entry(fence_id).or_default();
            if shift {
                let start = anchor.unwrap_or(0);
                if !ctrl {
                    set.clear();
                }
                for i in start.min(idx)..=start.max(idx) {
                    set.insert(i);
                }
            } else if ctrl {
                if !set.remove(&idx) {
                    set.insert(idx);
                }
                guard.selection_anchor.insert(fence_id, idx);
            } else {
                set.clear();
                set.insert(idx);
                guard.selection_anchor.insert(fence_id, idx);
            }
        }
        ItemUiAction::ContextSelect(idx) => {
            let mut guard = shared.lock().unwrap();
            let set = guard.selected_items.entry(fence_id).or_default();
            if !set.contains(&idx) {
                set.clear();
                set.insert(idx);
                guard.selection_anchor.insert(fence_id, idx);
            }
        }
        ItemUiAction::Launch(path) => {
            let _ = launch_item(&path);
        }
        ItemUiAction::LaunchAsAdmin(path) => {
            let _ = launch_as_admin(&path);
        }
        ItemUiAction::LaunchSelected => {
            let mut sorted: Vec<usize> = selected_set.iter().copied().collect();
            sorted.sort_unstable();
            for idx in sorted.into_iter().take(8) {
                if let Some(it) = spec.items.get(idx) {
                    let _ = launch_item(&it.path);
                }
            }
        }
        ItemUiAction::CopySelected { cut } => {
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if !paths.is_empty() {
                if set_clipboard_files_for_window(my_hwnd, &paths, cut).is_ok() {
                    let mut guard = shared.lock().unwrap();
                    guard.cut_paths.clear();
                    if cut {
                        for p in &paths {
                            guard.cut_paths.insert(p.to_ascii_lowercase());
                        }
                    }
                    let verb = if cut { "已剪切" } else { "已复制" };
                    guard.toast_msg.insert(
                        fence_id,
                        (format!("{verb} {} 项", paths.len()), Instant::now()),
                    );
                    ctx.request_repaint();
                } else {
                    let mut guard = shared.lock().unwrap();
                    guard
                        .toast_msg
                        .insert(fence_id, ("剪贴板被占用，请重试".to_string(), Instant::now()));
                    ctx.request_repaint();
                }
            }
        }
        ItemUiAction::CopyPaths => {
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if !paths.is_empty() && copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                let mut guard = shared.lock().unwrap();
                guard
                    .toast_msg
                    .insert(fence_id, ("已复制路径".to_string(), Instant::now()));
                ctx.request_repaint();
            }
        }
        ItemUiAction::PasteFromClipboard => {
            handle_paste_action(fence_id, spec, shared, ctx, out);
        }
        ItemUiAction::UndoLast => {
            execute_undo(fence_id, shared, ctx, out);
        }
        ItemUiAction::BeginRename(idx) => {
            if let Some(it) = spec.items.get(idx) {
                let file_name = Path::new(&it.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| it.display_name.clone());
                let mut guard = shared.lock().unwrap();
                guard.item_rename_prompt = Some(ItemRenamePrompt {
                    fence_id,
                    item_idx: idx,
                    original_path: it.path.clone(),
                    draft: file_name,
                    needs_focus: true,
                });
            }
        }
        ItemUiAction::ShowProperties(path) => {
            let _ = show_file_properties(&path);
        }
        ItemUiAction::Reveal(path) => {
            let _ = reveal_in_explorer(&path);
        }
        ItemUiAction::RefreshPortal => {
            let managed = {
                let mut guard = shared.lock().unwrap();
                if let Some(f) = guard.config.fence_mut(fence_id)
                    && let Some(ref dir) = f.portal_dir
                {
                    f.items = scan_portal_directory(Path::new(dir));
                }
                collect_managed_desktop_paths(&guard.config)
            };
            hide_desktop_icons_for_paths_async(managed);
            ctx.request_repaint();
        }
        ItemUiAction::RemoveSelected => {
            let (removed_paths, remaining) = {
                let mut guard = shared.lock().unwrap();
                let to_remove = guard
                    .selected_items
                    .get(&fence_id)
                    .cloned()
                    .unwrap_or_default();
                let mut removed_paths = Vec::new();
                let mut removed_items = Vec::new();
                if !to_remove.is_empty()
                    && let Some(f) = guard.config.fence_mut(fence_id)
                    && f.portal_dir.is_none()
                {
                    let mut kept = Vec::with_capacity(f.items.len());
                    for (idx, it) in f.items.drain(..).enumerate() {
                        if to_remove.contains(&idx) {
                            removed_paths.push(it.path.clone());
                            removed_items.push(it);
                        } else {
                            kept.push(it);
                        }
                    }
                    for (i, it) in kept.iter_mut().enumerate() {
                        it.order = i as i32;
                    }
                    f.items = kept;
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                    if !removed_items.is_empty() {
                        guard.push_undo(UndoAction::RemoveItems {
                            fence_id,
                            items: removed_items,
                        });
                    }
                    guard.dirty_layout = true;
                    out.needs_immediate_save = true;
                }
                (removed_paths, collect_managed_desktop_paths(&guard.config))
            };
            if !removed_paths.is_empty() {
                for rp in removed_paths {
                    if is_directly_on_desktop(Path::new(&rp)) {
                        restore_desktop_icon_async(rp, None, remaining.clone());
                    }
                }
                ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fence_id)));
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        }
        ItemUiAction::TrashSelected => {
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if !paths.is_empty() && trash_files(&paths).unwrap_or(false) {
                let mut guard = shared.lock().unwrap();
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    if let Some(ref dir) = f.portal_dir {
                        f.items = scan_portal_directory(Path::new(dir));
                    } else {
                        f.items.retain(|it| {
                            !paths.iter().any(|p| it.path.eq_ignore_ascii_case(p))
                        });
                        for (i, it) in f.items.iter_mut().enumerate() {
                            it.order = i as i32;
                        }
                    }
                }
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
                ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fence_id)));
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        }
        ItemUiAction::BeginItemDrag(trigger_idx) => {
            let mut guard = shared.lock().unwrap();
            let cur_set = guard
                .selected_items
                .get(&fence_id)
                .cloned()
                .unwrap_or_default();
            let mut indices: Vec<usize> = if cur_set.contains(&trigger_idx) && cur_set.len() > 1 {
                cur_set.into_iter().collect()
            } else {
                let set = guard.selected_items.entry(fence_id).or_default();
                set.clear();
                set.insert(trigger_idx);
                guard.selection_anchor.insert(fence_id, trigger_idx);
                vec![trigger_idx]
            };
            indices.sort_unstable();
            let paths: Vec<String> = indices
                .iter()
                .filter_map(|&i| spec.items.get(i).map(|it| it.path.clone()))
                .collect();
            if !paths.is_empty() {
                guard.item_reorder.insert(
                    fence_id,
                    ReorderDragState {
                        trigger_idx,
                        indices,
                        paths,
                    },
                );
            }
        }
        ItemUiAction::StartDragOut(trigger_idx) => {
            let drag_paths: Vec<String> = {
                let mut guard = shared.lock().unwrap();
                let cur_set = guard
                    .selected_items
                    .get(&fence_id)
                    .cloned()
                    .unwrap_or_default();
                let mut indices: Vec<usize> = if cur_set.contains(&trigger_idx) && cur_set.len() > 1 {
                    cur_set.into_iter().collect()
                } else {
                    let set = guard.selected_items.entry(fence_id).or_default();
                    set.clear();
                    set.insert(trigger_idx);
                    guard.selection_anchor.insert(fence_id, trigger_idx);
                    vec![trigger_idx]
                };
                indices.sort_unstable();
                indices
                    .into_iter()
                    .filter_map(|i| spec.items.get(i).map(|it| it.path.clone()))
                    .collect()
            };
            if !drag_paths.is_empty() {
                let any_desktop_res = drag_paths
                    .iter()
                    .any(|p| is_directly_on_desktop(Path::new(p)));
                let fence_hwnds: Vec<isize> = {
                    let mut guard = shared.lock().unwrap();
                    guard.dragging_from_fence = Some(DragSourceState {
                        fence_id,
                        paths: drag_paths.clone(),
                        finished_at: None,
                    });
                    guard.attached_hwnds.values().copied().collect()
                };
                let outcome = start_file_drag_out(&drag_paths, any_desktop_res, &fence_hwnds);
                let restore_desktop_at: Option<(i32, i32, Vec<String>, Vec<String>)> = {
                    let mut guard = shared.lock().unwrap();
                    let unconsumed = guard.dragging_from_fence.is_some();
                    let all_gone_from_disk = drag_paths.iter().all(|p| !Path::new(p).exists());
                    match outcome {
                        Ok(DragOutOutcome::DroppedOnDesktop { x, y }) => {
                            guard.dragging_from_fence = None;
                            let mut removed_items = Vec::new();
                            if let Some(f) = guard.config.fence_mut(fence_id)
                                && f.portal_dir.is_none()
                            {
                                let mut kept = Vec::with_capacity(f.items.len());
                                for it in f.items.drain(..) {
                                    if drag_paths
                                        .iter()
                                        .any(|dp| it.path.eq_ignore_ascii_case(dp))
                                    {
                                        removed_items.push(it);
                                    } else {
                                        kept.push(it);
                                    }
                                }
                                for (i, it) in kept.iter_mut().enumerate() {
                                    it.order = i as i32;
                                }
                                f.items = kept;
                                guard.selected_items.remove(&fence_id);
                                guard.selection_anchor.remove(&fence_id);
                                if !removed_items.is_empty() {
                                    guard.push_undo(UndoAction::RemoveItems {
                                        fence_id,
                                        items: removed_items,
                                    });
                                }
                                guard.dirty_layout = true;
                                out.needs_immediate_save = true;
                            }
                            let desktop_paths: Vec<String> = drag_paths
                                .iter()
                                .filter(|p| is_directly_on_desktop(Path::new(p)))
                                .cloned()
                                .collect();
                            Some((
                                x,
                                y,
                                desktop_paths,
                                collect_managed_desktop_paths(&guard.config),
                            ))
                        }
                        Ok(DragOutOutcome::DroppedExternal { moved }) => {
                            let should_remove = unconsumed && (moved || all_gone_from_disk);
                            if should_remove {
                                guard.dragging_from_fence = None;
                                let mut removed_items = Vec::new();
                                if let Some(f) = guard.config.fence_mut(fence_id)
                                    && f.portal_dir.is_none()
                                {
                                    let mut kept = Vec::with_capacity(f.items.len());
                                    for it in f.items.drain(..) {
                                        if drag_paths
                                            .iter()
                                            .any(|dp| it.path.eq_ignore_ascii_case(dp))
                                        {
                                            removed_items.push(it);
                                        } else {
                                            kept.push(it);
                                        }
                                    }
                                    for (i, it) in kept.iter_mut().enumerate() {
                                        it.order = i as i32;
                                    }
                                    f.items = kept;
                                    guard.selected_items.remove(&fence_id);
                                    guard.selection_anchor.remove(&fence_id);
                                    if !removed_items.is_empty() && !all_gone_from_disk && !moved {
                                        guard.push_undo(UndoAction::RemoveItems {
                                            fence_id,
                                            items: removed_items,
                                        });
                                    }
                                    guard.dirty_layout = true;
                                    out.needs_immediate_save = true;
                                }
                            } else if let Some(ref mut ds) = guard.dragging_from_fence {
                                ds.finished_at = Some(Instant::now());
                            }
                            None
                        }
                        Ok(DragOutOutcome::Cancelled) | Err(_) => {
                            guard.dragging_from_fence = None;
                            None
                        }
                    }
                };
                if let Some((dx, dy, desktop_paths, remaining)) = restore_desktop_at {
                    for (i, dp) in desktop_paths.into_iter().enumerate() {
                        let ox = (i as i32 % 4) * 86;
                        let oy = (i as i32 / 4) * 96;
                        restore_desktop_icon_async(
                            dp,
                            Some((dx + ox, dy + oy)),
                            remaining.clone(),
                        );
                    }
                }
                ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fence_id)));
                ctx.request_repaint_of(ViewportId::ROOT);
            }
        }
    }
}

fn handle_paste_action(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let Some((sources, is_cut)) = read_clipboard_files() else {
        if clipboard_has_files() {
            shared.lock().unwrap().toast_msg.insert(
                fence_id,
                ("剪贴板正忙，请重试".to_string(), Instant::now()),
            );
            ctx.request_repaint();
        }
        return;
    };
    if let Some(ref dir) = spec.portal_dir {
        if let Ok(pairs) = paste_files_to_directory_detailed(&sources, Path::new(dir), is_cut)
            && !pairs.is_empty()
        {
            let mut moved_pairs = Vec::new();
            let mut created_paths = Vec::new();
            for (src, dst) in &pairs {
                if is_cut {
                    moved_pairs.push((
                        src.to_string_lossy().into_owned(),
                        dst.to_string_lossy().into_owned(),
                    ));
                } else {
                    created_paths.push(dst.to_string_lossy().into_owned());
                }
            }
            let mut removed_from_fences = Vec::new();
            let mut guard = shared.lock().unwrap();
            if is_cut {
                for f in &mut guard.config.fences {
                    if f.id != fence_id && f.portal_dir.is_none() {
                        let mut removed = Vec::new();
                        let mut kept = Vec::with_capacity(f.items.len());
                        for it in f.items.drain(..) {
                            if sources.iter().any(|s| it.path.eq_ignore_ascii_case(s)) {
                                removed.push(it);
                            } else {
                                kept.push(it);
                            }
                        }
                        if !removed.is_empty() {
                            for (i, it) in kept.iter_mut().enumerate() {
                                it.order = i as i32;
                            }
                            removed_from_fences.push((f.id, removed));
                        }
                        f.items = kept;
                    }
                }
                guard.cut_paths.clear();
            }
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.items = scan_portal_directory(Path::new(dir));
            }
            guard.push_undo(UndoAction::PasteFiles {
                fence_id,
                created_paths,
                moved_pairs,
                removed_from_fences,
                added_paths: Vec::new(),
                is_portal: true,
            });
            guard
                .toast_msg
                .insert(fence_id, (format!("已粘贴 {} 项", pairs.len()), Instant::now()));
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            let managed = collect_managed_desktop_paths(&guard.config);
            drop(guard);
            hide_desktop_icons_for_paths_async(managed.clone());
            hide_desktop_icons_for_paths_delayed(managed, 220);
            ctx.request_repaint();
        }
        return;
    }

    let mut created_paths = Vec::new();
    let mut moved_pairs = Vec::new();
    let target_paths: Vec<String> = if is_cut {
        if let Some(desktop) = user_desktop_dir() {
            let mut moved_paths = Vec::new();
            let mut passthrough = Vec::new();
            for s in &sources {
                let sp = Path::new(s);
                if is_directly_on_desktop(sp) {
                    passthrough.push(s.clone());
                } else if let Ok(pairs) =
                    paste_files_to_directory_detailed(std::slice::from_ref(s), &desktop, true)
                {
                    for (src, dst) in pairs {
                        let ds = dst.to_string_lossy().into_owned();
                        moved_pairs.push((src.to_string_lossy().into_owned(), ds.clone()));
                        moved_paths.push(ds);
                    }
                }
            }
            moved_paths.extend(passthrough);
            moved_paths
        } else {
            sources.clone()
        }
    } else if let Some(desktop) = user_desktop_dir() {
        let mut out_paths = Vec::new();
        for s in &sources {
            let already_here = spec.items.iter().any(|it| it.path.eq_ignore_ascii_case(s));
            let already_managed_anywhere = shared.lock().unwrap().config.fences.iter().any(|f| {
                f.items.iter().any(|it| it.path.eq_ignore_ascii_case(s))
            });
            if already_here || already_managed_anywhere {
                if let Ok(pairs) =
                    paste_files_to_directory_detailed(std::slice::from_ref(s), &desktop, false)
                {
                    for (_, dst) in pairs {
                        let ds = dst.to_string_lossy().into_owned();
                        created_paths.push(ds.clone());
                        out_paths.push(ds);
                    }
                }
            } else {
                out_paths.push(s.clone());
            }
        }
        out_paths
    } else {
        sources.clone()
    };

    if target_paths.is_empty() {
        return;
    }
    let managed = {
        let mut guard = shared.lock().unwrap();
        let mut removed_from_fences = Vec::new();
        if is_cut {
            for f in &mut guard.config.fences {
                if f.id != fence_id && f.portal_dir.is_none() {
                    let mut removed = Vec::new();
                    let mut kept = Vec::with_capacity(f.items.len());
                    for it in f.items.drain(..) {
                        if sources.iter().any(|s| it.path.eq_ignore_ascii_case(s)) {
                            removed.push(it);
                        } else {
                            kept.push(it);
                        }
                    }
                    if !removed.is_empty() {
                        for (i, it) in kept.iter_mut().enumerate() {
                            it.order = i as i32;
                        }
                        removed_from_fences.push((f.id, removed));
                    }
                    f.items = kept;
                }
            }
            guard.cut_paths.clear();
        }
        let mut added_paths = Vec::new();
        if let Some(f) = guard.config.fence_mut(fence_id) {
            for p in &target_paths {
                if !f.items.iter().any(|it| it.path.eq_ignore_ascii_case(p)) {
                    let ord = f.items.len() as i32;
                    let item = resolve_dropped_item(Path::new(p), ord);
                    added_paths.push(item.path.clone());
                    f.items.push(item);
                }
            }
        }
        let added = added_paths.len();
        if added > 0 || !created_paths.is_empty() || !moved_pairs.is_empty() || !removed_from_fences.is_empty() {
            guard.push_undo(UndoAction::PasteFiles {
                fence_id,
                created_paths,
                moved_pairs,
                removed_from_fences,
                added_paths,
                is_portal: false,
            });
            guard
                .toast_msg
                .insert(fence_id, (format!("已粘贴 {added} 项"), Instant::now()));
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
        }
        collect_managed_desktop_paths(&guard.config)
    };
    hide_desktop_icons_for_paths_async(managed.clone());
    hide_desktop_icons_for_paths_delayed(managed, 220);
    ctx.request_repaint();
}

fn commit_item_rename(
    fence_id: i32,
    idx: usize,
    old_path: &str,
    new_name: &str,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let old_p = Path::new(old_path);
    let trimmed = new_name.trim();
    if trimmed.is_empty()
        || old_p
            .file_name()
            .is_some_and(|n| n == std::ffi::OsStr::new(trimmed))
    {
        shared.lock().unwrap().item_rename_prompt = None;
        return;
    }
    if let Ok(mut updated) = rename_file_in_place(old_p, trimmed) {
        let new_path = updated.path.clone();
        let managed = {
            let mut guard = shared.lock().unwrap();
            guard.textures.remove(&old_path.to_ascii_lowercase());
            if let Some(f) = guard.config.fence_mut(fence_id) {
                if let Some(ref dir) = f.portal_dir {
                    f.items = scan_portal_directory(Path::new(dir));
                } else if idx < f.items.len() {
                    updated.order = idx as i32;
                    f.items[idx] = updated;
                }
            }
            guard.push_undo(UndoAction::RenameItem {
                fence_id,
                old_path: old_path.to_string(),
                new_path,
            });
            guard.item_rename_prompt = None;
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            collect_managed_desktop_paths(&guard.config)
        };
        hide_desktop_icons_for_paths_async(managed.clone());
        hide_desktop_icons_for_paths_delayed(managed, 220);
        ctx.request_repaint();
    } else {
        shared.lock().unwrap().item_rename_prompt = None;
    }
}

fn compute_reorder_target(
    item_rects: &[(usize, egui::Rect)],
    pos: Pos2,
    is_list: bool,
) -> Option<usize> {
    if item_rects.is_empty() {
        return None;
    }
    let mut best_idx = 0usize;
    let mut best_dist = f32::MAX;
    for &(idx, r) in item_rects {
        let c = r.center();
        let d = (pos - c).length_sq();
        if d < best_dist {
            best_dist = d;
            best_idx = if is_list {
                if pos.y > c.y { idx + 1 } else { idx }
            } else if pos.x > c.x {
                idx + 1
            } else {
                idx
            };
        }
    }
    Some(best_idx)
}

fn paint_reorder_indicator(
    ui: &Ui,
    clip_rect: egui::Rect,
    item_rects: &[(usize, egui::Rect)],
    insert_idx: usize,
    is_list: bool,
) {
    let painter = ui.painter().with_clip_rect(clip_rect);
    let accent = Stroke::new(2.0, Color32::from_rgb(96, 175, 255));
    if let Some((_, r)) = item_rects.iter().find(|(i, _)| *i == insert_idx) {
        if is_list {
            painter.hline(r.x_range(), r.min.y - 1.0, accent);
        } else {
            painter.vline(r.min.x - 2.0, r.y_range(), accent);
        }
    } else if let Some((_, last_r)) = item_rects.last() {
        if is_list {
            painter.hline(last_r.x_range(), last_r.max.y + 1.0, accent);
        } else {
            painter.vline(last_r.max.x + 2.0, last_r.y_range(), accent);
        }
    }
}

fn apply_reorder_items(
    fence_id: i32,
    moving_indices: &[usize],
    insert_idx: usize,
    shared: &Arc<Mutex<SharedFenceState>>,
    out: &mut FenceUiOutput,
) {
    if moving_indices.is_empty() {
        return;
    }
    let mut guard = shared.lock().unwrap();
    let old_items = guard
        .config
        .fence_mut(fence_id)
        .filter(|f| f.portal_dir.is_none())
        .map(|f| f.items.clone());
    let mut changed = false;
    if let Some(f) = guard.config.fence_mut(fence_id)
        && f.portal_dir.is_none()
    {
        let moving_set: HashSet<usize> = moving_indices.iter().copied().collect();
        let mut before_count = 0usize;
        for &m in moving_indices {
            if m < insert_idx {
                before_count += 1;
            }
        }
        let target_in_kept = insert_idx.saturating_sub(before_count);
        let mut kept = Vec::with_capacity(f.items.len());
        let mut moved = Vec::with_capacity(moving_indices.len());
        for (i, it) in f.items.drain(..).enumerate() {
            if moving_set.contains(&i) {
                moved.push(it);
            } else {
                kept.push(it);
            }
        }
        let pos = target_in_kept.min(kept.len());
        let mut rebuilt = Vec::with_capacity(kept.len() + moved.len());
        rebuilt.extend(kept.drain(..pos));
        let new_sel: HashSet<usize> = (pos..pos + moved.len()).collect();
        rebuilt.extend(moved);
        rebuilt.extend(kept);
        for (i, it) in rebuilt.iter_mut().enumerate() {
            it.order = i as i32;
        }
        changed = old_items.as_ref().is_some_and(|prev| prev != &rebuilt);
        f.items = rebuilt;
        guard.selected_items.insert(fence_id, new_sel);
        guard.selection_anchor.insert(fence_id, pos);
        guard.dirty_layout = true;
        out.needs_immediate_save = true;
    }
    if changed && let Some(prev) = old_items {
        guard.push_undo(UndoAction::ReorderItems {
            fence_id,
            old_items: prev,
        });
    }
}

fn paint_animated_item_bg(
    ui: &Ui,
    rect: egui::Rect,
    id: egui::Id,
    hovered: bool,
    selected: bool,
    rounding: u8,
) {
    let sel_t = ui.ctx().animate_bool_responsive(id.with("sel"), selected);
    let hov_t = ui
        .ctx()
        .animate_bool_responsive(id.with("hov"), hovered && !selected);
    if sel_t > 0.01 {
        let a = (76.0 * sel_t).round() as u8;
        let ba = (155.0 * sel_t).round() as u8;
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(rounding),
            Color32::from_rgba_unmultiplied(88, 156, 250, a),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(rounding),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(125, 190, 255, ba)),
            egui::StrokeKind::Inside,
        );
    } else if hov_t > 0.01 {
        let a = (34.0 * hov_t).round() as u8;
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(rounding),
            Color32::from_rgba_unmultiplied(255, 255, 255, a),
        );
    }
}

fn grid_item_metrics(size: Vec2) -> (f32, f32, f32) {
    let font_size = (11.2 + (size.x.min(size.y) - 80.0).max(0.0) * 0.035).clamp(11.5, 14.5);
    let est_two_line_h = font_size * 2.55;
    let max_by_w = (size.x * 0.60).max(28.0);
    let max_by_h = (size.y - 14.0 - est_two_line_h).max(28.0);
    let icon_sz = max_by_w.min(max_by_h).clamp(28.0, 96.0).round();
    let gap = (icon_sz * 0.14).clamp(4.0, 9.0).round();
    (icon_sz, font_size, gap)
}

fn render_grid_item(
    ui: &mut Ui,
    item: &desklite_core::FenceItem,
    tex: Option<&TextureHandle>,
    selected: bool,
    is_cut: bool,
    size: Vec2,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut was_truncated = false;
    if ui.is_rect_visible(rect) {
        paint_animated_item_bg(ui, rect.shrink(2.0), resp.id, resp.hovered(), selected, 6);
        let alpha_tint = if is_cut {
            Color32::from_rgba_unmultiplied(255, 255, 255, 130)
        } else {
            Color32::WHITE
        };

        let (icon_sz, font_size, gap) = grid_item_metrics(size);
        let clipped = ui.painter().with_clip_rect(rect.shrink(2.0));
        let font = egui::FontId::proportional(font_size);
        let max_text_w = (size.x - 8.0).max(20.0);
        let fitted = fit_two_lines(ui.painter(), &item.display_name, font.clone(), max_text_w);
        was_truncated = fitted != item.display_name;

        let text_color = if is_cut {
            Color32::from_rgba_unmultiplied(244, 247, 255, 145)
        } else {
            Color32::from_rgb(244, 247, 255)
        };
        let mut shadow_job = LayoutJob::simple(
            fitted.clone(),
            font.clone(),
            Color32::from_rgba_unmultiplied(0, 0, 0, if is_cut { 90 } else { 175 }),
            max_text_w,
        );
        shadow_job.halign = Align::Center;
        let shadow_galley = ui.painter().layout_job(shadow_job);

        let mut label_job = LayoutJob::simple(fitted, font, text_color, max_text_w);
        label_job.halign = Align::Center;
        let label_galley = ui.painter().layout_job(label_job);

        let text_h = label_galley.size().y;
        let content_h = icon_sz + gap + text_h;
        let top_y = (rect.center().y - content_h * 0.5)
            .round()
            .max(rect.min.y + 4.0);

        if let Some(t) = tex {
            let icon_rect = egui::Rect::from_min_size(
                Pos2::new((rect.center().x - icon_sz * 0.5).round(), top_y),
                Vec2::splat(icon_sz),
            );
            ui.painter().image(
                t.id(),
                icon_rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                alpha_tint,
            );
        }

        let center_x = rect.center().x.round();
        let text_y = (top_y + icon_sz + gap).round();
        clipped.galley(Pos2::new(center_x, text_y + 1.0), shadow_galley, Color32::WHITE);
        clipped.galley(Pos2::new(center_x, text_y), label_galley, Color32::WHITE);
    }
    if was_truncated {
        resp.on_hover_text(&item.display_name)
    } else {
        resp
    }
}

fn render_list_item(
    ui: &mut Ui,
    item: &desklite_core::FenceItem,
    tex: Option<&TextureHandle>,
    selected: bool,
    is_cut: bool,
    row_h: f32,
    icon_px: f32,
) -> egui::Response {
    let icon_sz = icon_px.clamp(14.0, (row_h - 6.0).max(14.0));
    let size = Vec2::new(ui.available_width(), row_h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut was_truncated = false;
    if ui.is_rect_visible(rect) {
        paint_animated_item_bg(ui, rect, resp.id, resp.hovered(), selected, 5);
        let alpha_tint = if is_cut {
            Color32::from_rgba_unmultiplied(255, 255, 255, 130)
        } else {
            Color32::WHITE
        };
        if let Some(t) = tex {
            let icon_rect = egui::Rect::from_min_size(
                Pos2::new(rect.min.x + 8.0, (rect.center().y - icon_sz * 0.5).round()),
                Vec2::splat(icon_sz),
            );
            ui.painter().image(
                t.id(),
                icon_rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                alpha_tint,
            );
        }
        let text_x = rect.min.x + 8.0 + icon_sz + 8.0;
        let text_max_w = (rect.max.x - text_x - 6.0).max(20.0);
        let font_sz = (11.5 + (icon_sz - 16.0) * 0.16).clamp(12.0, 15.0);
        let font = egui::FontId::proportional(font_sz);
        let fitted = fit_single_line(ui.painter(), &item.display_name, font.clone(), text_max_w);
        was_truncated = fitted != item.display_name;
        let clipped = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(2.0, 0.0)));
        clipped.text(
            Pos2::new(text_x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            fitted,
            font,
            if is_cut {
                Color32::from_rgba_unmultiplied(240, 244, 252, 145)
            } else {
                Color32::from_rgb(240, 244, 252)
            },
        );
    }
    if was_truncated {
        resp.on_hover_text(&item.display_name)
    } else {
        resp
    }
}

fn file_stem_char_len(name: &str) -> usize {
    if let Some(dot) = name.rfind('.')
        && dot > 0
    {
        return name[..dot].chars().count();
    }
    name.chars().count()
}

fn handle_text_edit_clipboard(ctx: &egui::Context, id: egui::Id, draft: &mut String) {
    let (copy, cut, paste) = ctx.input(|i| {
        let cmd = i.modifiers.command && !i.modifiers.alt && !i.modifiers.shift;
        (
            cmd && i.key_pressed(Key::C),
            cmd && i.key_pressed(Key::X),
            cmd && i.key_pressed(Key::V),
        )
    });
    if !copy && !cut && !paste {
        return;
    }
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
        if copy && !draft.is_empty() {
            let _ = copy_text_to_clipboard(draft);
        }
        return;
    };
    let char_count = draft.chars().count();
    let (sel_start, sel_end) = state
        .cursor
        .char_range()
        .map(|r| {
            let p: usize = r.primary.index.into();
            let q: usize = r.secondary.index.into();
            (p.min(q).min(char_count), p.max(q).min(char_count))
        })
        .unwrap_or((0, char_count));
    let byte_idx = |cidx: usize| -> usize {
        draft
            .char_indices()
            .nth(cidx)
            .map(|(b, _)| b)
            .unwrap_or(draft.len())
    };

    if copy || cut {
        let slice = if sel_end > sel_start {
            &draft[byte_idx(sel_start)..byte_idx(sel_end)]
        } else {
            draft.as_str()
        };
        if !slice.is_empty() {
            let _ = copy_text_to_clipboard(slice);
        }
        if cut && sel_end > sel_start {
            let bs = byte_idx(sel_start);
            let be = byte_idx(sel_end);
            draft.replace_range(bs..be, "");
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(sel_start),
            )));
            state.store(ctx, id);
        }
    } else if paste && let Some(clip) = read_clipboard_text() {
        let clean: String = clip.chars().filter(|&c| c != '\r' && c != '\n').collect();
        if !clean.is_empty() {
            let bs = byte_idx(sel_start);
            let be = byte_idx(sel_end);
            draft.replace_range(bs..be, &clean);
            let next_c = sel_start + clean.chars().count();
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(next_c),
            )));
            state.store(ctx, id);
        }
    }
}

fn handle_rename_text_edit(
    ui: &mut Ui,
    edit_rect: egui::Rect,
    clip_rect: egui::Rect,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> (Option<(usize, String, String)>, bool) {
    let mut commit = None;
    let mut cancel = false;
    let mut guard = shared.lock().unwrap();
    if let Some(ref mut rp) = guard.item_rename_prompt
        && rp.fence_id == fence_id
    {
        // Float inside an egui::Area so editing never widens egui::Grid columns or shifts neighbor icons.
        let te_id = ui.make_persistent_id(("item_rename_te", fence_id, rp.item_idx));
        let box_w = edit_rect.width().max(40.0);
        let box_h = edit_rect.height().max(20.0);
        let area_resp = egui::Area::new(ui.make_persistent_id(("item_rename_area", fence_id)))
            .order(egui::Order::Foreground)
            .fixed_pos(edit_rect.min)
            .constrain_to(clip_rect)
            .show(ctx, |ui| {
                ui.set_clip_rect(clip_rect);
                ui.set_min_size(Vec2::new(box_w, box_h));
                ui.set_max_size(Vec2::new(box_w, box_h));
                ui.add_sized(
                    [box_w, box_h],
                    egui::TextEdit::singleline(&mut rp.draft)
                        .id(te_id)
                        .desired_width((box_w - 8.0).max(20.0))
                        .clip_text(true),
                )
            });
        let out_te = area_resp.inner;
        if rp.needs_focus {
            out_te.request_focus();
            let stem_len = file_stem_char_len(&rp.draft);
            if let Some(mut state) = egui::TextEdit::load_state(ctx, out_te.id) {
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(stem_len),
                )));
                state.store(ctx, out_te.id);
            }
            rp.needs_focus = false;
        }
        if out_te.has_focus() {
            handle_text_edit_clipboard(ctx, out_te.id, &mut rp.draft);
        }
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            let trimmed = rp.draft.trim().to_string();
            if !trimmed.is_empty() {
                commit = Some((rp.item_idx, rp.original_path.clone(), trimmed));
            } else {
                cancel = true;
            }
        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
            cancel = true;
        } else if out_te.lost_focus() {
            let trimmed = rp.draft.trim().to_string();
            if !trimmed.is_empty() {
                commit = Some((rp.item_idx, rp.original_path.clone(), trimmed));
            } else {
                cancel = true;
            }
        }
    }
    (commit, cancel)
}

fn render_grid_item_renaming(
    ui: &mut Ui,
    tex: Option<&TextureHandle>,
    size: Vec2,
    clip_rect: egui::Rect,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> (egui::Response, Option<(usize, String, String)>, bool) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let (icon_sz, font_size, gap) = grid_item_metrics(size);
    let edit_h = (font_size * 1.9).round().clamp(22.0, 28.0);
    let content_h = icon_sz + gap + edit_h;
    let top_y = (rect.center().y - content_h * 0.5)
        .round()
        .max(rect.min.y + 4.0);
    if let Some(t) = tex {
        let icon_rect = egui::Rect::from_min_size(
            Pos2::new((rect.center().x - icon_sz * 0.5).round(), top_y),
            Vec2::splat(icon_sz),
        );
        ui.painter().image(
            t.id(),
            icon_rect,
            egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    let edit_y = (top_y + icon_sz + gap).round();
    let edit_rect = egui::Rect::from_min_max(
        Pos2::new(rect.min.x + 2.0, edit_y),
        Pos2::new(rect.max.x - 2.0, edit_y + edit_h),
    );
    let (commit, cancel) =
        handle_rename_text_edit(ui, edit_rect, clip_rect, fence_id, shared, ctx);
    (resp, commit, cancel)
}

#[allow(clippy::too_many_arguments)]
fn render_list_item_renaming(
    ui: &mut Ui,
    tex: Option<&TextureHandle>,
    row_h: f32,
    icon_px: f32,
    clip_rect: egui::Rect,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> (egui::Response, Option<(usize, String, String)>, bool) {
    let icon_sz = icon_px.clamp(14.0, (row_h - 6.0).max(14.0));
    let size = Vec2::new(ui.available_width(), row_h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if let Some(t) = tex {
        let icon_rect = egui::Rect::from_min_size(
            Pos2::new(rect.min.x + 8.0, (rect.center().y - icon_sz * 0.5).round()),
            Vec2::splat(icon_sz),
        );
        ui.painter().image(
            t.id(),
            icon_rect,
            egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    let edit_x = rect.min.x + 8.0 + icon_sz + 6.0;
    let edit_rect = egui::Rect::from_min_max(
        Pos2::new(edit_x, rect.min.y + 2.0),
        Pos2::new(rect.max.x - 4.0, rect.max.y - 2.0),
    );
    let (commit, cancel) =
        handle_rename_text_edit(ui, edit_rect, clip_rect, fence_id, shared, ctx);
    (resp, commit, cancel)
}

fn draw_header_icon_btn(
    ui: &mut Ui,
    tooltip: &str,
    paint_icon: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let hov_t = ui.ctx().animate_bool_responsive(
            resp.id.with("btn_hov"),
            resp.hovered() || resp.is_pointer_button_down_on(),
        );
        if hov_t > 0.01 {
            let base_a = if resp.is_pointer_button_down_on() {
                48.0
            } else {
                28.0
            };
            let bg = Color32::from_rgba_unmultiplied(255, 255, 255, (base_a * hov_t).round() as u8);
            ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
        }
        let fg = if resp.hovered() {
            Color32::WHITE
        } else {
            Color32::from_rgb(208, 214, 228)
        };
        paint_icon(ui.painter(), rect, fg);
    }
    resp.on_hover_text(tooltip)
}

fn fit_single_line(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    let g = painter.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
    if g.size().x <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi).max(lo + 1);
        let cand: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        let w = painter
            .layout_no_wrap(cand, font.clone(), Color32::WHITE)
            .size()
            .x;
        if w <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().chain(std::iter::once(&'…')).collect()
    }
}

fn fit_two_lines(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    let g = painter.layout(text.to_string(), font.clone(), Color32::WHITE, max_width);
    if g.rows.len() <= 2 {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi).max(lo + 1);
        let cand: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        let rows = painter
            .layout(cand, font.clone(), Color32::WHITE, max_width)
            .rows
            .len();
        if rows <= 2 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().chain(std::iter::once(&'…')).collect()
    }
}

fn toggle_collapse(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let new_collapsed = !spec.collapsed;
    let ppp = ctx.pixels_per_point().max(0.5);
    let target_h = if new_collapsed {
        COLLAPSED_HEIGHT
    } else {
        (spec.rect.height().max(MIN_FENCE_HEIGHT) as f32) / ppp
    };
    let target_w = (spec.rect.width().max(MIN_FENCE_WIDTH) as f32) / ppp;
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(target_w, target_h)));
    ctx.request_repaint();
    let mut guard = shared.lock().unwrap();
    guard
        .pending_collapse_until
        .insert(fence_id, Instant::now() + Duration::from_millis(450));
    if let Some(f) = guard.config.fence_mut(fence_id) {
        f.collapsed = new_collapsed;
        guard.dirty_layout = true;
        out.needs_immediate_save = true;
    }
}

enum SortMode {
    Name,
    Kind,
    Modified,
}

fn sort_fence_items(
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    out: &mut FenceUiOutput,
    mode: SortMode,
) {
    let mut guard = shared.lock().unwrap();
    let old_items = guard
        .config
        .fence_mut(fence_id)
        .filter(|f| f.portal_dir.is_none())
        .map(|f| f.items.clone());
    let mut changed = false;
    if let Some(f) = guard.config.fence_mut(fence_id)
        && f.portal_dir.is_none()
    {
        match mode {
            SortMode::Name => {
                f.items.sort_by(|a, b| {
                    let a_dir = matches!(a.kind, desklite_core::ItemKind::Folder);
                    let b_dir = matches!(b.kind, desklite_core::ItemKind::Folder);
                    b_dir.cmp(&a_dir).then_with(|| {
                        a.display_name
                            .to_ascii_lowercase()
                            .cmp(&b.display_name.to_ascii_lowercase())
                    })
                });
            }
            SortMode::Kind => {
                f.items.sort_by(|a, b| {
                    let ext_a = Path::new(&a.path)
                        .extension()
                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default();
                    let ext_b = Path::new(&b.path)
                        .extension()
                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default();
                    ext_a.cmp(&ext_b).then_with(|| {
                        a.display_name
                            .to_ascii_lowercase()
                            .cmp(&b.display_name.to_ascii_lowercase())
                    })
                });
            }
            SortMode::Modified => {
                f.items.sort_by(|a, b| {
                    let mtime = |p: &str| {
                        std::fs::metadata(p)
                            .and_then(|m| m.modified())
                            .ok()
                    };
                    mtime(&b.path).cmp(&mtime(&a.path))
                });
            }
        }
        for (i, it) in f.items.iter_mut().enumerate() {
            it.order = i as i32;
        }
        changed = old_items.as_ref().is_some_and(|prev| prev != &f.items);
        guard.selected_items.remove(&fence_id);
        guard.selection_anchor.remove(&fence_id);
        guard.dirty_layout = true;
        out.needs_immediate_save = true;
    }
    if changed && let Some(prev) = old_items {
        guard.push_undo(UndoAction::ReorderItems {
            fence_id,
            old_items: prev,
        });
    }
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
    let mut guard = shared.lock().unwrap();
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
        portal_dir: None,
        backdrop_alpha: src.backdrop_alpha,
        layout_override: src.layout_override.clone(),
        items: Vec::new(),
    });
    let topo = guard.topology.clone();
    let _ = guard.config.apply_topology(&topo, (72, 72));
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
        let mut guard = shared.lock().unwrap();
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

fn request_repaint_all_fences(shared: &Arc<Mutex<SharedFenceState>>, ctx: &egui::Context) {
    let ids: Vec<i32> = shared
        .lock()
        .unwrap()
        .config
        .fences
        .iter()
        .map(|f| f.id)
        .collect();
    for fid in ids {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
}

fn compute_inspector_initial_pos(
    spec_rect: Rect,
    monitors: &[desklite_core::MonitorDescriptor],
) -> (i32, i32) {
    if monitors.is_empty() {
        return (spec_rect.right + 14, spec_rect.top);
    }
    let mon = desklite_core::nearest_monitor(spec_rect.center(), monitors);
    let scale = (mon.dpi_x as f32 / 96.0).max(0.5);
    let insp_w = (INSPECTOR_WIDTH * scale).round() as i32;
    let insp_h = (INSPECTOR_HEIGHT * scale).round() as i32;
    let work = mon.work_area;
    let gap = (14.0 * scale).round() as i32;
    let x = if spec_rect.right + gap + insp_w <= work.right {
        spec_rect.right + gap
    } else if spec_rect.left - gap - insp_w >= work.left {
        spec_rect.left - gap - insp_w
    } else {
        (work.right - insp_w).max(work.left)
    };
    let y = spec_rect.top.clamp(work.top, (work.bottom - insp_h).max(work.top));
    (x, y)
}

fn open_param_editor_for_fence(
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    let (insp_hwnd, initial_pos, needs_uncollapse) = {
        let mut guard = shared.lock().unwrap();
        let Some(spec) = guard.config.fences.iter().find(|f| f.id == fence_id).cloned() else {
            return;
        };
        let initial_pos = compute_inspector_initial_pos(spec.rect, guard.topology.monitors());
        let existing_hwnd = guard.inspector.as_ref().map(|i| i.hwnd).unwrap_or(0);
        let styled = guard.inspector.as_ref().is_some_and(|i| i.styled);
        guard.inspector = Some(InspectorState {
            open: true,
            target_fence_id: fence_id,
            initial_pos,
            hwnd: existing_hwnd,
            styled,
            drag_grab: None,
            draft_name: Some((fence_id, spec.name.clone())),
        });
        guard.inspector_spawned = true;
        let uncollapse = spec.collapsed;
        if uncollapse {
            guard.pending_toggle_collapse.insert(fence_id);
        }
        (existing_hwnd, initial_pos, uncollapse)
    };
    let insp_vid = ViewportId::from_hash_of("desklite_inspector");
    if insp_hwnd != 0 {
        ctx.send_viewport_cmd_to(insp_vid, ViewportCommand::Visible(true));
        desklite_win32::move_window_no_z(insp_hwnd, initial_pos.0, initial_pos.1);
        focus_window_now(insp_hwnd);
    }
    if needs_uncollapse {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fence_id)));
    }
    ctx.request_repaint_of(ViewportId::ROOT);
    ctx.request_repaint_of(insp_vid);
}

fn inspector_card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let card_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 9))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 16),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10));
    card_frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(title)
                    .size(11.5)
                    .strong()
                    .color(Color32::from_rgb(154, 170, 198)),
            );
            ui.add_space(6.0);
            add_contents(ui)
        })
        .inner
}

fn labeled_row(ui: &mut Ui, label: &str, add_control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(72.0, 22.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .size(12.0)
                        .color(Color32::from_rgb(218, 224, 236)),
                );
            },
        );
        ui.with_layout(Layout::left_to_right(Align::Center), add_control);
    });
}

fn px_slider(ui: &mut Ui, val: &mut i32, range: std::ops::RangeInclusive<i32>) -> bool {
    let w = (ui.available_width() - 70.0).max(72.0);
    ui.spacing_mut().slider_width = w;
    ui.add(egui::Slider::new(val, range).suffix(" px")).changed()
}

fn close_inspector_safely(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let mut guard = shared.lock().unwrap();
    let draft_to_commit = if let Some(ref mut ins) = guard.inspector {
        ins.open = false;
        ins.drag_grab = None;
        ins.draft_name.take()
    } else {
        None
    };
    if let Some((fid, draft)) = draft_to_commit {
        let trimmed = draft.trim().to_string();
        if !trimmed.is_empty()
            && let Some(f) = guard.config.fence_mut(fid)
        {
            f.name = trimmed;
        }
    }
    guard.dirty_layout = true;
    out.needs_immediate_save = true;
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    ctx.request_repaint_of(ViewportId::ROOT);
}

/// Floating live-preview settings window positioned beside the target fence. Uses `Visible(false)`
/// on close rather than viewport destruction so `eframe` never tears down an active WGL surface.
pub fn render_inspector_viewport(
    ui: &mut Ui,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();

    if ctx.input(|i| i.viewport().close_requested()) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        close_inspector_safely(shared, &ctx, &mut out);
        return out;
    }

    let (insp_opt, all_fences, monitors, prefs_snap) = {
        let mut guard = shared.lock().unwrap();
        if let Some(ref mut ins) = guard.inspector {
            if ins.hwnd == 0
                && let Some(h) = find_window_by_exact_title(INSPECTOR_TITLE)
            {
                ins.hwnd = h;
            }
            if ins.hwnd != 0 && !ins.styled {
                style_floating_inspector_window(ins.hwnd);
                ins.styled = true;
            }
        }
        (
            guard.inspector.clone(),
            guard
                .config
                .fences
                .iter()
                .map(|f| (f.id, format_fence_title(f)))
                .collect::<Vec<_>>(),
            guard.topology.monitors().to_vec(),
            guard.config.preferences.clone(),
        )
    };

    let Some(insp) = insp_opt else {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        return out;
    };
    if !insp.open {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        return out;
    }

    let target_id = insp.target_fence_id;
    let (target_spec_opt, target_hwnd) = {
        let guard = shared.lock().unwrap();
        (
            guard.config.fences.iter().find(|f| f.id == target_id).cloned(),
            guard.attached_hwnds.get(&target_id).copied().unwrap_or(0),
        )
    };
    let Some(spec) = target_spec_opt else {
        close_inspector_safely(shared, &ctx, &mut out);
        return out;
    };

    if let Some((gx, gy)) = insp.drag_grab {
        if desklite_win32::is_primary_mouse_down() {
            if let Some((cx, cy)) = desklite_win32::cursor_screen_pos()
                && let Some((wl, wt, wr, wb)) = desklite_win32::window_outer_rect(insp.hwnd)
            {
                let (nx, ny) = clamp_drag_pos_to_monitors(
                    cx - gx,
                    cy - gy,
                    (wr - wl).max(200),
                    (wb - wt).max(200),
                    (cx, cy),
                    &monitors,
                );
                desklite_win32::move_window_no_z(insp.hwnd, nx, ny);
            }
            ctx.request_repaint();
        } else if let Some(ref mut ins) = shared.lock().unwrap().inspector {
            ins.drag_grab = None;
        }
    }

    let live_rect = if let Some((l, t, r, b)) = live_sizemove_rect_for_hwnd(target_hwnd) {
        ctx.request_repaint();
        if spec.collapsed {
            Rect::new(l, t, r, t + spec.rect.height().max(MIN_FENCE_HEIGHT))
        } else {
            Rect::new(l, t, r, b)
        }
    } else if target_hwnd != 0
        && desklite_win32::is_in_size_move()
        && let Some((l, t, r, b)) = desklite_win32::window_outer_rect(target_hwnd)
    {
        ctx.request_repaint();
        if spec.collapsed {
            Rect::new(l, t, r, t + spec.rect.height().max(MIN_FENCE_HEIGHT))
        } else {
            Rect::new(l, t, r, b)
        }
    } else {
        spec.rect
    };

    let mut close_inspector =
        ctx.input(|i| i.key_pressed(Key::Escape)) && !ctx.egui_wants_keyboard_input();

    let panel_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(24, 27, 34, 242))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(0));

    egui::CentralPanel::default().frame(panel_frame).show(ui, |ui| {
        // ---- Custom draggable header ----
        let header_frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(32, 36, 46, 225))
            .corner_radius(CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            })
            .inner_margin(Margin {
                left: 12,
                right: 10,
                top: 7,
                bottom: 7,
            });
        header_frame.show(ui, |ui| {
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if draw_header_icon_btn(ui, "关闭设置 (Esc)", |painter, r, fg| {
                    let c = r.center();
                    let s = Stroke::new(1.5, fg);
                    painter.line_segment(
                        [Pos2::new(c.x - 4.0, c.y - 4.0), Pos2::new(c.x + 4.0, c.y + 4.0)],
                        s,
                    );
                    painter.line_segment(
                        [Pos2::new(c.x + 4.0, c.y - 4.0), Pos2::new(c.x - 4.0, c.y + 4.0)],
                        s,
                    );
                })
                .clicked()
                {
                    close_inspector = true;
                }

                if all_fences.len() > 1 {
                    let cur_label = format_fence_title(&spec);
                    let mut picked = target_id;
                    egui::ComboBox::from_id_salt("insp_target_fence")
                        .width(156.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (fid, fname) in &all_fences {
                                ui.selectable_value(&mut picked, *fid, fname);
                            }
                        });
                    if picked != target_id {
                        let mut guard = shared.lock().unwrap();
                        let next_name = guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == picked)
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        if let Some(ref mut ins) = guard.inspector {
                            ins.target_fence_id = picked;
                            ins.draft_name = Some((picked, next_name));
                        }
                        if guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == picked)
                            .is_some_and(|f| f.collapsed)
                        {
                            guard.pending_toggle_collapse.insert(picked);
                            ctx.request_repaint_of(ViewportId::from_hash_of((
                                "desklite_fence",
                                picked,
                            )));
                        }
                    }
                }

                let rem_w = ui.available_width().max(40.0);
                let (h_rect, _) = ui.allocate_exact_size(Vec2::new(rem_w, 24.0), Sense::hover());
                let drag_resp = ui.interact(
                    h_rect,
                    ui.make_persistent_id("insp_header_drag"),
                    Sense::click_and_drag(),
                );
                ui.painter().text(
                    Pos2::new(h_rect.min.x + 2.0, h_rect.center().y - 0.5),
                    egui::Align2::LEFT_CENTER,
                    "参数设置",
                    egui::FontId::proportional(13.5),
                    Color32::from_rgb(242, 246, 255),
                );
                if drag_resp.drag_started_by(PointerButton::Primary)
                    && let Some((cx, cy)) = desklite_win32::cursor_screen_pos()
                    && let Some((wl, wt, _, _)) = desklite_win32::window_outer_rect(insp.hwnd)
                    && let Some(ref mut ins) = shared.lock().unwrap().inspector
                {
                    ins.drag_grab = Some((cx - wl, cy - wt));
                    ctx.request_repaint();
                }
            });
        });

        let body_h = (ui.available_height() - 46.0).max(120.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), body_h),
            Layout::top_down(Align::Min),
            |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::new().inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;

                            // ---- Card 1: Current Fence Appearance & Geometry ----
                            let mut name_buf = insp
                                .draft_name
                                .as_ref()
                                .filter(|(fid, _)| *fid == target_id)
                                .map(|(_, s)| s.clone())
                                .unwrap_or_else(|| spec.name.clone());
                            let mut next_kind = spec.kind;
                            let mut alpha = spec.backdrop_alpha.clamp(40, 245);
                            let mut locked = spec.locked;
                            let mut rx = live_rect.left;
                            let mut ry = live_rect.top;
                            let mut rw = live_rect.width().max(MIN_FENCE_WIDTH);
                            let mut rh = live_rect.height().max(MIN_FENCE_HEIGHT);

                            let mut name_changed = false;
                            let mut name_lost_focus = false;
                            let mut kind_changed = false;
                            let mut alpha_changed = false;
                            let mut lock_changed = false;
                            let mut rect_changed = false;

                            inspector_card(ui, "当前格子外观与位置", |ui| {
                                ui.spacing_mut().item_spacing.y = 7.0;
                                labeled_row(ui, "名称", |ui| {
                                    let w = ui.available_width().max(120.0);
                                    let name_resp = ui.add(
                                        egui::TextEdit::singleline(&mut name_buf).desired_width(w),
                                    );
                                    if name_resp.changed() {
                                        name_changed = true;
                                    }
                                    if name_resp.lost_focus() {
                                        name_lost_focus = true;
                                    }
                                });

                                labeled_row(ui, "视图模式", |ui| {
                                    ui.columns(2, |cols| {
                                        let is_list = matches!(next_kind, FenceKind::FileList);
                                        if cols[0]
                                            .add_sized(
                                                [cols[0].available_width(), 24.0],
                                                egui::Button::selectable(!is_list, "图标网格"),
                                            )
                                            .clicked()
                                            && is_list
                                        {
                                            next_kind = FenceKind::IconGrid;
                                            kind_changed = true;
                                        }
                                        if cols[1]
                                            .add_sized(
                                                [cols[1].available_width(), 24.0],
                                                egui::Button::selectable(is_list, "紧凑列表"),
                                            )
                                            .clicked()
                                            && !is_list
                                        {
                                            next_kind = FenceKind::FileList;
                                            kind_changed = true;
                                        }
                                    });
                                });

                                labeled_row(ui, "背景不透明", |ui| {
                                    let pct = ((alpha as f32 / 255.0) * 100.0).round() as i32;
                                    ui.spacing_mut().slider_width =
                                        (ui.available_width() - 56.0).max(80.0);
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut alpha, 40..=245)
                                                .show_value(false),
                                        )
                                        .changed()
                                    {
                                        alpha_changed = true;
                                    }
                                    ui.label(
                                        egui::RichText::new(format!("{pct}%"))
                                            .size(11.5)
                                            .color(Color32::from_rgb(176, 192, 218)),
                                    );
                                });

                                labeled_row(ui, "位置 / 尺寸", |ui| {
                                    ui.add_enabled_ui(!locked, |ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut rx)
                                                    .speed(2.0)
                                                    .prefix("X "),
                                            )
                                            .changed()
                                        {
                                            rect_changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut ry)
                                                    .speed(2.0)
                                                    .prefix("Y "),
                                            )
                                            .changed()
                                        {
                                            rect_changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut rw)
                                                    .range(MIN_FENCE_WIDTH..=3840)
                                                    .speed(2.0)
                                                    .prefix("宽 "),
                                            )
                                            .changed()
                                        {
                                            rect_changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut rh)
                                                    .range(MIN_FENCE_HEIGHT..=2160)
                                                    .speed(2.0)
                                                    .prefix("高 "),
                                            )
                                            .changed()
                                        {
                                            rect_changed = true;
                                        }
                                    });
                                });

                                labeled_row(ui, "锁定防误触", |ui| {
                                    if ui
                                        .checkbox(&mut locked, "锁定位置与尺寸")
                                        .changed()
                                    {
                                        lock_changed = true;
                                    }
                                });
                            });

                            // ---- Card 2: Independent Per-Fence Layout (auto-switches with view mode) ----
                            let is_list_mode = matches!(next_kind, FenceKind::FileList);
                            let mut ov = spec.layout_override.clone().unwrap_or_default();
                            let mut s_cell_w =
                                ov.icon_cell_w.unwrap_or(prefs_snap.icon_cell_w).clamp(64, 160);
                            let mut s_cell_h =
                                ov.icon_cell_h.unwrap_or(prefs_snap.icon_cell_h).clamp(72, 180);
                            let mut s_row_h =
                                ov.list_row_h.unwrap_or(prefs_snap.list_row_h).clamp(24, 56);
                            let mut s_list_icon =
                                ov.list_icon_px.unwrap_or(prefs_snap.list_icon_px).clamp(14, 36);
                            let mut single_changed = false;
                            let mut single_reset = false;

                            let has_mode_override = if is_list_mode {
                                ov.list_row_h.is_some() || ov.list_icon_px.is_some()
                            } else {
                                ov.icon_cell_w.is_some() || ov.icon_cell_h.is_some()
                            };
                            let single_title = if is_list_mode {
                                "单格排版（当前格子 · 紧凑列表）"
                            } else {
                                "单格排版（当前格子 · 图标网格）"
                            };
                            inspector_card(ui, single_title, |ui| {
                                ui.spacing_mut().item_spacing.y = 7.0;
                                if is_list_mode {
                                    labeled_row(ui, "列表行高", |ui| {
                                        if px_slider(ui, &mut s_row_h, 24..=56) {
                                            ov.list_row_h = Some(s_row_h);
                                            single_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "列表图标", |ui| {
                                        if px_slider(ui, &mut s_list_icon, 14..=36) {
                                            ov.list_icon_px = Some(s_list_icon);
                                            single_changed = true;
                                        }
                                    });
                                } else {
                                    labeled_row(ui, "网格格宽", |ui| {
                                        if px_slider(ui, &mut s_cell_w, 64..=160) {
                                            ov.icon_cell_w = Some(s_cell_w);
                                            single_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "网格格高", |ui| {
                                        if px_slider(ui, &mut s_cell_h, 72..=180) {
                                            ov.icon_cell_h = Some(s_cell_h);
                                            single_changed = true;
                                        }
                                    });
                                }
                                ui.horizontal(|ui| {
                                    let status = if has_mode_override {
                                        "当前已使用单格独立参数（不影响其它格子）"
                                    } else {
                                        "当前跟随全局排版"
                                    };
                                    ui.label(
                                        egui::RichText::new(status)
                                            .size(11.0)
                                            .color(Color32::from_rgb(142, 156, 180)),
                                    );
                                    if has_mode_override {
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui.small_button("恢复跟随全局").clicked() {
                                                single_reset = true;
                                            }
                                        });
                                    }
                                });
                            });

                            // ---- Card 3: Global Layout Settings ----
                            let mut g_cell_w = prefs_snap.icon_cell_w.clamp(64, 160);
                            let mut g_cell_h = prefs_snap.icon_cell_h.clamp(72, 180);
                            let mut g_row_h = prefs_snap.list_row_h.clamp(24, 56);
                            let mut g_list_icon = prefs_snap.list_icon_px.clamp(14, 36);
                            let mut snap_px = prefs_snap.snap_threshold_px.clamp(0, 40);
                            let mut prefs_changed = false;

                            inspector_card(ui, "全局默认排版（未独立设置的格子同步）", |ui| {
                                ui.spacing_mut().item_spacing.y = 7.0;
                                labeled_row(ui, "网格格宽", |ui| {
                                    if px_slider(ui, &mut g_cell_w, 64..=160) {
                                        prefs_changed = true;
                                    }
                                });
                                labeled_row(ui, "网格格高", |ui| {
                                    if px_slider(ui, &mut g_cell_h, 72..=180) {
                                        prefs_changed = true;
                                    }
                                });
                                labeled_row(ui, "列表行高", |ui| {
                                    if px_slider(ui, &mut g_row_h, 24..=56) {
                                        prefs_changed = true;
                                    }
                                });
                                labeled_row(ui, "列表图标", |ui| {
                                    if px_slider(ui, &mut g_list_icon, 14..=36) {
                                        prefs_changed = true;
                                    }
                                });
                                labeled_row(ui, "边缘吸附", |ui| {
                                    if px_slider(ui, &mut snap_px, 0..=40) {
                                        prefs_changed = true;
                                    }
                                });
                            });

                            if single_reset {
                                if is_list_mode {
                                    ov.list_row_h = None;
                                    ov.list_icon_px = None;
                                } else {
                                    ov.icon_cell_w = None;
                                    ov.icon_cell_h = None;
                                }
                                single_changed = true;
                            }

                            if name_changed
                                || name_lost_focus
                                || kind_changed
                                || alpha_changed
                                || lock_changed
                                || single_changed
                                || prefs_changed
                            {
                                let mut guard = shared.lock().unwrap();
                                if let Some(ref mut ins) = guard.inspector {
                                    if name_lost_focus {
                                        let trimmed = name_buf.trim().to_string();
                                        ins.draft_name = Some((target_id, trimmed.clone()));
                                        name_buf = trimmed;
                                    } else if name_changed {
                                        ins.draft_name = Some((target_id, name_buf.clone()));
                                    }
                                }
                                if let Some(f) = guard.config.fence_mut(target_id) {
                                    if name_changed || name_lost_focus {
                                        let valid = if name_lost_focus {
                                            name_buf.trim().to_string()
                                        } else {
                                            name_buf.clone()
                                        };
                                        if !valid.trim().is_empty() {
                                            f.name = valid;
                                        }
                                    }
                                    if kind_changed {
                                        f.kind = next_kind;
                                    }
                                    if alpha_changed {
                                        f.backdrop_alpha = alpha;
                                    }
                                    if lock_changed {
                                        f.locked = locked;
                                        if target_hwnd != 0 {
                                            set_hwnd_locked(target_hwnd, locked);
                                        }
                                    }
                                    if single_changed {
                                        f.layout_override = if ov.is_empty() { None } else { Some(ov) };
                                    }
                                }
                                if prefs_changed {
                                    guard.config.preferences.icon_cell_w = g_cell_w;
                                    guard.config.preferences.icon_cell_h = g_cell_h;
                                    guard.config.preferences.list_row_h = g_row_h;
                                    guard.config.preferences.list_icon_px = g_list_icon;
                                    guard.config.preferences.snap_threshold_px = snap_px;
                                }
                                guard.mark_debounced_save(&ctx);
                            }
                            if prefs_changed {
                                request_repaint_all_fences(shared, &ctx);
                            } else if name_changed
                                || name_lost_focus
                                || kind_changed
                                || alpha_changed
                                || lock_changed
                                || single_changed
                            {
                                ctx.request_repaint_of(ViewportId::from_hash_of((
                                    "desklite_fence",
                                    target_id,
                                )));
                            }
                            if rect_changed && !locked {
                                let _ = apply_param_editor_rect(
                                    target_id,
                                    target_hwnd,
                                    rx,
                                    ry,
                                    rw,
                                    rh,
                                    shared,
                                    &ctx,
                                    &mut out,
                                );
                            }
                        });
                    });
            },
        );

        // ---- Footer bar ----
        Frame::new()
            .fill(Color32::from_rgba_unmultiplied(28, 32, 42, 235))
            .corner_radius(CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            })
            .inner_margin(Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("复制同样式格子").clicked()
                        && duplicate_fence_with_style(target_id, shared, &ctx).is_some()
                    {
                        shared.lock().unwrap().toast_msg.insert(
                            target_id,
                            ("已复制样式格子".to_string(), Instant::now()),
                        );
                        out.needs_immediate_save = true;
                        ctx.request_repaint_of(ViewportId::from_hash_of((
                            "desklite_fence",
                            target_id,
                        )));
                        ctx.request_repaint_of(ViewportId::ROOT);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let done_btn = egui::Button::new(
                            egui::RichText::new("完成")
                                .size(12.5)
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(56, 130, 246))
                        .min_size(Vec2::new(72.0, 26.0));
                        if ui.add(done_btn).clicked() {
                            close_inspector = true;
                        }
                    });
                });
            });
    });

    if close_inspector {
        close_inspector_safely(shared, &ctx, &mut out);
    }

    out
}

fn open_fence_menu(
    fence_id: i32,
    my_hwnd: isize,
    btn_rect_bottom: Option<(f32, f32, f32)>,
    include_body_actions: bool,
    monitors: &[desklite_core::MonitorDescriptor],
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    let (raw_x, raw_y) = if let Some((bx, by, ppp)) = btn_rect_bottom
        && let Some((wl, wt, _, _)) = desklite_win32::window_outer_rect(my_hwnd)
    {
        (wl + (bx * ppp).round() as i32, wt + (by * ppp).round() as i32 + 4)
    } else if let Some((cx, cy)) = desklite_win32::cursor_screen_pos() {
        (cx, cy)
    } else {
        (120, 120)
    };
    let (pos_x, pos_y) = if monitors.is_empty() {
        (raw_x, raw_y)
    } else {
        let mon = desklite_core::nearest_monitor(
            desklite_core::Point { x: raw_x, y: raw_y },
            monitors,
        );
        let w = mon.work_area;
        (
            raw_x.clamp(w.left, (w.right - 20).max(w.left)),
            raw_y.clamp(w.top, (w.bottom - 20).max(w.top)),
        )
    };
    allow_foreground_self();
    shared.lock().unwrap().push_command(
        ctx,
        FenceCommand::ShowFenceMenu {
            fence_id,
            screen_x: pos_x,
            screen_y: pos_y,
            include_body_actions,
        },
    );
}

pub fn apply_collapsed_menu_command(
    fence_id: i32,
    cmd: CollapsedMenuCommand,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> bool {
    let mut out = FenceUiOutput::default();
    let fence_vid = ViewportId::from_hash_of(("desklite_fence", fence_id));
    match cmd {
        CollapsedMenuCommand::ToggleCollapse => {
            shared.lock().unwrap().pending_toggle_collapse.insert(fence_id);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::RenameFence => {
            let mut guard = shared.lock().unwrap();
            if let Some(name) = guard.config.fences.iter().find(|f| f.id == fence_id).map(|f| f.name.clone()) {
                guard.rename_prompt = Some(RenamePrompt {
                    fence_id,
                    draft: name,
                    needs_focus: true,
                });
                ctx.request_repaint_of(fence_vid);
            }
        }
        CollapsedMenuCommand::ToggleKind => {
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.kind = match f.kind {
                    FenceKind::FileList => FenceKind::IconGrid,
                    _ => FenceKind::FileList,
                };
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::ToggleLock => {
            let mut guard = shared.lock().unwrap();
            let my_hwnd = guard.attached_hwnds.get(&fence_id).copied().unwrap_or(0);
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.locked = !f.locked;
                let locked = f.locked;
                guard.toast_msg.insert(
                    fence_id,
                    (
                        if locked { "已锁定格子" } else { "已解锁格子" }.to_string(),
                        Instant::now(),
                    ),
                );
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
                if my_hwnd != 0 {
                    set_hwnd_locked(my_hwnd, locked);
                }
            }
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::DuplicateStyle => {
            if duplicate_fence_with_style(fence_id, shared, ctx).is_some() {
                shared.lock().unwrap().toast_msg.insert(
                    fence_id,
                    ("已复制样式格子".to_string(), Instant::now()),
                );
                out.needs_immediate_save = true;
            }
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::OpenParamsEditor => {
            open_param_editor_for_fence(fence_id, shared, ctx);
        }
        CollapsedMenuCommand::OpenPortalFolder => {
            let dir = shared
                .lock()
                .unwrap()
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .and_then(|f| f.portal_dir.clone());
            if let Some(dir) = dir {
                let _ = launch_item(&dir);
            }
        }
        CollapsedMenuCommand::UnbindPortal => {
            let (old_paths, remaining) = {
                let mut guard = shared.lock().unwrap();
                let old_paths: Vec<String> = guard
                    .config
                    .fences
                    .iter()
                    .find(|f| f.id == fence_id)
                    .map(|f| f.items.iter().map(|it| it.path.clone()).collect())
                    .unwrap_or_default();
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.portal_dir = None;
                    f.items.clear();
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                    guard.marquee_drag.remove(&fence_id);
                    guard.dirty_layout = true;
                    out.needs_immediate_save = true;
                }
                guard.push_command(ctx, FenceCommand::SyncPortals);
                (old_paths, collect_managed_desktop_paths(&guard.config))
            };
            let to_restore: Vec<String> = old_paths
                .into_iter()
                .filter(|op| !remaining.iter().any(|r| r.eq_ignore_ascii_case(op)))
                .collect();
            restore_desktop_icons_async(to_restore);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::BindPortal => {
            shared
                .lock()
                .unwrap()
                .push_command(ctx, FenceCommand::RequestBindPortal(fence_id));
        }
        CollapsedMenuCommand::SortName => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Name);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::SortKind => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Kind);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::SortModified => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Modified);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::PasteFromClipboard => {
            let spec_opt = shared
                .lock()
                .unwrap()
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .cloned();
            if let Some(spec) = spec_opt {
                handle_paste_action(fence_id, &spec, shared, ctx, &mut out);
                ctx.request_repaint_of(fence_vid);
            }
        }
        CollapsedMenuCommand::UndoLast => {
            execute_undo(fence_id, shared, ctx, &mut out);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::SelectAll => {
            let mut guard = shared.lock().unwrap();
            let count = guard
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| f.items.len())
                .unwrap_or(0);
            if count > 0 {
                guard.selected_items.insert(fence_id, (0..count).collect());
                guard.selection_anchor.insert(fence_id, 0);
            }
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::RefreshPortal => {
            let managed = {
                let mut guard = shared.lock().unwrap();
                if let Some(f) = guard.config.fence_mut(fence_id)
                    && let Some(ref dir) = f.portal_dir
                {
                    f.items = scan_portal_directory(Path::new(dir));
                }
                collect_managed_desktop_paths(&guard.config)
            };
            hide_desktop_icons_for_paths_async(managed);
            ctx.request_repaint_of(fence_vid);
        }
        CollapsedMenuCommand::CreateBlank => {
            shared
                .lock()
                .unwrap()
                .push_command(ctx, FenceCommand::RequestCreateBlank);
        }
        CollapsedMenuCommand::CreatePortal => {
            shared
                .lock()
                .unwrap()
                .push_command(ctx, FenceCommand::RequestCreatePortal);
        }
        CollapsedMenuCommand::DeleteFence => {
            shared
                .lock()
                .unwrap()
                .push_command(ctx, FenceCommand::RequestDelete(fence_id));
        }
        CollapsedMenuCommand::ExitApp => {
            shared
                .lock()
                .unwrap()
                .push_command(ctx, FenceCommand::RequestExitApp);
        }
    }
    out.needs_immediate_save
}

fn execute_undo(
    caller_fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let action_opt = { shared.lock().unwrap().undo_stack.pop() };
    let Some(action) = action_opt else {
        shared.lock().unwrap().toast_msg.insert(
            caller_fence_id,
            ("无可撤销操作".to_string(), Instant::now()),
        );
        ctx.request_repaint();
        return;
    };

    let mut notify_fences: HashSet<i32> = [caller_fence_id].into_iter().collect();
    let mut restored_desktop_candidates: Vec<String> = Vec::new();
    let label: &'static str;

    match action {
        UndoAction::RenameItem {
            fence_id,
            old_path,
            new_path,
        } => {
            notify_fences.insert(fence_id);
            label = "已撤销重命名";
            let old_name = Path::new(&old_path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !old_name.is_empty()
                && Path::new(&new_path).exists()
                && let Ok(updated) = rename_file_in_place(Path::new(&new_path), &old_name)
            {
                let mut guard = shared.lock().unwrap();
                guard.textures.remove(&new_path.to_ascii_lowercase());
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    if let Some(ref dir) = f.portal_dir {
                        f.items = scan_portal_directory(Path::new(dir));
                    } else if let Some(slot) = f
                        .items
                        .iter_mut()
                        .find(|it| it.path.eq_ignore_ascii_case(&new_path))
                    {
                        let ord = slot.order;
                        *slot = updated;
                        slot.order = ord;
                    }
                }
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                shared.lock().unwrap().toast_msg.insert(
                    caller_fence_id,
                    ("无法撤销重命名：文件不存在或重名".to_string(), Instant::now()),
                );
                ctx.request_repaint();
                return;
            }
        }
        UndoAction::RenameFence { fence_id, old_name } => {
            notify_fences.insert(fence_id);
            label = "已撤销格子重命名";
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.name = old_name;
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
        UndoAction::RemoveItems { fence_id, items } => {
            notify_fences.insert(fence_id);
            label = "已撤销移出";
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id)
                && f.portal_dir.is_none()
            {
                for it in items {
                    if !f
                        .items
                        .iter()
                        .any(|cur| cur.path.eq_ignore_ascii_case(&it.path))
                    {
                        let insert_at = (it.order.max(0) as usize).min(f.items.len());
                        f.items.insert(insert_at, it);
                    }
                }
                for (i, it) in f.items.iter_mut().enumerate() {
                    it.order = i as i32;
                }
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
        UndoAction::AddItems {
            fence_id,
            added_paths,
        } => {
            notify_fences.insert(fence_id);
            label = "已撤销移入";
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id)
                && f.portal_dir.is_none()
            {
                f.items.retain(|it| {
                    !added_paths
                        .iter()
                        .any(|ap| it.path.eq_ignore_ascii_case(ap))
                });
                for (i, it) in f.items.iter_mut().enumerate() {
                    it.order = i as i32;
                }
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
            restored_desktop_candidates.extend(added_paths);
        }
        UndoAction::PasteFiles {
            fence_id,
            created_paths,
            moved_pairs,
            removed_from_fences,
            added_paths,
            is_portal,
        } => {
            notify_fences.insert(fence_id);
            label = "已撤销粘贴";
            for (src_before, dst_after) in &moved_pairs {
                let _ = move_path_in_place(Path::new(dst_after), Path::new(src_before));
            }
            if !created_paths.is_empty() {
                let _ = trash_files(&created_paths);
            }
            let mut guard = shared.lock().unwrap();
            for (src_fid, items) in removed_from_fences {
                notify_fences.insert(src_fid);
                if let Some(sf) = guard.config.fence_mut(src_fid)
                    && sf.portal_dir.is_none()
                {
                    for it in items {
                        if !sf
                            .items
                            .iter()
                            .any(|cur| cur.path.eq_ignore_ascii_case(&it.path))
                        {
                            let insert_at = (it.order.max(0) as usize).min(sf.items.len());
                            sf.items.insert(insert_at, it);
                        }
                    }
                    for (i, it) in sf.items.iter_mut().enumerate() {
                        it.order = i as i32;
                    }
                }
            }
            if let Some(f) = guard.config.fence_mut(fence_id) {
                if is_portal {
                    if let Some(ref dir) = f.portal_dir {
                        f.items = scan_portal_directory(Path::new(dir));
                    }
                } else if f.portal_dir.is_none() {
                    f.items.retain(|it| {
                        !added_paths
                            .iter()
                            .any(|ap| it.path.eq_ignore_ascii_case(ap))
                    });
                    for (i, it) in f.items.iter_mut().enumerate() {
                        it.order = i as i32;
                    }
                }
            }
            guard.selected_items.remove(&fence_id);
            guard.selection_anchor.remove(&fence_id);
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            restored_desktop_candidates.extend(added_paths);
        }
        UndoAction::ReorderItems {
            fence_id,
            old_items,
        } => {
            notify_fences.insert(fence_id);
            label = "已撤销排序";
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id)
                && f.portal_dir.is_none()
            {
                f.items = old_items;
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
    }

    let managed = {
        let mut guard = shared.lock().unwrap();
        guard
            .toast_msg
            .insert(caller_fence_id, (label.to_string(), Instant::now()));
        collect_managed_desktop_paths(&guard.config)
    };
    let to_restore: Vec<String> = restored_desktop_candidates
        .into_iter()
        .filter(|rp| {
            is_directly_on_desktop(Path::new(rp))
                && Path::new(rp).exists()
                && !managed.iter().any(|m| m.eq_ignore_ascii_case(rp))
        })
        .collect();
    if !to_restore.is_empty() {
        restore_desktop_icons_async(to_restore);
    }
    hide_desktop_icons_for_paths_async(managed.clone());
    hide_desktop_icons_for_paths_delayed(managed, 220);
    for fid in notify_fences {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
    ctx.request_repaint_of(ViewportId::ROOT);
}

fn detect_resize_edge(rect: egui::Rect, pos: Pos2, collapsed: bool) -> Option<ResizeDirection> {
    let left = pos.x - rect.min.x < RESIZE_EDGE;
    let right = rect.max.x - pos.x < RESIZE_EDGE;
    let top = pos.y - rect.min.y < RESIZE_EDGE;
    let bottom = rect.max.y - pos.y < RESIZE_EDGE;
    if collapsed {
        return match (left, right) {
            (true, _) => Some(ResizeDirection::West),
            (_, true) => Some(ResizeDirection::East),
            _ => None,
        };
    }
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

fn clamp_drag_pos_to_monitors(
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    cursor: (i32, i32),
    monitors: &[desklite_core::MonitorDescriptor],
) -> (i32, i32) {
    if monitors.is_empty() {
        return (left, top);
    }
    let mon = desklite_core::nearest_monitor(
        desklite_core::Point {
            x: cursor.0,
            y: cursor.1,
        },
        monitors,
    );
    let work = mon.work_area;
    let max_left = (work.right - width).max(work.left);
    let max_top = (work.bottom - height).max(work.top);
    (left.clamp(work.left, max_left), top.clamp(work.top, max_top))
}

fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desklite_core::{DisplayTopology, ItemKind, MonitorDescriptor};

    fn test_shared(fences: Vec<FenceSpec>) -> Arc<Mutex<SharedFenceState>> {
        let cfg = NativeLayoutConfig {
            fences,
            ..Default::default()
        };
        let topo = DisplayTopology::try_from_monitors(vec![MonitorDescriptor {
            device_key: "M0".to_string(),
            friendly_name: "Primary".to_string(),
            rect: Rect::new(0, 0, 1920, 1080),
            work_area: Rect::new(0, 0, 1920, 1040),
            dpi_x: 96,
            dpi_y: 96,
            is_primary: true,
        }])
        .expect("topology init");
        let icon_cache = IconCache::new().expect("icon cache init");
        Arc::new(Mutex::new(SharedFenceState::new(cfg, topo, icon_cache)))
    }

    #[test]
    fn undo_restores_fence_rename_remove_reorder_and_file_rename() {
        let tmp_dir = std::env::temp_dir().join(format!("desklite_undo_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let old_file = tmp_dir.join("alpha.txt");
        std::fs::write(&old_file, "hello").unwrap();

        let item_a = FenceItem {
            display_name: "alpha".to_string(),
            order: 0,
            kind: ItemKind::File,
            path: old_file.to_string_lossy().into_owned(),
        };
        let item_b = FenceItem {
            display_name: "beta".to_string(),
            order: 1,
            kind: ItemKind::File,
            path: tmp_dir.join("beta.txt").to_string_lossy().into_owned(),
        };
        let fence = FenceSpec {
            id: 101,
            name: "OrigFence".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 420, 400),
            collapsed: false,
            locked: false,
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![item_a.clone(), item_b.clone()],
        };
        let shared = test_shared(vec![fence]);
        let ctx = egui::Context::default();
        let mut out = FenceUiOutput::default();

        // 1. Undo fence rename
        {
            let mut g = shared.lock().unwrap();
            g.config.fence_mut(101).unwrap().name = "NewFence".to_string();
            g.push_undo(UndoAction::RenameFence {
                fence_id: 101,
                old_name: "OrigFence".to_string(),
            });
        }
        execute_undo(101, &shared, &ctx, &mut out);
        assert_eq!(shared.lock().unwrap().config.fences[0].name, "OrigFence");

        // 2. Undo reorder / sort
        apply_reorder_items(101, &[1], 0, &shared, &mut out);
        assert_eq!(
            shared.lock().unwrap().config.fences[0].items[0].display_name,
            "beta"
        );
        execute_undo(101, &shared, &ctx, &mut out);
        assert_eq!(
            shared.lock().unwrap().config.fences[0].items[0].display_name,
            "alpha"
        );

        // 3. Undo file rename in place
        commit_item_rename(
            101,
            0,
            &old_file.to_string_lossy(),
            "renamed.txt",
            &shared,
            &ctx,
            &mut out,
        );
        assert!(tmp_dir.join("renamed.txt").exists());
        assert!(!old_file.exists());
        execute_undo(101, &shared, &ctx, &mut out);
        assert!(old_file.exists());
        assert!(!tmp_dir.join("renamed.txt").exists());

        // 4. Undo remove items from fence
        {
            let mut g = shared.lock().unwrap();
            g.selected_items.insert(101, [0usize].into_iter().collect());
        }
        let spec_snap = shared.lock().unwrap().config.fences[0].clone();
        execute_item_action(
            ItemUiAction::RemoveSelected,
            101,
            0,
            &spec_snap,
            &[0usize].into_iter().collect(),
            &shared,
            &ctx,
            &mut out,
        );
        assert_eq!(shared.lock().unwrap().config.fences[0].items.len(), 1);
        execute_undo(101, &shared, &ctx, &mut out);
        assert_eq!(shared.lock().unwrap().config.fences[0].items.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn collapsed_menu_command_queues_toggle_and_sort() {
        let fence = FenceSpec {
            id: 102,
            name: "Test".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(40, 40, 360, 340),
            collapsed: true,
            locked: false,
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![
                FenceItem {
                    display_name: "z.txt".to_string(),
                    order: 0,
                    kind: ItemKind::File,
                    path: "C:\\nonexistent\\z.txt".to_string(),
                },
                FenceItem {
                    display_name: "a.txt".to_string(),
                    order: 1,
                    kind: ItemKind::File,
                    path: "C:\\nonexistent\\a.txt".to_string(),
                },
            ],
        };
        let shared = test_shared(vec![fence]);
        let ctx = egui::Context::default();

        assert!(!apply_collapsed_menu_command(
            102,
            CollapsedMenuCommand::ToggleCollapse,
            &shared,
            &ctx
        ));
        assert!(shared.lock().unwrap().pending_toggle_collapse.contains(&102));

        assert!(apply_collapsed_menu_command(
            102,
            CollapsedMenuCommand::SortName,
            &shared,
            &ctx
        ));
        assert_eq!(
            shared.lock().unwrap().config.fences[0].items[0].display_name,
            "a.txt"
        );
    }

    #[test]
    fn blank_area_click_deselects_and_right_click_opens_menu() {
        let fence = FenceSpec {
            id: 105,
            name: "Audio Media".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(0, 0, 420, 420),
            collapsed: false,
            locked: false,
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![
                FenceItem {
                    display_name: "one.txt".to_string(),
                    order: 0,
                    kind: ItemKind::File,
                    path: "C:\\nonexistent\\one.txt".to_string(),
                },
                FenceItem {
                    display_name: "two.txt".to_string(),
                    order: 1,
                    kind: ItemKind::File,
                    path: "C:\\nonexistent\\two.txt".to_string(),
                },
            ],
        };
        let shared = test_shared(vec![fence]);
        shared
            .lock()
            .unwrap()
            .selected_items
            .insert(105, [0usize].into_iter().collect());
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 420.0));

        // Frame 1: warm up layout
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            ..Default::default()
        };
        let mut out1 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out1.textures_delta.clear();

        // Click blank area near bottom of fence (210, 320)
        let blank_pos = Pos2::new(210.0, 320.0);
        raw.events = vec![
            egui::Event::PointerMoved(blank_pos),
            egui::Event::PointerButton {
                pos: blank_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ];
        let mut out2 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out2.textures_delta.clear();
        raw.events = vec![egui::Event::PointerButton {
            pos: blank_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut out3 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out3.textures_delta.clear();

        let sel_after_left = shared.lock().unwrap().selected_items.get(&105).cloned();
        assert!(
            sel_after_left.as_ref().is_none_or(|s| s.is_empty()),
            "expected blank left click to clear selection, got {sel_after_left:?}"
        );

        // Right-click blank area (210, 320)
        raw.events = vec![
            egui::Event::PointerButton {
                pos: blank_pos,
                button: PointerButton::Secondary,
                pressed: true,
                modifiers: Default::default(),
            },
        ];
        let mut out4 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out4.textures_delta.clear();
        raw.events = vec![egui::Event::PointerButton {
            pos: blank_pos,
            button: PointerButton::Secondary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut out5 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out5.textures_delta.clear();
        let cmds_105 = std::mem::take(&mut shared.lock().unwrap().pending_commands);
        assert!(
            cmds_105.iter().any(|c| matches!(
                c,
                FenceCommand::ShowFenceMenu {
                    fence_id: 105,
                    include_body_actions: true,
                    ..
                }
            )),
            "expected blank right click to emit ShowFenceMenu(include_body_actions: true), got {cmds_105:?}"
        );

        // Test 12-item fence (457x506) after clicking item 0, then blank click & right-click
        let many_items: Vec<FenceItem> = (0..12)
            .map(|i| FenceItem {
                display_name: format!("item_{i}.txt"),
                order: i,
                kind: ItemKind::File,
                path: format!("C:\\nonexistent\\item_{i}.txt"),
            })
            .collect();
        let fence_many = FenceSpec {
            id: 106,
            name: "Audio Media".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(0, 0, 457, 506),
            collapsed: false,
            locked: false,
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: many_items,
        };
        let shared_many = test_shared(vec![fence_many]);
        let ctx2 = egui::Context::default();
        let screen2 = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(457.0, 506.0));
        let mut raw2 = egui::RawInput {
            screen_rect: Some(screen2),
            focused: true,
            ..Default::default()
        };
        for _ in 0..2 {
            let mut o = ctx2.run_ui(raw2.clone(), |ui| {
                let _ = render_fence_viewport(ui, 106, &shared_many);
            });
            o.textures_delta.clear();
        }
        let item_pos = Pos2::new(60.0, 90.0);
        raw2.events = vec![
            egui::Event::PointerMoved(item_pos),
            egui::Event::PointerButton {
                pos: item_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        raw2.events = vec![egui::Event::PointerButton {
            pos: item_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        assert_eq!(
            shared_many.lock().unwrap().selected_items.get(&106).map(|s| s.len()),
            Some(1),
            "item 0 should be selected"
        );

        let scrolled_blank = Pos2::new(210.0, 420.0);
        raw2.events = vec![
            egui::Event::PointerMoved(scrolled_blank),
            egui::Event::PointerButton {
                pos: scrolled_blank,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        raw2.events = vec![egui::Event::PointerButton {
            pos: scrolled_blank,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        let sel_after_scrolled_blank = shared_many.lock().unwrap().selected_items.get(&106).cloned();
        assert!(
            sel_after_scrolled_blank.as_ref().is_none_or(|s| s.is_empty()),
            "expected blank left click to clear selection, got {sel_after_scrolled_blank:?}"
        );

        // Blank right click on scrolled fence opens unified menu with body actions
        raw2.events = vec![egui::Event::PointerButton {
            pos: scrolled_blank,
            button: PointerButton::Secondary,
            pressed: true,
            modifiers: Default::default(),
        }];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        raw2.events = vec![egui::Event::PointerButton {
            pos: scrolled_blank,
            button: PointerButton::Secondary,
            pressed: false,
            modifiers: Default::default(),
        }];
        let mut o = ctx2.run_ui(raw2.clone(), |ui| {
            let _ = render_fence_viewport(ui, 106, &shared_many);
        });
        o.textures_delta.clear();
        let cmds_106 = std::mem::take(&mut shared_many.lock().unwrap().pending_commands);
        assert!(
            cmds_106.iter().any(|c| matches!(
                c,
                FenceCommand::ShowFenceMenu {
                    fence_id: 106,
                    include_body_actions: true,
                    ..
                }
            )),
            "expected blank right click on 12-item fence to open unified menu, got {cmds_106:?}"
        );
    }

    #[test]
    fn title_format_duplicate_lock_and_param_apply_work() {
        let portal = FenceSpec {
            id: 201,
            name: "Desktop".to_string(),
            kind: FenceKind::FileList,
            rect: Rect::new(50, 60, 410, 440),
            collapsed: true,
            locked: false,
            portal_dir: Some("C:\\Users\\Public\\Desktop".to_string()),
            backdrop_alpha: 195,
            layout_override: Some(desklite_core::FenceLayoutOverride {
                icon_cell_w: Some(128),
                icon_cell_h: Some(136),
                list_row_h: None,
                list_icon_px: None,
            }),
            items: vec![FenceItem {
                display_name: "a.txt".to_string(),
                order: 0,
                kind: ItemKind::File,
                path: "C:\\Users\\Public\\Desktop\\a.txt".to_string(),
            }],
        };
        assert_eq!(format_fence_title(&portal), "Desktop");
        let prefs = desklite_core::LayoutPreferences::default();
        assert_eq!(resolve_fence_layout(&portal, &prefs), (128.0, 136.0, 30.0, 18.0));

        let shared = test_shared(vec![portal]);
        let ctx = egui::Context::default();

        // ToggleLock
        assert!(apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::ToggleLock,
            &shared,
            &ctx
        ));
        assert!(shared.lock().unwrap().config.fences[0].locked);

        // DuplicateStyle clones style, layout_override & dimensions into an unlocked blank fence
        assert!(apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::DuplicateStyle,
            &shared,
            &ctx
        ));
        {
            let guard = shared.lock().unwrap();
            assert_eq!(guard.config.fences.len(), 2);
            let dup = &guard.config.fences[1];
            assert_eq!(dup.name, "Desktop 副本");
            assert!(matches!(dup.kind, FenceKind::FileList));
            assert_eq!(dup.backdrop_alpha, 195);
            assert_eq!(
                dup.layout_override.as_ref().and_then(|o| o.icon_cell_w),
                Some(128)
            );
            assert_eq!(dup.rect.width(), 360);
            assert_eq!(dup.rect.height(), 380);
            assert!(!dup.locked);
            assert!(dup.portal_dir.is_none());
            assert!(dup.items.is_empty());
        }

        // OpenParamsEditor opens floating inspector and auto-queues uncollapse when collapsed
        assert!(!apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::OpenParamsEditor,
            &shared,
            &ctx
        ));
        {
            let guard = shared.lock().unwrap();
            assert!(guard.inspector_spawned);
            assert!(guard.pending_toggle_collapse.contains(&201));
            assert_eq!(
                guard.inspector.as_ref().map(|i| (i.open, i.target_fence_id)),
                Some((true, 201))
            );
        }

        // Closing inspector hides it (`open = false`) without dropping the GL viewport state
        let mut close_out = FenceUiOutput::default();
        close_inspector_safely(&shared, &ctx, &mut close_out);
        assert!(close_out.needs_immediate_save);
        assert_eq!(
            shared.lock().unwrap().inspector.as_ref().map(|i| i.open),
            Some(false)
        );
        assert!(shared.lock().unwrap().inspector_spawned);

        // Apply custom rect via apply_param_editor_rect
        let mut out = FenceUiOutput::default();
        let applied = apply_param_editor_rect(201, 0, 120, 140, 480, 520, &shared, &ctx, &mut out);
        assert!(out.needs_immediate_save);
        assert_eq!(applied, Rect::new(120, 140, 600, 660));
        assert_eq!(shared.lock().unwrap().config.fences[0].rect, applied);
    }
}
