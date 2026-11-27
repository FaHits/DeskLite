use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    DesktopIconPos, FenceItem, FenceKind, FenceRole, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, Rect,
    clamp_rect_to_monitors, normalize_path_key,
};
use desklite_win32::{
    all_bucket_fence_hwnds, attach_desktop_owner_and_rounded_corners, find_window_by_exact_title,
    focus_window_now, foreground_window_handle, is_directly_on_desktop, is_file_drag_active,
    is_free_desktop_hwnd, live_sizemove_rect_for_hwnd, paste_files_to_directory_detailed,
    resolve_dropped_item, scan_portal_directory, set_active_text_edit_hwnd, set_hwnd_locked,
    take_win32_hotkeys,
};
use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Key, Layout, Margin, PointerButton, Pos2,
    ResizeDirection, ScrollArea, Sense, Stroke, Ui, Vec2, ViewportCommand, ViewportId,
};

use crate::actions::{
    dispatch_win32_hotkey, ease_out_cubic, execute_item_action, handle_keyboard_shortcuts,
    open_fence_menu, start_async_drag_from_paths, toggle_collapse,
};
use crate::item_grid::{
    ItemUiAction, apply_reorder_items, compute_reorder_target, draw_header_icon_btn,
    fit_single_line, handle_item_interactions, handle_text_edit_clipboard, paint_reorder_indicator,
    render_grid_item, render_grid_item_renaming, render_list_item, render_list_item_renaming,
};
use crate::snap::snap_move_rect;
use crate::state::{
    MarqueeState, PopupOwnerState, RESIZE_EDGE, TitleDragState, UndoAction, ViewModeAnimState,
    flush_pending_repaints, lock_unpoisoned, resolve_fence_layout,
};

// Re-export public API for backward compatibility with desklite-app and tests.
pub use crate::actions::apply_collapsed_menu_command;
pub use crate::free_desktop_view::{
    FreeDesktopDispatchResult, apply_free_desktop_menu_command, apply_free_icon_menu_command,
    build_free_desktop_snapshot, handle_free_desktop_event,
};
pub use crate::inspector::render_inspector_viewport;
pub use crate::state::{
    COLLAPSED_HEIGHT, CollapsedMenuCommand, FREE_DESKTOP_TITLE, FenceCommand, FenceUiOutput,
    FreeDesktopMenuCommand, FreeIconMenuCommand, INSPECTOR_HEIGHT, INSPECTOR_TITLE,
    INSPECTOR_WIDTH, InspectorState, RenamePrompt, SelectionScope, SharedFenceState,
    apply_param_editor_rect, collect_managed_desktop_paths,
    collect_managed_desktop_paths_for_dirs, configure_egui_theme, duplicate_fence_with_style,
    format_fence_title,
};

fn ensure_fence_attached(fence_id: i32, shared: &Arc<Mutex<SharedFenceState>>) {
    let already = {
        let guard = lock_unpoisoned(shared);
        guard.attached_hwnds.contains_key(&fence_id)
    };
    if already {
        return;
    }
    let Some(raw_hwnd) = find_window_by_exact_title(&format!("DeskLite#{fence_id}")) else {
        return;
    };
    if is_free_desktop_hwnd(raw_hwnd) {
        return;
    }
    {
        let guard = lock_unpoisoned(shared);
        if guard.attached_hwnds.values().any(|&h| h == raw_hwnd) {
            return;
        }
    }
    desklite_win32::set_window_cloaked(raw_hwnd, true);
    attach_desktop_owner_and_rounded_corners(raw_hwnd);
    if all_bucket_fence_hwnds().contains(&raw_hwnd) {
        let mut guard = lock_unpoisoned(shared);
        guard.register_fence_hwnd(fence_id, raw_hwnd);
        guard.pending_reveal_fences.insert(fence_id, 1);
    }
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

fn handle_fence_dropped_files(
    fence_id: i32,
    dropped_paths: Vec<String>,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    if dropped_paths.is_empty() {
        return;
    }
    let desktop_dirs = { lock_unpoisoned(shared).desktop_dirs.clone() };
    let target_role = {
        let guard = lock_unpoisoned(shared);
        let Some(f) = guard.config.fences.iter().find(|f| f.id == fence_id) else {
            return;
        };
        f.effective_role(&desktop_dirs)
    };

    let mut notify_fences: HashSet<i32> = [fence_id].into_iter().collect();
    match target_role {
        FenceRole::DesktopBucket => {
            let mut guard = lock_unpoisoned(shared);
            let source_transfer = guard.dragging_from_fence.take().filter(|ds| {
                ds.paths.iter().any(|sp| {
                    dropped_paths
                        .iter()
                        .any(|dp| normalize_path_key(dp) == normalize_path_key(sp))
                })
            });
            let mut prev_free_icons: Vec<DesktopIconPos> = Vec::new();
            for dp in &dropped_paths {
                let dkey = normalize_path_key(dp);
                if let Some(ic) = guard
                    .config
                    .active_free_icons()
                    .iter()
                    .find(|ic| normalize_path_key(&ic.id) == dkey)
                {
                    prev_free_icons.push(ic.clone());
                }
            }
            let items: Vec<FenceItem> = dropped_paths
                .iter()
                .enumerate()
                .map(|(i, raw)| resolve_dropped_item(Path::new(raw), i as i32))
                .collect();
            let added_paths: Vec<String> = items.iter().map(|it| it.path.clone()).collect();
            let src_bucket = source_transfer
                .as_ref()
                .and_then(|s| s.fence_id)
                .filter(|&sid| {
                    sid != fence_id
                        && guard
                            .config
                            .fences
                            .iter()
                            .any(|f| f.id == sid && f.is_desktop_bucket(&desktop_dirs))
                });
            if guard
                .config
                .assign_desktop_items_to_bucket(fence_id, items.clone(), &desktop_dirs)
            {
                if let Some(src_fid) = src_bucket {
                    guard.selected_items.remove(&src_fid);
                    guard.selection_anchor.remove(&src_fid);
                    notify_fences.insert(src_fid);
                    guard.push_undo(UndoAction::MoveBetweenBuckets {
                        src_fence_id: src_fid,
                        dst_fence_id: fence_id,
                        items,
                    });
                } else if !prev_free_icons.is_empty() {
                    for p in &added_paths {
                        guard.free_selected_paths.remove(&normalize_path_key(p));
                    }
                    guard.push_undo(UndoAction::MoveFreeIconsToBucket {
                        dst_fence_id: fence_id,
                        items,
                        prev_free_icons,
                    });
                } else if !added_paths.is_empty() {
                    guard.push_undo(UndoAction::AddItems {
                        fence_id,
                        added_paths,
                    });
                }
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
        FenceRole::UnfiledDesktop => {
            let mut guard = lock_unpoisoned(shared);
            let mut released_total = 0usize;
            let mut pending_undos = Vec::new();
            for f in &guard.config.fences {
                if f.is_desktop_bucket(&desktop_dirs) {
                    let rem: Vec<FenceItem> = f
                        .items
                        .iter()
                        .filter(|it| {
                            dropped_paths
                                .iter()
                                .any(|dp| normalize_path_key(&it.path) == normalize_path_key(dp))
                        })
                        .cloned()
                        .collect();
                    if !rem.is_empty() {
                        let fid = f.id;
                        notify_fences.insert(fid);
                        released_total += rem.len();
                        pending_undos.push(UndoAction::ReleaseBucketToFree {
                            src_fence_id: fid,
                            items: rem,
                        });
                    }
                }
            }
            for act in pending_undos {
                guard.push_undo(act);
            }
            for fid in notify_fences.iter().copied().collect::<Vec<_>>() {
                if fid != fence_id {
                    let _ = guard
                        .config
                        .release_desktop_items(fid, &dropped_paths, &desktop_dirs);
                    guard.selected_items.remove(&fid);
                    guard.selection_anchor.remove(&fid);
                }
            }
            if released_total > 0 {
                guard.place_released_icons_on_free_desktop(&dropped_paths, None);
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
        FenceRole::ExternalPortal { dir } => {
            let is_move = dropped_paths
                .iter()
                .all(|p| is_directly_on_desktop(Path::new(p)))
                && !desklite_win32::is_ctrl_key_down();
            if let Ok(pairs) =
                paste_files_to_directory_detailed(&dropped_paths, Path::new(&dir), is_move)
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
                let mut guard = lock_unpoisoned(shared);
                let mut removed_from_fences = Vec::new();
                if is_move {
                    for (s, _) in &pairs {
                        let ss = s.to_string_lossy().into_owned();
                        let _ = guard.config.remove_free_icon(&ss);
                        for f in &mut guard.config.fences {
                            if f.is_desktop_bucket(&desktop_dirs) {
                                let rem: Vec<FenceItem> = f
                                    .items
                                    .iter()
                                    .filter(|it| {
                                        normalize_path_key(&it.path) == normalize_path_key(&ss)
                                    })
                                    .cloned()
                                    .collect();
                                if !rem.is_empty() {
                                    notify_fences.insert(f.id);
                                    removed_from_fences.push((f.id, rem));
                                }
                            }
                        }
                        let _ = guard.config.remove_desktop_path_everywhere(&ss, &desktop_dirs);
                    }
                }
                if let Some(tf) = guard.config.fence_mut(fence_id) {
                    tf.items = scan_portal_directory(Path::new(&dir));
                }
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.push_undo(UndoAction::PasteFiles {
                    fence_id: Some(fence_id),
                    created_paths,
                    moved_pairs,
                    removed_from_fences,
                    added_paths: Vec::new(),
                    is_portal: true,
                });
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
        }
    }

    {
        let mut guard = lock_unpoisoned(shared);
        for fid in notify_fences {
            guard.request_repaint_fence(ctx, fid);
        }
        guard.mark_free_desktop_dirty(Some(ctx));
    }
    ctx.request_repaint_of(ViewportId::ROOT);
}

pub fn render_fence_viewport(
    ui: &mut Ui,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();
    ensure_fence_attached(fence_id, shared);

    {
        let mut guard = lock_unpoisoned(shared);
        if guard.poll_async_drag() {
            out.needs_immediate_save = true;
        }
        if std::mem::take(&mut guard.pending_immediate_save_from_async) {
            out.needs_immediate_save = true;
        }
    }
    flush_pending_repaints(shared, &ctx);

    let focused_now = ctx.input(|i| i.focused);
    let lost_focus_ev = ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::WindowFocused(false)))
    });
    let tracked_popup = { lock_unpoisoned(shared).popup_owners.get(&fence_id).copied() };
    if let Some(mut owner) = tracked_popup {
        if !egui::Popup::is_id_open(&ctx, owner.popup_id) {
            lock_unpoisoned(shared).popup_owners.remove(&fence_id);
        } else {
            let fg_hwnd = foreground_window_handle();
            let my_hwnd = {
                lock_unpoisoned(shared)
                    .attached_hwnds
                    .get(&fence_id)
                    .copied()
                    .unwrap_or(0)
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
                lock_unpoisoned(shared).popup_owners.remove(&fence_id);
                ctx.request_repaint();
            } else {
                lock_unpoisoned(shared).popup_owners.insert(fence_id, owner);
            }
        }
    }

    let (was_focused, has_fence_sel, is_exempt) = {
        let guard = lock_unpoisoned(shared);
        let was = guard
            .last_focused_fences
            .get(&fence_id)
            .copied()
            .unwrap_or(false);
        let has_sel = guard
            .selected_items
            .get(&fence_id)
            .is_some_and(|s| !s.is_empty());
        let exempt = guard.is_focus_clear_exempt(SelectionScope::Fence(fence_id), Some(&ctx));
        (was, has_sel, exempt)
    };
    if focused_now {
        lock_unpoisoned(shared)
            .last_focused_fences
            .insert(fence_id, true);
    } else if (lost_focus_ev || was_focused) && !is_exempt {
        let mut guard = lock_unpoisoned(shared);
        guard.last_focused_fences.insert(fence_id, false);
        if has_fence_sel {
            guard.selected_items.remove(&fence_id);
            guard.selection_anchor.remove(&fence_id);
            ctx.request_repaint();
        }
    }

    let (
        spec_opt,
        role,
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
        let mut guard = lock_unpoisoned(shared);
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
        let desktop_dirs = &guard.desktop_dirs;
        let role = spec
            .as_ref()
            .map(|s| s.effective_role(desktop_dirs))
            .unwrap_or(FenceRole::DesktopBucket);
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
            role,
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

    let _ = &ctx;

    let want_toggle = lock_unpoisoned(shared)
        .pending_toggle_collapse
        .remove(&fence_id);
    if want_toggle {
        toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
        spec.collapsed = !spec.collapsed;
    }

    let my_hwnd = {
        lock_unpoisoned(shared)
            .attached_hwnds
            .get(&fence_id)
            .copied()
            .unwrap_or(0)
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
        let mut guard = lock_unpoisoned(shared);
        if guard.title_drag.is_some_and(|td| td.fence_id == fence_id) {
            guard.title_drag = None;
        }
    }
    let active_title_drag = if spec.locked {
        None
    } else {
        lock_unpoisoned(shared)
            .title_drag
            .filter(|td| td.fence_id == fence_id)
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
                let mut guard = lock_unpoisoned(shared);
                let live_h = spec.rect.height().max(MIN_FENCE_HEIGHT);
                let live_rect = Rect::new(nx, ny, nx + td.width.max(MIN_FENCE_WIDTH), ny + live_h);
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.rect = live_rect;
                }
                if guard
                    .inspector
                    .as_ref()
                    .is_some_and(|ins| ins.target_fence_id == fence_id)
                {
                    guard.request_repaint_inspector(&ctx);
                }
            }
            ctx.request_repaint();
        } else {
            title_drag_ended_this_frame = true;
            let mut guard = lock_unpoisoned(shared);
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
                    let expanded_h = spec.rect.height().max(MIN_FENCE_HEIGHT);
                    let cand = Rect::new(snapped.left, snapped.top, snapped.right, snapped.top + expanded_h);
                    // Preserve expanded height without pushing collapsed window off-screen; clamp if needed on expand.
                    Rect::new(cand.left, cand.top, cand.right, cand.bottom)
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
                    guard.request_repaint_inspector(&ctx);
                }
            }
        }
    }

    let is_title_dragging = lock_unpoisoned(shared)
        .title_drag
        .is_some_and(|td| td.fence_id == fence_id);

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
    let topology_settling = lock_unpoisoned(shared).is_topology_settling();
    let collapse_settling = {
        let mut guard = lock_unpoisoned(shared);
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
    {
        let guard = lock_unpoisoned(shared);
        if guard
            .inspector
            .as_ref()
            .is_some_and(|ins| ins.open && ins.target_fence_id == fence_id)
        {
            guard.request_repaint_inspector(&ctx);
        }
    }
    let reveal_step = {
        lock_unpoisoned(shared)
            .pending_reveal_fences
            .get(&fence_id)
            .copied()
    };
    let collapse_anim_opt = {
        lock_unpoisoned(shared)
            .collapse_anims
            .get(&fence_id)
            .copied()
    };
    let (is_collapse_animating, open_factor) = if let Some(anim) = collapse_anim_opt {
        let raw_t = anim.started_at.elapsed().as_secs_f32()
            / (anim.duration_ms.max(1) as f32 / 1000.0);
        let eased = ease_out_cubic(raw_t);
        let cur_anim_h = (anim.from_phys_h as f32
            + (anim.to_phys_h - anim.from_phys_h) as f32 * eased)
            .round() as i32;
        let open_factor = if anim.target_collapsed {
            (1.0 - eased).clamp(0.0, 1.0)
        } else {
            eased.clamp(0.0, 1.0)
        };
        let (pos_l, pos_t, cur_w) = live_phys_rect
            .map(|r| (r.left, r.top, r.width().max(MIN_FENCE_WIDTH)))
            .unwrap_or((
                spec.rect.left,
                spec.rect.top,
                spec.rect.width().max(MIN_FENCE_WIDTH),
            ));
        if my_hwnd != 0 && reveal_step.is_none() {
            let apply_h = if raw_t >= 1.0 {
                anim.to_phys_h.max(1)
            } else {
                cur_anim_h.max(1)
            };
            desklite_win32::set_window_rect_no_z(my_hwnd, pos_l, pos_t, cur_w, apply_h);
        }
        if raw_t >= 1.0 {
            lock_unpoisoned(shared).collapse_anims.remove(&fence_id);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(
                cur_w as f32 / ppp,
                anim.to_phys_h as f32 / ppp,
            )));
            (false, if spec.collapsed { 0.0 } else { 1.0 })
        } else {
            ctx.request_repaint();
            (true, open_factor)
        }
    } else {
        (false, if spec.collapsed { 0.0 } else { 1.0 })
    };

    if let Some(cur_phys) = live_phys_rect
        && reveal_step.is_none()
        && !is_collapse_animating
        && !topology_settling
        && !any_pointer_down
        && !desklite_win32::is_in_size_move()
        && !is_title_dragging
        && !title_drag_ended_this_frame
    {
        let collapsed_phys_h = (COLLAPSED_HEIGHT * ppp).round() as i32;
        let expected_h = if spec.collapsed {
            collapsed_phys_h
        } else {
            spec.rect.height().max(MIN_FENCE_HEIGHT)
        };
        if collapse_settling && (cur_phys.height() - expected_h).abs() <= 2 {
            lock_unpoisoned(shared)
                .pending_collapse_until
                .remove(&fence_id);
        } else if collapse_settling && (cur_phys.height() - expected_h).abs() > 2 {
            // Wait for InnerSize collapse/expand resize to settle so pre-toggle height never clobbers spec.rect.
        } else if !spec.collapsed
            && (cur_phys.height() - collapsed_phys_h).abs() <= 4
            && spec.rect.height() >= MIN_FENCE_HEIGHT
        {
            // Settling timer expired before winit applied expanded InnerSize: re-assert expanded size instead of overwriting spec.rect.
            let target_w = (spec.rect.width().max(MIN_FENCE_WIDTH) as f32) / ppp;
            let target_h = (spec.rect.height().max(MIN_FENCE_HEIGHT) as f32) / ppp;
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(target_w, target_h)));
            ctx.request_repaint();
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
                    if spec.collapsed {
                        // Never shrink expanded height during horizontal resize of a collapsed bar.
                        let (clamped, _) =
                            clamp_rect_to_monitors(candidate, &monitors).unwrap_or((candidate, false));
                        Rect::new(
                            clamped.left,
                            cur_phys.top,
                            clamped.right,
                            cur_phys.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                        )
                    } else {
                        clamp_rect_to_monitors(candidate, &monitors)
                            .map(|(r, _)| r)
                            .unwrap_or(candidate)
                    }
                } else {
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
                    let snapped =
                        snap_move_rect(move_box, &sibling_rects, &monitors, snap_px, alt_held);
                    if (snapped.left - cur_phys.left).abs() > 1
                        || (snapped.top - cur_phys.top).abs() > 1
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

                let mut guard = lock_unpoisoned(shared);
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
                    guard.request_repaint_inspector(&ctx);
                }
            }
        }
    }

    let win_rect = ui.max_rect();
    let drag_suppresses_resize = {
        let guard = lock_unpoisoned(shared);
        is_file_drag_active()
            || guard.active_async_drag.is_some()
            || !guard.item_reorder.is_empty()
            || !guard.marquee_drag.is_empty()
            || guard.title_drag.is_some()
    };
    let resize_dir = if spec.locked || drag_suppresses_resize || is_collapse_animating {
        None
    } else {
        ctx.input(|i| i.pointer.hover_pos())
            .and_then(|pos| detect_resize_edge(win_rect, pos, spec.collapsed))
    };
    if let Some(dir) = resize_dir {
        ctx.set_cursor_icon(resize_cursor(dir));
    }

    let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let dropped_paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    if !dropped_paths.is_empty() {
        handle_fence_dropped_files(fence_id, dropped_paths, shared, &ctx, &mut out);
    }

    let is_bucket = matches!(role, FenceRole::DesktopBucket);
    let is_unfiled = matches!(role, FenceRole::UnfiledDesktop);
    let is_external_portal = matches!(role, FenceRole::ExternalPortal { .. });
    let border_stroke = if hovering_files && is_bucket {
        Stroke::new(2.0, Color32::from_rgb(96, 165, 250))
    } else if hovering_files && is_external_portal {
        Stroke::new(2.0, Color32::from_rgb(74, 222, 128))
    } else if hovering_files && is_unfiled {
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
    let title_alpha = (bg_alpha as f32 + 40.0 - 6.0 * open_factor).min(235.0) as u8;
    let bar_bottom_r = ((1.0 - open_factor) * 8.0).round() as u8;
    let panel_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(22, 24, 30, bg_alpha))
        .stroke(border_stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(0));

    egui::CentralPanel::default().frame(panel_frame).show(ui, |ui| {
        let title_bar_frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(32, 36, 46, title_alpha))
            .corner_radius(CornerRadius {
                nw: 8,
                ne: 8,
                sw: bar_bottom_r,
                se: bar_bottom_r,
            })
            .inner_margin(Margin {
                left: 10,
                right: 9,
                top: 6,
                bottom: 6,
            });

        let is_list = matches!(spec.kind, FenceKind::FileList);
        let view_anim_opt = {
            lock_unpoisoned(shared)
                .view_mode_anims
                .get(&fence_id)
                .copied()
        };
        let (view_content_mix, list_morph) = if let Some(va) = view_anim_opt {
            let raw_t =
                va.started_at.elapsed().as_secs_f32() / (va.duration_ms.max(1) as f32 / 1000.0);
            let mix = ease_out_cubic(raw_t);
            if raw_t >= 1.0 {
                lock_unpoisoned(shared).view_mode_anims.remove(&fence_id);
                (1.0_f32, if is_list { 1.0_f32 } else { 0.0_f32 })
            } else {
                ctx.request_repaint();
                let morph = if va.to_list { mix } else { 1.0 - mix };
                (mix, morph.clamp(0.0, 1.0))
            }
        } else {
            (1.0_f32, if is_list { 1.0_f32 } else { 0.0_f32 })
        };

        title_bar_frame.show(ui, |ui| {
            ui.set_height(COLLAPSED_HEIGHT - 12.0);
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

                if draw_header_icon_btn(ui, "切换网格 / 列表视图", |painter, r, fg| {
                    let c = r.center();
                    let m = list_morph;
                    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
                    let bar_h = lerp(3.5, 1.6, m);
                    let rx = lerp(1.0, 0.8, m);
                    let cr = CornerRadius::same(rx.round() as u8);

                    // Top & bottom pairs stretch smoothly inward and merge into full bars.
                    let x_outer = lerp(4.0, 5.0, m);
                    let x_inner = lerp(0.5, 0.0, m);
                    let y_top = lerp(-2.25, -3.5, m);
                    let y_bot = lerp(2.25, 3.5, m);
                    for yc in [y_top, y_bot] {
                        if m < 0.72 {
                            let left_r = egui::Rect::from_min_max(
                                Pos2::new(c.x - x_outer, c.y + yc - bar_h * 0.5),
                                Pos2::new(c.x - x_inner, c.y + yc + bar_h * 0.5),
                            );
                            let right_r = egui::Rect::from_min_max(
                                Pos2::new(c.x + x_inner, c.y + yc - bar_h * 0.5),
                                Pos2::new(c.x + x_outer, c.y + yc + bar_h * 0.5),
                            );
                            painter.rect_filled(left_r, cr, fg);
                            painter.rect_filled(right_r, cr, fg);
                        } else {
                            let full_r = egui::Rect::from_min_max(
                                Pos2::new(c.x - x_outer, c.y + yc - bar_h * 0.5),
                                Pos2::new(c.x + x_outer, c.y + yc + bar_h * 0.5),
                            );
                            painter.rect_filled(full_r, cr, fg);
                        }
                    }

                    // Middle list bar fades and grows out horizontally during morph.
                    if m > 0.12 {
                        let mid_t = ((m - 0.12) / 0.88).clamp(0.0, 1.0);
                        let mid_hw = 5.0 * mid_t;
                        let mid_alpha = (fg.a() as f32 * mid_t).round().clamp(0.0, 255.0) as u8;
                        let mid_col =
                            Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), mid_alpha);
                        let mid_r = egui::Rect::from_min_max(
                            Pos2::new(c.x - mid_hw, c.y - 0.8),
                            Pos2::new(c.x + mid_hw, c.y + 0.8),
                        );
                        painter.rect_filled(mid_r, CornerRadius::same(1), mid_col);
                    }
                })
                .clicked()
                {
                    let mut guard = lock_unpoisoned(shared);
                    let next_kind = match spec.kind {
                        FenceKind::FileList => FenceKind::IconGrid,
                        _ => FenceKind::FileList,
                    };
                    if let Some(f) = guard.config.fence_mut(fence_id) {
                        f.kind = next_kind;
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                    guard.view_mode_anims.insert(
                        fence_id,
                        ViewModeAnimState {
                            started_at: Instant::now(),
                            duration_ms: 170,
                            to_list: matches!(next_kind, FenceKind::FileList),
                        },
                    );
                    ctx.request_repaint();
                }

                let collapsed = spec.collapsed;
                if draw_header_icon_btn(
                    ui,
                    if collapsed { "展开格子" } else { "折叠格子" },
                    |painter, r, fg| {
                        let c = r.center();
                        let stroke = Stroke::new(1.6, fg);
                        let angle = open_factor * std::f32::consts::FRAC_PI_2;
                        let (sin_a, cos_a) = angle.sin_cos();
                        let rot = |dx: f32, dy: f32| {
                            Pos2::new(c.x + dx * cos_a - dy * sin_a, c.y + dx * sin_a + dy * cos_a)
                        };
                        let p0 = rot(-2.0, -4.0);
                        let p1 = rot(2.5, 0.0);
                        let p2 = rot(-2.0, 4.0);
                        painter.line_segment([p0, p1], stroke);
                        painter.line_segment([p1, p2], stroke);
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
                    let mut guard = lock_unpoisoned(shared);
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

                let rem_w = ui.available_width().max(24.0);
                if is_renaming_this {
                    // Snapshot draft before rendering TextEdit so SharedFenceState lock is never held across UI/clipboard calls.
                    let rp_snap = {
                        lock_unpoisoned(shared)
                            .rename_prompt
                            .as_ref()
                            .filter(|rp| rp.fence_id == fence_id)
                            .cloned()
                    };
                    if let Some(mut rp) = rp_snap {
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
                        let mut commit = None;
                        let mut cancel = false;
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            let trimmed = rp.draft.trim().to_string();
                            if !trimmed.is_empty() {
                                commit = Some(trimmed);
                            } else {
                                cancel = true;
                            }
                        } else if ui.input(|i| i.key_pressed(Key::Escape)) {
                            cancel = true;
                        } else if resp.lost_focus() {
                            let trimmed = rp.draft.trim().to_string();
                            if !trimmed.is_empty() {
                                commit = Some(trimmed);
                            } else {
                                cancel = true;
                            }
                        }
                        let mut guard = lock_unpoisoned(shared);
                        if let Some(new_name) = commit {
                            let old_name = guard.config.fence_mut(fence_id).and_then(|f| {
                                if f.name != new_name {
                                    Some(std::mem::replace(&mut f.name, new_name))
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
                            guard.rename_prompt = None;
                        } else if let Some(ref mut cur) = guard.rename_prompt
                            && cur.fence_id == fence_id
                        {
                            cur.draft = rp.draft;
                            cur.needs_focus = rp.needs_focus;
                        }
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
                            let mut guard = lock_unpoisoned(shared);
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
                                lock_unpoisoned(shared).title_drag = Some(TitleDragState {
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
                    if title_resp.clicked() {
                        if !focused_now {
                            focus_window_now(my_hwnd);
                        }
                        let mods = ctx.input(|i| i.modifiers);
                        if !mods.ctrl && !mods.shift {
                            lock_unpoisoned(shared).clear_all_selections_except(None);
                        }
                    }
                    if title_resp.secondary_clicked() {
                        if !focused_now {
                            focus_window_now(my_hwnd);
                        }
                        lock_unpoisoned(shared).clear_all_selections_except(None);
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

        if spec.collapsed && open_factor <= 0.01 {
            if my_hwnd != 0 {
                let _ = take_win32_hotkeys(my_hwnd);
            }
            return;
        }

        let top_div_y = ui.cursor().top();
        let div_alpha = (16.0 * open_factor).round().clamp(0.0, 255.0) as u8;
        ui.painter().hline(
            ui.max_rect().x_range(),
            top_div_y,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, div_alpha)),
        );

        let body_frame = Frame::new().inner_margin(Margin::symmetric(8, 0));
        body_frame.show(ui, |ui| {
            if open_factor < 0.999 {
                ui.set_opacity(open_factor.powf(0.85).clamp(0.0, 1.0));
            }
            if hovering_files {
                ui.add_space(8.0);
                if is_external_portal {
                    ui.colored_label(
                        Color32::from_rgb(134, 239, 172),
                        "松开鼠标移入/复制到映射文件夹",
                    );
                } else if is_unfiled {
                    ui.colored_label(
                        Color32::from_rgb(251, 191, 36),
                        "松开鼠标从分类格放回未归档桌面",
                    );
                } else {
                    ui.colored_label(
                        Color32::from_rgb(130, 195, 255),
                        "松开鼠标收纳到此格子",
                    );
                }
            } else if spec.items.is_empty() {
                let mut blank_action: Option<ItemUiAction> = None;
                let empty_hit_rect = ui.available_rect_before_wrap().expand2(Vec2::new(8.0, 0.0));
                let empty_resp = ui.interact(
                    empty_hit_rect,
                    ui.make_persistent_id(("empty_body", fence_id)),
                    Sense::click(),
                );
                if (empty_resp.clicked() || empty_resp.secondary_clicked()) && !focused_now {
                    focus_window_now(my_hwnd);
                }
                if (empty_resp.clicked() || empty_resp.secondary_clicked()) && resize_dir.is_none()
                {
                    let mods = ctx.input(|i| i.modifiers);
                    if !mods.ctrl && !mods.shift {
                        lock_unpoisoned(shared).clear_all_selections_except(None);
                    }
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
                        let cmd = (i.modifiers.command || desklite_win32::is_ctrl_key_down())
                            && !i.modifiers.alt;
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
                let hint = if is_unfiled {
                    "全部桌面文件已归档，暂无未归档条目"
                } else {
                    "可从桌面或资源管理器拖入文件，或 Ctrl+V 粘贴\n也可以拖动条目放到桌面或其它格子"
                };
                ui.painter().text(
                    empty_resp.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    hint,
                    egui::FontId::proportional(12.0),
                    Color32::from_rgba_unmultiplied(200, 210, 228, 175),
                );
                return;
            }

            let mut item_action: Option<ItemUiAction> = None;
            let mut opened_item_popup: Option<egui::Id> = None;
            let mut item_rects: Vec<(usize, egui::Rect)> = Vec::with_capacity(spec.items.len());
            let viewport_clip_rect = ui.available_rect_before_wrap();
            let blank_hit_rect = viewport_clip_rect.expand2(Vec2::new(8.0, 0.0));
            let blank_resp = ui.interact(
                blank_hit_rect,
                ui.make_persistent_id(("blank_body", fence_id)),
                Sense::click_and_drag(),
            );
            if (blank_resp.clicked() || blank_resp.secondary_clicked()) && resize_dir.is_none() {
                if !focused_now {
                    focus_window_now(my_hwnd);
                }
                let mods = ctx.input(|i| i.modifiers);
                if !mods.ctrl && !mods.shift {
                    lock_unpoisoned(shared).clear_all_selections_except(None);
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
                    if view_content_mix < 0.999 {
                        ui.set_opacity((0.22 + 0.78 * view_content_mix).clamp(0.0, 1.0));
                    }
                    ui.add_space(6.0 + (1.0 - view_content_mix) * 7.0);
                    let avail_w = ui.available_width().max(cell_w);
                    let sel_count = selected_set.len();
                    let mut rename_commit: Option<(usize, String, String)> = None;
                    let mut rename_cancel = false;

                    let item_textures = {
                        let mut guard = lock_unpoisoned(shared);
                        guard.resolve_item_textures(&ctx, &spec.items)
                    };

                    match spec.kind {
                        FenceKind::FileList => {
                            for (idx, item) in spec.items.iter().enumerate() {
                                let tex = item_textures.get(idx).and_then(|t| t.as_ref());
                                let selected = selected_set.contains(&idx);
                                let is_cut =
                                    cut_paths_snap.contains(&normalize_path_key(&item.path));
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
                                        fence_id,
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
                                        is_bucket,
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
                            let cols =
                                (((avail_w + gap_x) / (tile_w + gap_x)).floor() as usize).max(1);
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
                                            let tex =
                                                item_textures.get(idx).and_then(|t| t.as_ref());
                                            let selected = selected_set.contains(&idx);
                                            let is_cut = cut_paths_snap
                                                .contains(&normalize_path_key(&item.path));
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
                                                    fence_id,
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
                                                    is_bucket,
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
                        crate::actions::commit_item_rename(
                            fence_id,
                            idx,
                            &old_path,
                            &new_name,
                            shared,
                            &ctx,
                            &mut out,
                        );
                    } else if rename_cancel {
                        lock_unpoisoned(shared).item_rename_prompt = None;
                    }
                    ui.add_space(6.0);
                });

            if let Some(ref b_resp) = body_blank_resp {
                let pointer_pos = ctx.input(|i| i.pointer.interact_pos());
                let primary_down = ctx.input(|i| i.pointer.primary_down());
                let reorder_busy = lock_unpoisoned(shared).item_reorder.contains_key(&fence_id);
                if !reorder_busy
                    && b_resp.drag_started_by(PointerButton::Primary)
                    && resize_dir.is_none()
                    && let Some(pos) = pointer_pos
                {
                    let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                    let additive = ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                    let mut guard = lock_unpoisoned(shared);
                    guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
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
                    let mut guard = lock_unpoisoned(shared);
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

            let active_reorder = { lock_unpoisoned(shared).item_reorder.get(&fence_id).cloned() };
            if let Some(reorder) = active_reorder {
                let pointer_pos = ctx.input(|i| i.pointer.interact_pos());
                let primary_down = ctx.input(|i| i.pointer.primary_down());
                let inside_fence =
                    pointer_pos.is_some_and(|p| win_rect.shrink(RESIZE_EDGE + 1.0).contains(p));
                if primary_down && !inside_fence {
                    lock_unpoisoned(shared).item_reorder.remove(&fence_id);
                    start_async_drag_from_paths(Some(fence_id), reorder.paths, shared);
                } else if primary_down
                    && let Some(pos) = pointer_pos
                    && let Some(insert_idx) = compute_reorder_target(
                        &item_rects,
                        pos,
                        matches!(spec.kind, FenceKind::FileList),
                    )
                {
                    paint_reorder_indicator(
                        ui,
                        viewport_clip_rect,
                        &item_rects,
                        insert_idx,
                        matches!(spec.kind, FenceKind::FileList),
                    );
                } else if !primary_down {
                    lock_unpoisoned(shared).item_reorder.remove(&fence_id);
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
                lock_unpoisoned(shared).popup_owners.insert(
                    fence_id,
                    PopupOwnerState {
                        popup_id: pid,
                        saw_focused: focused_now,
                    },
                );
            }

            let marquee_active = lock_unpoisoned(shared).marquee_drag.contains_key(&fence_id);
            let reorder_active = lock_unpoisoned(shared).item_reorder.contains_key(&fence_id);
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
            let current_selected = lock_unpoisoned(shared)
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
                if can_handle_hotkeys { list } else { Vec::new() }
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
            let current_selected = lock_unpoisoned(shared)
                .selected_items
                .get(&fence_id)
                .cloned()
                .unwrap_or_default();
            if !spec.collapsed && can_handle_hotkeys && item_action.is_none() && has_input_focus {
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

    if let Some(step) = reveal_step
        && my_hwnd != 0
    {
        if step > 0 {
            lock_unpoisoned(shared)
                .pending_reveal_fences
                .insert(fence_id, step - 1);
            ctx.request_repaint();
        } else {
            lock_unpoisoned(shared).pending_reveal_fences.remove(&fence_id);
            let target_h = if spec.collapsed {
                (COLLAPSED_HEIGHT * ppp).round() as i32
            } else {
                spec.rect.height().max(MIN_FENCE_HEIGHT)
            };
            desklite_win32::set_window_rect_no_z(
                my_hwnd,
                spec.rect.left,
                spec.rect.top,
                spec.rect.width().max(MIN_FENCE_WIDTH),
                target_h,
            );
            desklite_win32::post_reassert_all_fences_z();
            desklite_win32::post_uncloak_after_swap(my_hwnd);
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::commit_item_rename;
    use crate::inspector::close_inspector_safely;
    use crate::undo::execute_undo;
    use desklite_core::{DisplayTopology, FenceSpec, ItemKind, MonitorDescriptor, NativeLayoutConfig, Point};
    use desklite_win32::{DragTargetOutcome, IconCache};

    fn test_shared(fences: Vec<FenceSpec>) -> Arc<Mutex<SharedFenceState>> {
        let mut cfg = NativeLayoutConfig {
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
        let _ = cfg.apply_topology(&topo, (72, 72));
        let icon_cache = IconCache::new().expect("icon cache init");
        Arc::new(Mutex::new(SharedFenceState::new(cfg, topo, icon_cache)))
    }

    #[test]
    fn undo_restores_fence_rename_remove_reorder_and_file_rename() {
        let tmp_dir = std::env::temp_dir().join(format!("desklite_undo_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let old_file = tmp_dir.join("alpha.txt");
        let beta_file = tmp_dir.join("beta.txt");
        std::fs::write(&old_file, "hello").unwrap();
        std::fs::write(&beta_file, "world").unwrap();

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
            path: beta_file.to_string_lossy().into_owned(),
        };
        let fence = FenceSpec {
            id: 101,
            name: "OrigFence".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 420, 400),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
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
            let mut g = lock_unpoisoned(&shared);
            g.config.fence_mut(101).unwrap().name = "NewFence".to_string();
            g.push_undo(UndoAction::RenameFence {
                fence_id: 101,
                old_name: "OrigFence".to_string(),
            });
        }
        execute_undo(Some(101), &shared, &ctx, &mut out);
        assert_eq!(lock_unpoisoned(&shared).config.fences[0].name, "OrigFence");

        // 2. Undo reorder / sort
        apply_reorder_items(101, &[1], 0, &shared, &mut out);
        assert_eq!(
            lock_unpoisoned(&shared).config.fences[0].items[0].display_name,
            "beta"
        );
        execute_undo(Some(101), &shared, &ctx, &mut out);
        assert_eq!(
            lock_unpoisoned(&shared).config.fences[0].items[0].display_name,
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
        execute_undo(Some(101), &shared, &ctx, &mut out);
        assert!(old_file.exists());
        assert!(!tmp_dir.join("renamed.txt").exists());

        // 4. Undo remove items from fence
        {
            let mut g = lock_unpoisoned(&shared);
            g.selected_items.insert(101, [0usize].into_iter().collect());
        }
        let spec_snap = lock_unpoisoned(&shared).config.fences[0].clone();
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
        assert_eq!(lock_unpoisoned(&shared).config.fences[0].items.len(), 1);
        execute_undo(Some(101), &shared, &ctx, &mut out);
        assert_eq!(lock_unpoisoned(&shared).config.fences[0].items.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn single_ownership_async_drag_moves_all_items_and_releases_to_free_desktop() {
        let bucket_a = FenceSpec {
            id: 101,
            name: "BucketA".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(20, 20, 320, 320),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: (0..5)
                .map(|i| FenceItem {
                    display_name: format!("f{i}.txt"),
                    order: i,
                    kind: ItemKind::File,
                    path: format!("C:\\Users\\Demo\\Desktop\\f{i}.txt"),
                })
                .collect(),
        };
        let bucket_b = FenceSpec {
            id: 102,
            name: "BucketB".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(360, 20, 660, 320),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let shared = test_shared(vec![bucket_a, bucket_b]);
        {
            let mut g = lock_unpoisoned(&shared);
            g.set_desktop_dirs(vec![r"C:\Users\Demo\Desktop".to_string()]);
            g.register_fence_hwnd(101, 1111);
            g.register_fence_hwnd(102, 2222);
        }

        // Simulate atomic transfer of all 5 items from BucketA -> BucketB via async drag outcome
        let paths: Vec<String> = (0..5)
            .map(|i| format!("C:\\Users\\Demo\\Desktop\\f{i}.txt"))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(DragTargetOutcome::DroppedOnFence {
            target_hwnd: 2222,
            screen_x: 420,
            screen_y: 120,
        })
        .unwrap();
        {
            let mut g = lock_unpoisoned(&shared);
            g.active_async_drag = Some(crate::state::AsyncDragSession {
                source_fence_id: Some(101),
                paths: paths.clone(),
                is_desktop_resident: true,
                rx,
            });
            assert!(g.poll_async_drag());
            assert_eq!(g.config.fences[0].items.len(), 0);
            assert_eq!(g.config.fences[1].items.len(), 5);
        }

        // Undo restores all 5 items back to BucketA
        let ctx = egui::Context::default();
        let mut out = FenceUiOutput::default();
        execute_undo(Some(102), &shared, &ctx, &mut out);
        {
            let g = lock_unpoisoned(&shared);
            assert_eq!(g.config.fences[0].items.len(), 5);
            assert_eq!(g.config.fences[1].items.len(), 0);
        }

        // Release 2 items from BucketA onto FreeDesktop
        let (tx2, rx2) = std::sync::mpsc::channel();
        tx2.send(DragTargetOutcome::DroppedOnFreeDesktop {
            screen_x: 900,
            screen_y: 400,
        })
        .unwrap();
        {
            let mut g = lock_unpoisoned(&shared);
            g.active_async_drag = Some(crate::state::AsyncDragSession {
                source_fence_id: Some(101),
                paths: paths[..2].to_vec(),
                is_desktop_resident: true,
                rx: rx2,
            });
            assert!(g.poll_async_drag());
            assert_eq!(g.config.fences[0].items.len(), 3);
            assert_eq!(g.config.active_free_icons().len(), 2);
        }
    }

    #[test]
    fn collect_managed_desktop_paths_excludes_external_and_unfiled() {
        let desktop_dir = r"C:\Users\Demo\Desktop".to_string();
        let cfg = NativeLayoutConfig {
            fences: vec![
                FenceSpec {
                    id: 101,
                    name: "Bucket".into(),
                    kind: FenceKind::IconGrid,
                    rect: Rect::new(0, 0, 300, 300),
                    collapsed: false,
                    locked: false,
                    role: Some(FenceRole::DesktopBucket),
                    portal_dir: None,
                    backdrop_alpha: 115,
                    layout_override: None,
                    items: vec![FenceItem {
                        display_name: "a.txt".into(),
                        order: 0,
                        kind: ItemKind::File,
                        path: r"C:\Users\Demo\Desktop\a.txt".into(),
                    }],
                },
                FenceSpec {
                    id: 102,
                    name: "Unfiled".into(),
                    kind: FenceKind::IconGrid,
                    rect: Rect::new(0, 0, 300, 300),
                    collapsed: false,
                    locked: false,
                    role: Some(FenceRole::UnfiledDesktop),
                    portal_dir: None,
                    backdrop_alpha: 115,
                    layout_override: None,
                    items: vec![FenceItem {
                        display_name: "b.txt".into(),
                        order: 0,
                        kind: ItemKind::File,
                        path: r"C:\Users\Demo\Desktop\b.txt".into(),
                    }],
                },
                FenceSpec {
                    id: 103,
                    name: "Docs".into(),
                    kind: FenceKind::IconGrid,
                    rect: Rect::new(0, 0, 300, 300),
                    collapsed: false,
                    locked: false,
                    role: Some(FenceRole::ExternalPortal {
                        dir: r"D:\Docs".into(),
                    }),
                    portal_dir: Some(r"D:\Docs".into()),
                    backdrop_alpha: 115,
                    layout_override: None,
                    items: vec![FenceItem {
                        display_name: "c.txt".into(),
                        order: 0,
                        kind: ItemKind::File,
                        path: r"D:\Docs\c.txt".into(),
                    }],
                },
            ],
            ..Default::default()
        };
        let managed = collect_managed_desktop_paths_for_dirs(&cfg, &[desktop_dir]);
        assert!(managed.iter().all(|p| !p.contains("b.txt") && !p.contains("c.txt")));
    }

    #[test]
    fn collapsed_fence_height_is_preserved_after_toggle() {
        let fence = FenceSpec {
            id: 109,
            name: "Tall".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 420, 460),
            collapsed: true,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let shared = test_shared(vec![fence]);
        let ctx = egui::Context::default();
        // Expand fence even if screen_rect is temporarily still collapsed height (38px)
        {
            let mut g = lock_unpoisoned(&shared);
            g.pending_toggle_collapse.insert(109);
        }
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::new(100.0, 100.0),
                Vec2::new(320.0, COLLAPSED_HEIGHT),
            )),
            focused: true,
            ..Default::default()
        };
        let mut o = ctx.run_ui(raw, |ui| {
            let _ = render_fence_viewport(ui, 109, &shared);
        });
        o.textures_delta.clear();
        let g = lock_unpoisoned(&shared);
        assert!(!g.config.fences[0].collapsed);
        assert_eq!(g.config.fences[0].rect.height(), 360);
    }

    #[test]
    fn drawer_collapse_anim_eases_and_preserves_targets() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < 1e-6);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);

        let fence = FenceSpec {
            id: 110,
            name: "Anim".to_string(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 420, 460),
            collapsed: true,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let shared = test_shared(vec![fence.clone()]);
        lock_unpoisoned(&shared).attached_hwnds.insert(110, 0x1234_isize);
        let ctx = egui::Context::default();
        let mut out = FenceUiOutput::default();
        toggle_collapse(110, &fence, &shared, &ctx, &mut out);

        let guard = lock_unpoisoned(&shared);
        let anim = guard.collapse_anims.get(&110).expect("animation queued");
        assert!(!anim.target_collapsed);
        assert_eq!(anim.from_phys_h, COLLAPSED_HEIGHT.round() as i32);
        assert_eq!(anim.to_phys_h, 360);
        assert!(!guard.config.fences[0].collapsed);
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
            role: Some(FenceRole::DesktopBucket),
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
        assert!(lock_unpoisoned(&shared).pending_toggle_collapse.contains(&102));

        assert!(apply_collapsed_menu_command(
            102,
            CollapsedMenuCommand::SortName,
            &shared,
            &ctx
        ));
        assert_eq!(
            lock_unpoisoned(&shared).config.fences[0].items[0].display_name,
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
            role: Some(FenceRole::DesktopBucket),
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
        lock_unpoisoned(&shared)
            .selected_items
            .insert(105, [0usize].into_iter().collect());
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 420.0));

        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            ..Default::default()
        };
        let mut out1 = ctx.run_ui(raw.clone(), |ui| {
            let _ = render_fence_viewport(ui, 105, &shared);
        });
        out1.textures_delta.clear();

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

        let sel_after_left = lock_unpoisoned(&shared).selected_items.get(&105).cloned();
        assert!(
            sel_after_left.as_ref().is_none_or(|s| s.is_empty()),
            "expected blank left click to clear selection, got {sel_after_left:?}"
        );

        raw.events = vec![egui::Event::PointerButton {
            pos: blank_pos,
            button: PointerButton::Secondary,
            pressed: true,
            modifiers: Default::default(),
        }];
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
        let cmds_105 = std::mem::take(&mut lock_unpoisoned(&shared).pending_commands);
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
            role: Some(FenceRole::ExternalPortal {
                dir: "D:\\Projects".to_string(),
            }),
            portal_dir: Some("D:\\Projects".to_string()),
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
                path: "D:\\Projects\\a.txt".to_string(),
            }],
        };
        assert_eq!(format_fence_title(&portal), "Desktop");
        let prefs = desklite_core::LayoutPreferences::default();
        assert_eq!(
            resolve_fence_layout(&portal, &prefs),
            (128.0, 136.0, 30.0, 18.0)
        );

        let shared = test_shared(vec![portal]);
        let ctx = egui::Context::default();

        assert!(apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::ToggleLock,
            &shared,
            &ctx
        ));
        assert!(lock_unpoisoned(&shared).config.fences[0].locked);

        assert!(apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::DuplicateStyle,
            &shared,
            &ctx
        ));
        {
            let guard = lock_unpoisoned(&shared);
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

        assert!(!apply_collapsed_menu_command(
            201,
            CollapsedMenuCommand::OpenParamsEditor,
            &shared,
            &ctx
        ));
        {
            let guard = lock_unpoisoned(&shared);
            assert!(guard.inspector_spawned);
            assert!(guard.pending_toggle_collapse.contains(&201));
            assert_eq!(
                guard.inspector.as_ref().map(|i| (i.open, i.target_fence_id)),
                Some((true, 201))
            );
        }

        // Edit draft_name and close inspector -> commits name & pushes RenameFence to undo_stack
        {
            let mut guard = lock_unpoisoned(&shared);
            if let Some(ref mut ins) = guard.inspector {
                ins.draft_name = Some((201, "Renamed".to_string()));
            }
        }
        let mut close_out = FenceUiOutput::default();
        close_inspector_safely(&shared, &ctx, &mut close_out);
        assert!(close_out.needs_immediate_save);
        {
            let guard = lock_unpoisoned(&shared);
            assert_eq!(guard.config.fences[0].name, "Renamed");
            assert_eq!(guard.inspector.as_ref().map(|i| i.open), Some(false));
            assert!(guard.inspector_spawned);
            assert!(matches!(
                guard.undo_stack.last(),
                Some(UndoAction::RenameFence {
                    fence_id: 201,
                    old_name
                }) if old_name == "Desktop"
            ));
        }

        let mut out = FenceUiOutput::default();
        let applied = apply_param_editor_rect(201, 0, 120, 140, 480, 520, &shared, &ctx, &mut out);
        assert!(out.needs_immediate_save);
        assert_eq!(applied, Rect::new(120, 140, 600, 660));
        assert_eq!(lock_unpoisoned(&shared).config.fences[0].rect, applied);
    }

    #[test]
    fn free_desktop_menu_arrange_and_move_to_fence_work() {
        let bucket = FenceSpec {
            id: 101,
            name: "Work".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(400, 100, 700, 400),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let shared = test_shared(vec![bucket]);
        {
            let mut g = lock_unpoisoned(&shared);
            g.set_desktop_dirs(vec![r"C:\Users\Demo\Desktop".to_string()]);
            let snap_key = g.config.active_topology.clone().unwrap();
            let snap = g.config.snapshots.get_mut(&snap_key).unwrap();
            snap.free_icons = vec![
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\z.txt".into(),
                    display_name: "z.txt".into(),
                    pos: Point { x: 20, y: 20 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\a.txt".into(),
                    display_name: "a.txt".into(),
                    pos: Point { x: 120, y: 20 },
                },
            ];
        }
        let ctx = egui::Context::default();
        assert!(apply_free_desktop_menu_command(
            FreeDesktopMenuCommand::ArrangeByName,
            &shared,
            &ctx
        ));
        {
            let g = lock_unpoisoned(&shared);
            assert_eq!(g.config.active_free_icons()[0].display_name, "a.txt");
        }
        assert!(apply_free_icon_menu_command(
            &[r"C:\Users\Demo\Desktop\a.txt".to_string()],
            FreeIconMenuCommand::MoveToFence(101),
            &shared,
            &ctx
        ));
        {
            let g = lock_unpoisoned(&shared);
            assert_eq!(g.config.fences[0].items.len(), 1);
            assert_eq!(g.config.active_free_icons().len(), 1);
        }
    }

    #[test]
    fn dropping_unfiled_desktop_file_into_bucket_transfers_without_duplicating() {
        let bucket = FenceSpec {
            id: 101,
            name: "Work".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(400, 100, 700, 400),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let unfiled = FenceSpec {
            id: 102,
            name: "Unfiled".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(20, 100, 320, 400),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::UnfiledDesktop),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![FenceItem {
                display_name: "note.txt".into(),
                order: 0,
                kind: ItemKind::File,
                path: r"C:\Users\Demo\Desktop\note.txt".into(),
            }],
        };
        let shared = test_shared(vec![bucket, unfiled]);
        {
            let mut g = lock_unpoisoned(&shared);
            g.set_desktop_dirs(vec![r"C:\Users\Demo\Desktop".to_string()]);
            let snap_key = g.config.active_topology.clone().unwrap();
            g.config.snapshots.get_mut(&snap_key).unwrap().free_icons = vec![DesktopIconPos {
                id: r"C:\Users\Demo\Desktop\note.txt".into(),
                display_name: "note.txt".into(),
                pos: Point { x: 20, y: 20 },
            }];
        }
        let ctx = egui::Context::default();
        let mut out = FenceUiOutput::default();
        handle_fence_dropped_files(
            101,
            vec![r"C:\Users\Demo\Desktop\note.txt".to_string()],
            &shared,
            &ctx,
            &mut out,
        );
        assert!(out.needs_immediate_save);
        let g = lock_unpoisoned(&shared);
        assert_eq!(g.config.fences[0].items.len(), 1);
        assert_eq!(
            g.config.fences[0].items[0].path,
            r"C:\Users\Demo\Desktop\note.txt"
        );
        assert!(g.config.active_free_icons().is_empty());
        assert!(matches!(
            g.undo_stack.last(),
            Some(UndoAction::MoveFreeIconsToBucket { dst_fence_id: 101, .. })
        ));
    }

    #[test]
    fn topology_settling_prevents_overwriting_old_snapshot_during_debounce() {
        let dual = DisplayTopology::try_from_monitors(vec![
            MonitorDescriptor {
                device_key: "DP-1".into(),
                friendly_name: "Primary".into(),
                rect: Rect::new(0, 0, 2560, 1440),
                work_area: Rect::new(0, 0, 2560, 1392),
                dpi_x: 96,
                dpi_y: 96,
                is_primary: true,
            },
            MonitorDescriptor {
                device_key: "HDMI-1".into(),
                friendly_name: "Secondary".into(),
                rect: Rect::new(2560, 0, 4480, 1080),
                work_area: Rect::new(2560, 0, 4480, 1032),
                dpi_x: 96,
                dpi_y: 96,
                is_primary: false,
            },
        ])
        .unwrap();
        let dual_key = dual.key().clone();
        let orig_rect = Rect::new(2800, 120, 3120, 440);
        let fence = FenceSpec {
            id: 101,
            name: "SecondScreen".into(),
            kind: FenceKind::IconGrid,
            rect: orig_rect,
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let mut cfg = NativeLayoutConfig {
            fences: vec![fence],
            ..Default::default()
        };
        let _ = cfg.apply_topology(&dual, (72, 72));
        let icon_cache = IconCache::new().unwrap();
        let shared = Arc::new(Mutex::new(SharedFenceState::new(cfg, dual, icon_cache)));

        {
            let mut g = lock_unpoisoned(&shared);
            g.begin_topology_settling(Duration::from_millis(2000));
            assert!(g.is_topology_settling());
        }

        // Simulate OS pushing the secondary-screen HWND onto the primary monitor during debounce
        let ctx = egui::Context::default();
        let pushed_raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::new(2200.0, 120.0),
                Vec2::new(320.0, 320.0),
            )),
            focused: false,
            ..Default::default()
        };
        let mut out = ctx.run_ui(pushed_raw, |ui| {
            let _ = render_fence_viewport(ui, 101, &shared);
        });
        out.textures_delta.clear();

        let g = lock_unpoisoned(&shared);
        assert_eq!(g.config.fences[0].rect, orig_rect);
        assert_eq!(
            g.config.snapshots.get(&dual_key).and_then(|s| s.fences.get(&101)).copied(),
            Some(orig_rect)
        );
    }

    #[test]
    fn clear_all_selections_except_enforces_global_single_focus() {
        let f1 = FenceSpec {
            id: 101,
            name: "One".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(20, 20, 320, 320),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let f2 = FenceSpec {
            id: 102,
            name: "Two".into(),
            ..f1.clone()
        };
        let shared = test_shared(vec![f1, f2]);
        let mut g = lock_unpoisoned(&shared);
        g.selected_items.insert(101, [0, 1].into_iter().collect());
        g.selection_anchor.insert(101, 0);
        g.selected_items.insert(102, [2].into_iter().collect());
        g.selection_anchor.insert(102, 2);
        g.free_selected_paths.insert("desktop/a.txt".into());
        g.free_selection_anchor = Some("desktop/a.txt".into());

        g.clear_all_selections_except(Some(SelectionScope::Fence(101)));
        assert_eq!(g.selected_items.get(&101).map(HashSet::len), Some(2));
        assert!(!g.selected_items.contains_key(&102));
        assert!(g.free_selected_paths.is_empty());

        g.selected_items.insert(102, [1].into_iter().collect());
        g.free_selected_paths.insert("desktop/b.txt".into());
        g.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
        assert!(g.selected_items.is_empty());
        assert_eq!(g.free_selected_paths.len(), 1);

        g.selected_items.insert(101, [0].into_iter().collect());
        g.clear_all_selections_except(None);
        assert!(g.selected_items.is_empty());
        assert!(g.free_selected_paths.is_empty());
    }

    #[test]
    fn focus_clear_exemption_and_fence_focus_loss_deselect() {
        let f1 = FenceSpec {
            id: 101,
            name: "One".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(20, 20, 320, 320),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![FenceItem {
                display_name: "a.txt".into(),
                order: 0,
                kind: ItemKind::File,
                path: r"C:\Users\Demo\Desktop\a.txt".into(),
            }],
        };
        let shared = test_shared(vec![f1]);
        {
            let mut g = lock_unpoisoned(&shared);
            assert!(!g.is_focus_clear_exempt(SelectionScope::Fence(101), None));
            g.menu_popup_active = true;
            assert!(g.is_focus_clear_exempt(SelectionScope::Fence(101), None));
            assert!(g.is_focus_clear_exempt(SelectionScope::FreeDesktop, None));
            g.menu_popup_active = false;
            g.item_rename_prompt = Some(crate::state::ItemRenamePrompt {
                fence_id: 101,
                item_idx: 0,
                original_path: r"C:\Users\Demo\Desktop\a.txt".into(),
                draft: "a.txt".into(),
                needs_focus: false,
            });
            assert!(g.is_focus_clear_exempt(SelectionScope::Fence(101), None));
            assert!(!g.is_focus_clear_exempt(SelectionScope::FreeDesktop, None));
            g.item_rename_prompt = None;
            g.selected_items.insert(101, [0].into_iter().collect());
            g.last_focused_fences.insert(101, true);
        }

        let ctx = egui::Context::default();
        let unfocused = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::new(0.0, 0.0),
                Vec2::new(300.0, 300.0),
            )),
            focused: false,
            events: vec![egui::Event::WindowFocused(false)],
            ..Default::default()
        };
        let mut out = ctx.run_ui(unfocused, |ui| {
            let _ = render_fence_viewport(ui, 101, &shared);
        });
        out.textures_delta.clear();

        let g = lock_unpoisoned(&shared);
        assert!(
            g.selected_items.get(&101).is_none_or(HashSet::is_empty),
            "non-exempt focus loss should clear fence selection"
        );
    }

    #[test]
    fn free_desktop_snapshot_and_event_bridge_handle_selection_marquee_and_focus_loss() {
        let f1 = FenceSpec {
            id: 101,
            name: "Bucket".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(400, 100, 700, 400),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: Vec::new(),
        };
        let shared = test_shared(vec![f1]);
        {
            let mut g = lock_unpoisoned(&shared);
            let snap_key = g.config.active_topology.clone().unwrap();
            g.config.snapshots.get_mut(&snap_key).unwrap().free_icons = vec![
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\alpha.txt".into(),
                    display_name: "alpha.txt".into(),
                    pos: Point { x: 20, y: 20 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\beta.txt".into(),
                    display_name: "beta.txt".into(),
                    pos: Point { x: 140, y: 20 },
                },
            ];
            g.selected_items.insert(101, [0].into_iter().collect());
            let snap = build_free_desktop_snapshot(&g);
            assert_eq!(snap.icons.len(), 2);
            assert!(!snap.hide_all_fences);
        }

        let r1 = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::IconClicked {
                path: r"C:\Users\Demo\Desktop\alpha.txt".into(),
                ctrl: false,
                shift: false,
            },
            &shared,
        );
        assert!(r1.needs_snapshot_refresh);
        assert_eq!(r1.repaint_fences, vec![101]);
        {
            let g = lock_unpoisoned(&shared);
            assert!(g.selected_items.is_empty());
            let snap = build_free_desktop_snapshot(&g);
            assert!(snap.icons[0].selected);
            assert!(!snap.icons[1].selected);
        }

        let r2 = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::MarqueeSelect {
                rect: Rect::new(10, 10, 200, 90),
                additive: false,
            },
            &shared,
        );
        assert!(r2.needs_snapshot_refresh);
        assert_eq!(lock_unpoisoned(&shared).free_selected_paths.len(), 2);

        // Exempt while menu_popup_active is true.
        lock_unpoisoned(&shared).menu_popup_active = true;
        let r3 = handle_free_desktop_event(desklite_win32::FreeDesktopEvent::FocusLost, &shared);
        assert!(!r3.needs_snapshot_refresh);
        assert_eq!(lock_unpoisoned(&shared).free_selected_paths.len(), 2);

        // External FocusLost clears selection once exemption ends.
        lock_unpoisoned(&shared).menu_popup_active = false;
        let r4 = handle_free_desktop_event(desklite_win32::FreeDesktopEvent::FocusLost, &shared);
        assert!(r4.needs_snapshot_refresh);
        assert!(lock_unpoisoned(&shared).free_selected_paths.is_empty());

        // BlankClicked clears both fence and free desktop selections.
        {
            let mut g = lock_unpoisoned(&shared);
            g.selected_items.insert(101, [0].into_iter().collect());
            g.free_selected_paths
                .insert(normalize_path_key(r"C:\Users\Demo\Desktop\alpha.txt"));
        }
        let r5 =
            handle_free_desktop_event(desklite_win32::FreeDesktopEvent::BlankClicked, &shared);
        assert!(r5.needs_snapshot_refresh);
        assert_eq!(r5.repaint_fences, vec![101]);
        let g = lock_unpoisoned(&shared);
        assert!(g.selected_items.is_empty());
        assert!(g.free_selected_paths.is_empty());
    }

    #[test]
    fn free_desktop_hotkeys_handle_select_all_clear_and_refresh() {
        let shared = test_shared(Vec::new());
        {
            let mut g = lock_unpoisoned(&shared);
            let snap_key = g.config.active_topology.clone().unwrap();
            g.config.snapshots.get_mut(&snap_key).unwrap().free_icons = vec![
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\one.txt".into(),
                    display_name: "one.txt".into(),
                    pos: Point { x: 20, y: 20 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\two.txt".into(),
                    display_name: "two.txt".into(),
                    pos: Point { x: 140, y: 20 },
                },
            ];
        }
        let r1 = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::SelectAll),
            &shared,
        );
        assert!(r1.needs_snapshot_refresh);
        assert_eq!(lock_unpoisoned(&shared).free_selected_paths.len(), 2);

        let r2 = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(
                desklite_win32::FreeDesktopHotkey::ClearSelection,
            ),
            &shared,
        );
        assert!(r2.needs_snapshot_refresh);
        assert!(lock_unpoisoned(&shared).free_selected_paths.is_empty());

        let r3 = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::Refresh),
            &shared,
        );
        assert!(r3.commands.contains(&FenceCommand::RefreshDesktop));
        assert!(lock_unpoisoned(&shared).free_icon_epoch >= 1);
    }

    #[test]
    fn free_desktop_hotkeys_navigate_type_ahead_clear_cut_and_open() {
        let shared = test_shared(Vec::new());
        {
            let mut g = lock_unpoisoned(&shared);
            let snap_key = g.config.active_topology.clone().unwrap();
            g.config.snapshots.get_mut(&snap_key).unwrap().free_icons = vec![
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\Alpha.txt".into(),
                    display_name: "Alpha.txt".into(),
                    pos: Point { x: 20, y: 20 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\Beta.txt".into(),
                    display_name: "Beta.txt".into(),
                    pos: Point { x: 140, y: 20 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\Alps.txt".into(),
                    display_name: "Alps.txt".into(),
                    pos: Point { x: 20, y: 140 },
                },
                DesktopIconPos {
                    id: r"C:\Users\Demo\Desktop\Gamma.txt".into(),
                    display_name: "Gamma.txt".into(),
                    pos: Point { x: 140, y: 140 },
                },
            ];
        }

        // Navigate Right from nothing -> selects first (Alpha), then Right -> Beta, Down -> Gamma.
        let _ = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::Navigate {
                key: desklite_win32::FreeDesktopNavKey::Home,
                shift: false,
            }),
            &shared,
        );
        assert_eq!(
            lock_unpoisoned(&shared).free_selection_anchor.as_deref(),
            Some(r"c:\users\demo\desktop\alpha.txt")
        );

        let r_right = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::Navigate {
                key: desklite_win32::FreeDesktopNavKey::Right,
                shift: true,
            }),
            &shared,
        );
        assert!(r_right.needs_snapshot_refresh);
        {
            let g = lock_unpoisoned(&shared);
            assert_eq!(g.free_selected_paths.len(), 2);
            assert!(g
                .free_selected_paths
                .contains(r"c:\users\demo\desktop\alpha.txt"));
            assert!(g
                .free_selected_paths
                .contains(r"c:\users\demo\desktop\beta.txt"));
        }

        let _ = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::Navigate {
                key: desklite_win32::FreeDesktopNavKey::End,
                shift: false,
            }),
            &shared,
        );
        assert!(lock_unpoisoned(&shared)
            .free_selected_paths
            .contains(r"c:\users\demo\desktop\gamma.txt"));

        // TypeAhead "al" -> matches Alpha then Alps.
        let _ = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::TypeAhead(
                'a',
            )),
            &shared,
        );
        assert!(lock_unpoisoned(&shared)
            .free_selected_paths
            .contains(r"c:\users\demo\desktop\alpha.txt"));
        let _ = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(desklite_win32::FreeDesktopHotkey::TypeAhead(
                'l',
            )),
            &shared,
        );
        assert!(lock_unpoisoned(&shared)
            .free_selected_paths
            .contains(r"c:\users\demo\desktop\alps.txt"));

        // OpenSelected on nonexistent path writes toast and keeps selection/anchor.
        let r_open = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(
                desklite_win32::FreeDesktopHotkey::OpenSelected,
            ),
            &shared,
        );
        assert!(r_open.needs_snapshot_refresh);
        assert!(lock_unpoisoned(&shared).free_toast.is_some());
        assert_eq!(
            lock_unpoisoned(&shared).free_selection_anchor.as_deref(),
            Some(r"c:\users\demo\desktop\alps.txt")
        );

        // ClearSelection clears free_selected_paths AND cut_paths.
        lock_unpoisoned(&shared)
            .cut_paths
            .insert(r"c:\users\demo\desktop\alps.txt".into());
        let r_clr = handle_free_desktop_event(
            desklite_win32::FreeDesktopEvent::Hotkey(
                desklite_win32::FreeDesktopHotkey::ClearSelection,
            ),
            &shared,
        );
        assert!(r_clr.needs_snapshot_refresh);
        let g = lock_unpoisoned(&shared);
        assert!(g.free_selected_paths.is_empty());
        assert!(g.cut_paths.is_empty());
    }

    #[test]
    fn free_desktop_dpi_scaling_rescale_and_paste_undo_restores_added_desktop_items() {
        let dir = std::env::temp_dir().join(format!(
            "desklite_stage2_undo_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("note.txt");
        std::fs::write(&file_path, "hi").unwrap();
        let file_str = file_path.to_string_lossy().into_owned();
        let dir_str = dir.to_string_lossy().into_owned();

        let bucket = FenceSpec {
            id: 101,
            name: "Bucket".into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(20, 20, 320, 320),
            collapsed: false,
            locked: false,
            role: Some(FenceRole::DesktopBucket),
            portal_dir: None,
            backdrop_alpha: 115,
            layout_override: None,
            items: vec![resolve_dropped_item(&file_path, 0)],
        };
        let shared = test_shared(vec![bucket]);
        {
            let mut g = lock_unpoisoned(&shared);
            g.set_desktop_dirs(vec![dir_str.clone()]);
            g.topology = DisplayTopology::try_from_monitors(vec![MonitorDescriptor {
                device_key: "DISPLAY1".into(),
                friendly_name: "Primary".into(),
                rect: Rect::new(0, 0, 1920, 1080),
                work_area: Rect::new(0, 0, 1920, 1032),
                dpi_x: 144,
                dpi_y: 144,
                is_primary: true,
            }])
            .unwrap();
            g.config.preferences.icon_cell_w = 102;
            g.config.preferences.icon_cell_h = 144;
            assert_eq!(g.free_desktop_cell_size(), (153, 216));

            let snap_key = g.config.active_topology.clone().unwrap();
            g.config.snapshots.get_mut(&snap_key).unwrap().free_icons = vec![
                DesktopIconPos {
                    id: file_str.clone(),
                    display_name: "note.txt".into(),
                    pos: Point { x: 20, y: 20 },
                },
                DesktopIconPos {
                    id: dir.join("second.txt").to_string_lossy().into_owned(),
                    display_name: "second.txt".into(),
                    pos: Point { x: 1830, y: 20 },
                },
            ];
            let new_cell = g.free_desktop_cell_size();
            g.clamp_free_icons_for_cell_size(new_cell);
            let icons = g.config.active_free_icons();
            // Interior icon anchor must not drift along X/Y when global cell size changes.
            assert_eq!(icons[0].pos, Point { x: 20, y: 20 });
            // Edge-overflow icon clamps cleanly back inside primary monitor work_area.
            assert!(icons[1].pos.x + new_cell.0 <= 1920);
            g.mark_free_desktop_dirty(None);
            assert!(g.pending_repaint_free_desktop);

            g.push_undo(UndoAction::PasteFiles {
                fence_id: Some(101),
                created_paths: Vec::new(),
                moved_pairs: Vec::new(),
                removed_from_fences: Vec::new(),
                added_paths: vec![file_str.clone()],
                is_portal: false,
            });
            // Remove from free_icons to mirror assign_desktop_items_to_bucket state.
            let _ = g.config.remove_free_icon(&file_str);
        }

        let ctx = egui::Context::default();
        let mut out = FenceUiOutput::default();
        crate::undo::execute_undo(Some(101), &shared, &ctx, &mut out);
        {
            let g = lock_unpoisoned(&shared);
            assert!(g.config.fences[0].items.is_empty());
            assert!(
                g.config
                    .active_free_icons()
                    .iter()
                    .any(|ic| normalize_path_key(&ic.id) == normalize_path_key(&file_str)),
                "undo of DesktopBucket PasteFiles should restore added_paths to free_icons"
            );
            assert!(g.free_icon_epoch >= 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
