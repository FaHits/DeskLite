use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    FenceItem, FenceKind, FenceRole, FenceSpec, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH,
    normalize_path_key,
};
use desklite_win32::{
    AsyncDragRequest, Win32Hotkey, all_bucket_fence_hwnds, allow_foreground_self,
    clipboard_has_files, copy_text_to_clipboard, delete_files_permanently,
    free_desktop_hwnd as win32_free_desktop_hwnd, is_ctrl_key_down, is_directly_on_desktop,
    launch_as_admin, launch_item, paste_files_to_directory_detailed, read_clipboard_files,
    rename_file_in_place, resolve_dropped_item, reveal_in_explorer, scan_portal_directory,
    set_clipboard_files_for_window, set_file_drag_active, set_hwnd_locked, show_file_properties,
    start_file_drag_out_async, trash_files, user_desktop_dir,
};
use egui::{Key, Vec2, ViewportCommand};

use crate::inspector::open_param_editor_for_fence;
use crate::item_grid::ItemUiAction;
use crate::state::{
    AsyncDragSession, COLLAPSED_HEIGHT, CollapseAnimPhase, CollapseAnimState,
    CollapsedMenuCommand, DragSourceState, FenceCommand, FenceItemMenuCommand, FenceUiOutput,
    ItemRenamePrompt, RenamePrompt, ReorderDragState, SelectionScope, SharedFenceState,
    UndoAction, duplicate_fence_with_style, lock_unpoisoned,
};
use crate::undo::execute_undo;

pub fn dispatch_win32_hotkey(
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
                let mut guard = lock_unpoisoned(shared);
                guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
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
pub fn handle_keyboard_shortcuts(
    fence_id: i32,
    spec: &FenceSpec,
    grid_cols: usize,
    selected_set: &HashSet<usize>,
    skip_ctrl_actions: bool,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out_action: &mut Option<ItemUiAction>,
) {
    let desktop_dirs = { lock_unpoisoned(shared).desktop_dirs.clone() };
    let role = spec.effective_role(&desktop_dirs);
    let is_bucket = matches!(role, FenceRole::DesktopBucket);
    let is_refreshable = !is_bucket;
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
    } else if ctx.input(|i| i.key_pressed(Key::F5)) && is_refreshable {
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
        let mut guard = lock_unpoisoned(shared);
        guard.selected_items.remove(&fence_id);
        guard.selection_anchor.remove(&fence_id);
        guard.cut_paths.clear();
    } else if key_enter && !selected_set.is_empty() {
        *out_action = Some(ItemUiAction::LaunchSelected);
    } else if shift_del && !selected_set.is_empty() {
        *out_action = Some(ItemUiAction::DeleteSelectedPermanent);
    } else if key_del && !selected_set.is_empty() {
        if is_bucket {
            *out_action = Some(ItemUiAction::RemoveSelected);
        } else {
            *out_action = Some(ItemUiAction::TrashSelected);
        }
    } else if let Some(delta) = nav_delta
        && !spec.items.is_empty()
    {
        let len = spec.items.len();
        let mut guard = lock_unpoisoned(shared);
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
        let mut guard = lock_unpoisoned(shared);
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

pub fn sorted_selected_paths(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> Vec<String> {
    let mut sorted: Vec<usize> = {
        let guard = lock_unpoisoned(shared);
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

fn update_selection_for_drag_start(
    guard: &mut SharedFenceState,
    fence_id: i32,
    trigger_idx: usize,
    ctrl: bool,
    shift: bool,
) -> Vec<usize> {
    guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
    let cur_set = guard
        .selected_items
        .get(&fence_id)
        .cloned()
        .unwrap_or_default();
    let mut indices: Vec<usize> = if cur_set.contains(&trigger_idx) {
        cur_set.into_iter().collect()
    } else if shift {
        let anchor = guard.selection_anchor.get(&fence_id).copied().unwrap_or(0);
        let set = guard.selected_items.entry(fence_id).or_default();
        if !ctrl {
            set.clear();
        }
        for i in anchor.min(trigger_idx)..=anchor.max(trigger_idx) {
            set.insert(i);
        }
        set.iter().copied().collect()
    } else if ctrl {
        let set = guard.selected_items.entry(fence_id).or_default();
        set.insert(trigger_idx);
        guard.selection_anchor.insert(fence_id, trigger_idx);
        set.iter().copied().collect()
    } else {
        let set = guard.selected_items.entry(fence_id).or_default();
        set.clear();
        set.insert(trigger_idx);
        guard.selection_anchor.insert(fence_id, trigger_idx);
        vec![trigger_idx]
    };
    indices.sort_unstable();
    indices
}

pub fn start_async_drag_from_paths(
    source_fence_id: Option<i32>,
    drag_paths: Vec<String>,
    shared: &Arc<Mutex<SharedFenceState>>,
) {
    if drag_paths.is_empty() {
        return;
    }
    let is_desktop_resident = drag_paths
        .iter()
        .any(|p| is_directly_on_desktop(Path::new(p)));
    let (bucket_hwnds, free_hwnd, notify_hwnd, notify_msg) = {
        let mut guard = lock_unpoisoned(shared);
        if guard.active_async_drag.is_some() {
            return;
        }
        let mut hwnds = all_bucket_fence_hwnds();
        if hwnds.is_empty() {
            hwnds = guard.attached_hwnds.values().copied().collect();
        }
        let free_hwnd = if guard.free_desktop_hwnd != 0 {
            guard.free_desktop_hwnd
        } else {
            win32_free_desktop_hwnd()
        };
        guard.dragging_from_fence = Some(DragSourceState {
            fence_id: source_fence_id,
            paths: drag_paths.clone(),
            is_desktop_resident,
            finished_at: None,
        });
        (
            hwnds,
            free_hwnd,
            guard.drag_notify_hwnd,
            guard.drag_notify_msg,
        )
    };
    set_file_drag_active(true);
    let rx = start_file_drag_out_async(
        AsyncDragRequest {
            source_fence_id,
            paths: drag_paths.clone(),
            is_desktop_resident,
            bucket_hwnds,
            free_desktop_hwnd: free_hwnd,
        },
        notify_hwnd,
        notify_msg,
    );
    let mut guard = lock_unpoisoned(shared);
    guard.active_async_drag = Some(AsyncDragSession {
        source_fence_id,
        paths: drag_paths,
        is_desktop_resident,
        rx,
    });
}

#[allow(clippy::too_many_arguments)]
pub fn execute_item_action(
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
            let mut guard = lock_unpoisoned(shared);
            guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
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
                if let Some(it) = spec.items.get(idx)
                    && let Some(dispatcher) = guard.menu_dispatcher.clone()
                {
                    dispatcher(FenceCommand::PrefetchShellMenu {
                        paths: vec![it.path.clone()],
                        owner_hwnd: my_hwnd,
                    });
                }
            }
            ctx.request_repaint();
        }
        ItemUiAction::OpenContextMenu {
            idx,
            screen_x,
            screen_y,
        } => {
            let mut guard = lock_unpoisoned(shared);
            guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
            let sorted: Vec<usize> = {
                let mut inserted = false;
                let mut list: Vec<usize> = {
                    let set = guard.selected_items.entry(fence_id).or_default();
                    if !set.contains(&idx) {
                        set.clear();
                        set.insert(idx);
                        inserted = true;
                    }
                    set.iter().copied().collect()
                };
                if inserted {
                    guard.selection_anchor.insert(fence_id, idx);
                }
                list.sort_unstable();
                list
            };
            let paths: Vec<String> = sorted
                .into_iter()
                .filter_map(|i| spec.items.get(i).map(|it| it.path.clone()))
                .collect();
            if !paths.is_empty() {
                let is_bucket = guard
                    .config
                    .fences
                    .iter()
                    .find(|f| f.id == fence_id)
                    .is_some_and(|f| f.is_desktop_bucket(&guard.desktop_dirs));
                allow_foreground_self();
                guard.push_command(
                    ctx,
                    FenceCommand::ShowFenceItemMenu {
                        fence_id,
                        paths,
                        is_bucket,
                        screen_x,
                        screen_y,
                    },
                );
            }
        }
        ItemUiAction::Launch(path) => {
            if let Err(e) = launch_item(&path) {
                lock_unpoisoned(shared)
                    .toast_msg
                    .insert(fence_id, (format!("打开失败: {e}"), Instant::now()));
                ctx.request_repaint();
            }
        }
        ItemUiAction::LaunchAsAdmin(path) => {
            if let Err(e) = launch_as_admin(&path) {
                lock_unpoisoned(shared)
                    .toast_msg
                    .insert(fence_id, (format!("提权运行失败: {e}"), Instant::now()));
                ctx.request_repaint();
            }
        }
        ItemUiAction::LaunchSelected => {
            let mut sorted: Vec<usize> = selected_set.iter().copied().collect();
            sorted.sort_unstable();
            for idx in sorted.into_iter().take(8) {
                if let Some(it) = spec.items.get(idx)
                    && let Err(e) = launch_item(&it.path)
                {
                    lock_unpoisoned(shared)
                        .toast_msg
                        .insert(fence_id, (format!("打开失败: {e}"), Instant::now()));
                    ctx.request_repaint();
                    break;
                }
            }
        }
        ItemUiAction::CopySelected { cut } => {
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if !paths.is_empty() {
                if set_clipboard_files_for_window(my_hwnd, &paths, cut).is_ok() {
                    let mut guard = lock_unpoisoned(shared);
                    guard.cut_paths.clear();
                    if cut {
                        for p in &paths {
                            guard.cut_paths.insert(normalize_path_key(p));
                        }
                    }
                    let verb = if cut { "已剪切" } else { "已复制" };
                    guard.toast_msg.insert(
                        fence_id,
                        (format!("{verb} {} 项", paths.len()), Instant::now()),
                    );
                    ctx.request_repaint();
                } else {
                    lock_unpoisoned(shared)
                        .toast_msg
                        .insert(fence_id, ("剪贴板被占用，请重试".to_string(), Instant::now()));
                    ctx.request_repaint();
                }
            }
        }
        ItemUiAction::CopyPaths => {
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if !paths.is_empty() && copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                lock_unpoisoned(shared)
                    .toast_msg
                    .insert(fence_id, ("已复制路径".to_string(), Instant::now()));
                ctx.request_repaint();
            }
        }
        ItemUiAction::PasteFromClipboard => {
            handle_paste_action(fence_id, spec, shared, ctx, out);
        }
        ItemUiAction::UndoLast => {
            execute_undo(Some(fence_id), shared, ctx, out);
        }
        ItemUiAction::BeginRename(idx) => {
            if let Some(it) = spec.items.get(idx) {
                let file_name = Path::new(&it.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| it.display_name.clone());
                lock_unpoisoned(shared).item_rename_prompt = Some(ItemRenamePrompt {
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
            {
                let mut guard = lock_unpoisoned(shared);
                let desktop_dirs = guard.desktop_dirs.clone();
                let role = guard
                    .config
                    .fences
                    .iter()
                    .find(|f| f.id == fence_id)
                    .map(|f| f.effective_role(&desktop_dirs));
                match role {
                    Some(FenceRole::ExternalPortal { dir }) => {
                        if let Some(f) = guard.config.fence_mut(fence_id) {
                            f.items = scan_portal_directory(Path::new(&dir));
                        }
                    }
                    Some(FenceRole::UnfiledDesktop) => {
                        guard.refresh_all_unfiled_from_disk_if_present();
                    }
                    _ => {}
                }
            }
            ctx.request_repaint();
        }
        ItemUiAction::RemoveSelected => {
            let removed_any = {
                let mut guard = lock_unpoisoned(shared);
                let desktop_dirs = guard.desktop_dirs.clone();
                let to_remove = guard
                    .selected_items
                    .get(&fence_id)
                    .cloned()
                    .unwrap_or_default();
                let mut removed_paths = Vec::new();
                let mut removed_items = Vec::new();
                if !to_remove.is_empty()
                    && let Some(f) = guard.config.fence_mut(fence_id)
                    && f.is_desktop_bucket(&desktop_dirs)
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
                        guard.place_released_icons_on_free_desktop(&removed_paths, None);
                        guard.push_undo(UndoAction::RemoveItems {
                            fence_id,
                            items: removed_items,
                        });
                    }
                    guard.refresh_all_unfiled_from_disk_if_present();
                    guard.dirty_layout = true;
                    out.needs_immediate_save = true;
                    !removed_paths.is_empty()
                } else {
                    false
                }
            };
            if removed_any {
                let mut guard = lock_unpoisoned(shared);
                guard.request_repaint_fence(ctx, fence_id);
                guard.mark_free_desktop_dirty(Some(ctx));
            }
        }
        ItemUiAction::TrashSelected | ItemUiAction::DeleteSelectedPermanent => {
            let permanent = matches!(act, ItemUiAction::DeleteSelectedPermanent);
            let paths = sorted_selected_paths(fence_id, spec, shared);
            if paths.is_empty() {
                return;
            }
            let res = if permanent {
                delete_files_permanently(&paths)
            } else {
                trash_files(&paths)
            };
            match res {
                Ok(true) => {
                    let mut guard = lock_unpoisoned(shared);
                    let desktop_dirs = guard.desktop_dirs.clone();
                    for p in &paths {
                        guard.invalidate_icon_texture(p);
                        let _ = guard.config.remove_desktop_path_everywhere(p, &desktop_dirs);
                    }
                    if let Some(f) = guard.config.fence_mut(fence_id)
                        && let Some(dir) = f.external_portal_dir(&desktop_dirs)
                    {
                        f.items = scan_portal_directory(Path::new(&dir));
                    }
                    guard.refresh_all_unfiled_from_disk_if_present();
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                    guard.dirty_layout = true;
                    out.needs_immediate_save = true;
                    guard.request_repaint_fence(ctx, fence_id);
                    guard.bump_free_icon_epoch();
                }
                Ok(false) => {}
                Err(e) => {
                    lock_unpoisoned(shared)
                        .toast_msg
                        .insert(fence_id, (format!("删除失败: {e}"), Instant::now()));
                    ctx.request_repaint();
                }
            }
        }
        ItemUiAction::BeginItemDrag { idx, ctrl, shift } => {
            let mut guard = lock_unpoisoned(shared);
            let indices = update_selection_for_drag_start(&mut guard, fence_id, idx, ctrl, shift);
            let paths: Vec<String> = indices
                .iter()
                .filter_map(|&i| spec.items.get(i).map(|it| it.path.clone()))
                .collect();
            if !paths.is_empty() {
                guard.item_reorder.insert(
                    fence_id,
                    ReorderDragState {
                        trigger_idx: idx,
                        indices,
                        paths,
                    },
                );
            }
        }
        ItemUiAction::StartDragOut { idx, ctrl, shift } => {
            let drag_paths: Vec<String> = {
                let mut guard = lock_unpoisoned(shared);
                let indices =
                    update_selection_for_drag_start(&mut guard, fence_id, idx, ctrl, shift);
                indices
                    .into_iter()
                    .filter_map(|i| spec.items.get(i).map(|it| it.path.clone()))
                    .collect()
            };
            start_async_drag_from_paths(Some(fence_id), drag_paths, shared);
        }
    }
}

pub fn apply_fence_item_menu_command(
    fence_id: i32,
    paths: &[String],
    cmd: FenceItemMenuCommand,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> bool {
    if paths.is_empty() {
        return false;
    }
    let mut needs_immediate_save = false;
    match cmd {
        FenceItemMenuCommand::Open => {
            for p in paths.iter().take(8) {
                if let Err(e) = launch_item(p) {
                    let mut guard = lock_unpoisoned(shared);
                    guard
                        .toast_msg
                        .insert(fence_id, (format!("打开失败: {e}"), Instant::now()));
                    guard.request_repaint_fence(ctx, fence_id);
                    break;
                }
            }
        }
        FenceItemMenuCommand::OpenAsAdmin => {
            if let Some(first) = paths.first()
                && let Err(e) = launch_as_admin(first)
            {
                let mut guard = lock_unpoisoned(shared);
                guard
                    .toast_msg
                    .insert(fence_id, (format!("提权运行失败: {e}"), Instant::now()));
                guard.request_repaint_fence(ctx, fence_id);
            }
        }
        FenceItemMenuCommand::RevealInExplorer => {
            if let Some(first) = paths.first() {
                let _ = reveal_in_explorer(first);
            }
        }
        FenceItemMenuCommand::Copy { cut } => {
            let hwnd = {
                let guard = lock_unpoisoned(shared);
                guard
                    .attached_hwnds
                    .get(&fence_id)
                    .copied()
                    .unwrap_or(guard.free_desktop_hwnd)
            };
            if set_clipboard_files_for_window(hwnd, paths, cut).is_ok() {
                let mut guard = lock_unpoisoned(shared);
                guard.cut_paths.clear();
                if cut {
                    for p in paths {
                        guard.cut_paths.insert(normalize_path_key(p));
                    }
                }
                let verb = if cut { "已剪切" } else { "已复制" };
                guard.toast_msg.insert(
                    fence_id,
                    (format!("{verb} {} 项", paths.len()), Instant::now()),
                );
                guard.request_repaint_fence(ctx, fence_id);
                guard.mark_free_desktop_dirty(Some(ctx));
            } else {
                let mut guard = lock_unpoisoned(shared);
                guard
                    .toast_msg
                    .insert(fence_id, ("剪贴板被占用，请重试".to_string(), Instant::now()));
                guard.request_repaint_fence(ctx, fence_id);
            }
        }
        FenceItemMenuCommand::CopyPaths => {
            if copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                let mut guard = lock_unpoisoned(shared);
                guard
                    .toast_msg
                    .insert(fence_id, ("已复制路径".to_string(), Instant::now()));
                guard.request_repaint_fence(ctx, fence_id);
            }
        }
        FenceItemMenuCommand::Rename => {
            if let Some(first) = paths.first() {
                let target_key = normalize_path_key(first);
                let mut guard = lock_unpoisoned(shared);
                if let Some(f) = guard.config.fences.iter().find(|f| f.id == fence_id)
                    && let Some((idx, it)) = f
                        .items
                        .iter()
                        .enumerate()
                        .find(|(_, it)| normalize_path_key(&it.path) == target_key)
                {
                    let file_name = Path::new(&it.path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| it.display_name.clone());
                    guard.item_rename_prompt = Some(ItemRenamePrompt {
                        fence_id,
                        item_idx: idx,
                        original_path: it.path.clone(),
                        draft: file_name,
                        needs_focus: true,
                    });
                    guard.request_repaint_fence(ctx, fence_id);
                }
            }
        }
        FenceItemMenuCommand::RemoveFromFence => {
            let path_keys: HashSet<String> = paths.iter().map(|p| normalize_path_key(p)).collect();
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            let mut removed_paths = Vec::new();
            let mut removed_items = Vec::new();
            if let Some(f) = guard.config.fence_mut(fence_id)
                && f.is_desktop_bucket(&desktop_dirs)
            {
                let mut kept = Vec::with_capacity(f.items.len());
                for it in f.items.drain(..) {
                    if path_keys.contains(&normalize_path_key(&it.path)) {
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
                    guard.place_released_icons_on_free_desktop(&removed_paths, None);
                    guard.push_undo(UndoAction::RemoveItems {
                        fence_id,
                        items: removed_items,
                    });
                    guard.refresh_all_unfiled_from_disk_if_present();
                    guard.dirty_layout = true;
                    guard.bump_free_icon_epoch();
                    guard.request_repaint_fence(ctx, fence_id);
                    guard.mark_free_desktop_dirty(Some(ctx));
                    needs_immediate_save = true;
                }
            }
        }
        FenceItemMenuCommand::MoveToFence(target_fid) => {
            let path_keys: HashSet<String> = paths.iter().map(|p| normalize_path_key(p)).collect();
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            let valid = fence_id != target_fid
                && guard.config.fences.iter().any(|f| {
                    f.id == fence_id && f.effective_role(&desktop_dirs) == FenceRole::DesktopBucket
                })
                && guard.config.fences.iter().any(|f| {
                    f.id == target_fid
                        && f.effective_role(&desktop_dirs) == FenceRole::DesktopBucket
                });
            if valid {
                let mut moved_items = Vec::new();
                if let Some(src) = guard.config.fence_mut(fence_id) {
                    let mut kept = Vec::with_capacity(src.items.len());
                    for it in src.items.drain(..) {
                        if path_keys.contains(&normalize_path_key(&it.path)) {
                            moved_items.push(it);
                        } else {
                            kept.push(it);
                        }
                    }
                    for (i, it) in kept.iter_mut().enumerate() {
                        it.order = i as i32;
                    }
                    src.items = kept;
                }
                if !moved_items.is_empty() {
                    if let Some(dst) = guard.config.fence_mut(target_fid) {
                        let base = dst.items.len() as i32;
                        for (i, mut it) in moved_items.iter().cloned().enumerate() {
                            it.order = base + i as i32;
                            dst.items.push(it);
                        }
                    }
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                    guard.push_undo(UndoAction::MoveBetweenBuckets {
                        src_fence_id: fence_id,
                        dst_fence_id: target_fid,
                        items: moved_items,
                    });
                    guard.refresh_all_unfiled_from_disk_if_present();
                    guard.dirty_layout = true;
                    guard.request_repaint_fence(ctx, fence_id);
                    guard.request_repaint_fence(ctx, target_fid);
                    needs_immediate_save = true;
                }
            }
        }
        FenceItemMenuCommand::Trash => match trash_files(paths) {
            Ok(true) => {
                let mut guard = lock_unpoisoned(shared);
                let desktop_dirs = guard.desktop_dirs.clone();
                for p in paths {
                    guard.invalidate_icon_texture(p);
                    let _ = guard.config.remove_desktop_path_everywhere(p, &desktop_dirs);
                }
                if let Some(f) = guard.config.fence_mut(fence_id)
                    && let Some(dir) = f.external_portal_dir(&desktop_dirs)
                {
                    f.items = scan_portal_directory(Path::new(&dir));
                }
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.dirty_layout = true;
                guard.bump_free_icon_epoch();
                guard.request_repaint_fence(ctx, fence_id);
                guard.mark_free_desktop_dirty(Some(ctx));
                needs_immediate_save = true;
            }
            Ok(false) => {}
            Err(e) => {
                let mut guard = lock_unpoisoned(shared);
                guard
                    .toast_msg
                    .insert(fence_id, (format!("删除失败: {e}"), Instant::now()));
                guard.request_repaint_fence(ctx, fence_id);
            }
        },
        FenceItemMenuCommand::Properties => {
            if let Some(first) = paths.first() {
                let _ = show_file_properties(first);
            }
        }
    }
    needs_immediate_save
}

pub fn handle_paste_action(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let Some((sources, is_cut)) = read_clipboard_files() else {
        if clipboard_has_files() {
            lock_unpoisoned(shared)
                .toast_msg
                .insert(fence_id, ("剪贴板正忙，请重试".to_string(), Instant::now()));
            ctx.request_repaint();
        }
        return;
    };
    if sources.is_empty() {
        return;
    }
    let desktop_dirs = { lock_unpoisoned(shared).desktop_dirs.clone() };
    let target_role = spec.effective_role(&desktop_dirs);

    match target_role {
        FenceRole::ExternalPortal { dir } => {
            if let Ok(pairs) = paste_files_to_directory_detailed(&sources, Path::new(&dir), is_cut)
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
                let mut guard = lock_unpoisoned(shared);
                let mut removed_from_fences = Vec::new();
                if is_cut {
                    for f in &mut guard.config.fences {
                        if f.id != fence_id && f.is_desktop_bucket(&desktop_dirs) {
                            let mut removed = Vec::new();
                            let mut kept = Vec::with_capacity(f.items.len());
                            for it in f.items.drain(..) {
                                if sources
                                    .iter()
                                    .any(|s| normalize_path_key(&it.path) == normalize_path_key(s))
                                {
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
                    for s in &sources {
                        let _ = guard.config.remove_free_icon(s);
                    }
                    guard.cut_paths.clear();
                }
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.items = scan_portal_directory(Path::new(&dir));
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
                guard
                    .toast_msg
                    .insert(fence_id, (format!("已粘贴 {} 项", pairs.len()), Instant::now()));
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
                ctx.request_repaint();
                guard.mark_free_desktop_dirty(Some(ctx));
            }
        }
        FenceRole::UnfiledDesktop => {
            let mut created_paths = Vec::new();
            let mut moved_pairs = Vec::new();
            let mut released_paths = Vec::new();
            if let Some(desktop) = user_desktop_dir() {
                for s in &sources {
                    let sp = Path::new(s);
                    if is_directly_on_desktop(sp) {
                        if is_cut {
                            released_paths.push(s.clone());
                        } else if let Ok(pairs) = paste_files_to_directory_detailed(
                            std::slice::from_ref(s),
                            &desktop,
                            false,
                        ) {
                            for (_, dst) in pairs {
                                created_paths.push(dst.to_string_lossy().into_owned());
                            }
                        }
                    } else if let Ok(pairs) =
                        paste_files_to_directory_detailed(std::slice::from_ref(s), &desktop, is_cut)
                    {
                        for (src, dst) in pairs {
                            let ds = dst.to_string_lossy().into_owned();
                            if is_cut {
                                moved_pairs.push((src.to_string_lossy().into_owned(), ds.clone()));
                            } else {
                                created_paths.push(ds.clone());
                            }
                            released_paths.push(ds);
                        }
                    }
                }
            }
            if !released_paths.is_empty() || !created_paths.is_empty() {
                let mut guard = lock_unpoisoned(shared);
                let mut removed_from_fences = Vec::new();
                if is_cut {
                    for f in &mut guard.config.fences {
                        if f.is_desktop_bucket(&desktop_dirs) {
                            let mut rem = Vec::new();
                            let mut kept = Vec::with_capacity(f.items.len());
                            for it in f.items.drain(..) {
                                if released_paths.iter().any(|p| {
                                    normalize_path_key(&it.path) == normalize_path_key(p)
                                }) {
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
                            }
                            f.items = kept;
                        }
                    }
                    guard.cut_paths.clear();
                }
                let place_list: Vec<String> = released_paths
                    .iter()
                    .chain(created_paths.iter())
                    .cloned()
                    .collect();
                guard.place_released_icons_on_free_desktop(&place_list, None);
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.push_undo(UndoAction::PasteFiles {
                    fence_id: Some(fence_id),
                    created_paths,
                    moved_pairs,
                    removed_from_fences,
                    added_paths: Vec::new(),
                    is_portal: false,
                });
                guard.toast_msg.insert(
                    fence_id,
                    (format!("已粘贴 {} 项", place_list.len()), Instant::now()),
                );
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
                ctx.request_repaint();
                guard.mark_free_desktop_dirty(Some(ctx));
            }
        }
        FenceRole::DesktopBucket => {
            let mut created_paths = Vec::new();
            let mut moved_pairs = Vec::new();
            let mut target_paths = Vec::new();
            let owner_snapshot: Vec<(String, Option<i32>)> = {
                let guard = lock_unpoisoned(shared);
                sources
                    .iter()
                    .map(|s| (s.clone(), guard.config.desktop_owner_of(s, &desktop_dirs)))
                    .collect()
            };
            let desktop_opt = user_desktop_dir();
            for (s, owner) in owner_snapshot {
                let sp = Path::new(&s);
                if is_directly_on_desktop(sp) {
                    if is_cut {
                        // Cut from FreeDesktop / UnfiledDesktop / another DesktopBucket -> pure ownership transfer!
                        target_paths.push(s);
                    } else if owner.is_none() {
                        // Bug 3a fix: Copying an unfiled desktop item (from UnfiledDesktop or FreeDesktop)
                        // into a DesktopBucket assigns ownership directly without cloning `xxx (2)` on Desktop!
                        target_paths.push(s);
                    } else if let Some(ref desktop) = desktop_opt
                        && let Ok(pairs) = paste_files_to_directory_detailed(
                            std::slice::from_ref(&s),
                            desktop,
                            false,
                        )
                    {
                        for (_, dst) in pairs {
                            let ds = dst.to_string_lossy().into_owned();
                            created_paths.push(ds.clone());
                            target_paths.push(ds);
                        }
                    }
                } else if let Some(ref desktop) = desktop_opt
                    && let Ok(pairs) =
                        paste_files_to_directory_detailed(std::slice::from_ref(&s), desktop, is_cut)
                {
                    for (src, dst) in pairs {
                        let ds = dst.to_string_lossy().into_owned();
                        if is_cut {
                            moved_pairs.push((src.to_string_lossy().into_owned(), ds.clone()));
                        } else {
                            created_paths.push(ds.clone());
                        }
                        target_paths.push(ds);
                    }
                }
            }

            if target_paths.is_empty() {
                return;
            }
            let mut guard = lock_unpoisoned(shared);
            let mut removed_from_fences = Vec::new();
            let mut portal_fids_to_refresh = Vec::new();
            if is_cut {
                for f in &mut guard.config.fences {
                    if f.id != fence_id && f.is_desktop_bucket(&desktop_dirs) {
                        let rem: Vec<FenceItem> = f
                            .items
                            .iter()
                            .filter(|it| {
                                sources.iter().any(|s| {
                                    normalize_path_key(&it.path) == normalize_path_key(s)
                                })
                            })
                            .cloned()
                            .collect();
                        if !rem.is_empty() {
                            removed_from_fences.push((f.id, rem));
                        }
                    } else if f.id != fence_id
                        && let Some(dir) = f.external_portal_dir(&desktop_dirs)
                    {
                        let dir_key = normalize_path_key(&dir);
                        let touched = moved_pairs.iter().any(|(src, _)| {
                            Path::new(src).parent().is_some_and(|p| {
                                normalize_path_key(&p.to_string_lossy()) == dir_key
                            })
                        });
                        if touched {
                            f.items = scan_portal_directory(Path::new(&dir));
                            portal_fids_to_refresh.push(f.id);
                        }
                    }
                }
                guard.cut_paths.clear();
            }
            for pfid in portal_fids_to_refresh {
                guard.request_repaint_fence(ctx, pfid);
            }
            let items: Vec<FenceItem> = target_paths
                .iter()
                .enumerate()
                .map(|(i, p)| resolve_dropped_item(Path::new(p), i as i32))
                .collect();
            let added_paths: Vec<String> = items.iter().map(|it| it.path.clone()).collect();
            if guard
                .config
                .assign_desktop_items_to_bucket(fence_id, items, &desktop_dirs)
            {
                let count = added_paths.len();
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.push_undo(UndoAction::PasteFiles {
                    fence_id: Some(fence_id),
                    created_paths,
                    moved_pairs,
                    removed_from_fences,
                    added_paths,
                    is_portal: false,
                });
                guard
                    .toast_msg
                    .insert(fence_id, (format!("已粘贴 {count} 项"), Instant::now()));
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
                guard.mark_free_desktop_dirty(Some(ctx));
            }
            ctx.request_repaint();
        }
    }
}

pub fn commit_item_rename(
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
        lock_unpoisoned(shared).item_rename_prompt = None;
        return;
    }
    match rename_file_in_place(old_p, trimmed) {
        Ok(mut updated) => {
            let new_path = updated.path.clone();
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            guard.invalidate_icon_texture(old_path);
            guard.invalidate_icon_texture(&new_path);
            if let Some(f) = guard.config.fence_mut(fence_id) {
                if let Some(dir) = f.external_portal_dir(&desktop_dirs) {
                    f.items = scan_portal_directory(Path::new(&dir));
                } else if idx < f.items.len() {
                    updated.order = idx as i32;
                    f.items[idx] = updated;
                }
            }
            guard.refresh_all_unfiled_from_disk_if_present();
            guard.push_undo(UndoAction::RenameItem {
                fence_id: Some(fence_id),
                old_path: old_path.to_string(),
                new_path,
            });
            guard.item_rename_prompt = None;
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            ctx.request_repaint();
            guard.mark_free_desktop_dirty(Some(ctx));
        }
        Err(e) => {
            let mut guard = lock_unpoisoned(shared);
            guard.item_rename_prompt = None;
            guard
                .toast_msg
                .insert(fence_id, (format!("重命名失败: {e}"), Instant::now()));
            ctx.request_repaint();
        }
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

pub fn toggle_collapse(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let new_collapsed = !spec.collapsed;
    let ppp = ctx.pixels_per_point().max(0.5);
    let collapsed_phys_h = (COLLAPSED_HEIGHT * ppp).round() as i32;
    let expanded_phys_h = spec.rect.height().max(MIN_FENCE_HEIGHT);
    let to_phys_h = if new_collapsed {
        collapsed_phys_h
    } else {
        expanded_phys_h
    };
    let target_w = (spec.rect.width().max(MIN_FENCE_WIDTH) as f32) / ppp;
    let target_h = (to_phys_h as f32) / ppp;

    let mut guard = lock_unpoisoned(shared);
    let my_hwnd = guard.attached_hwnds.get(&fence_id).copied().unwrap_or(0);
    let cur_w_phys = spec.rect.width().max(MIN_FENCE_WIDTH);
    let (from_phys_h, existing_surface_h) =
        if let Some(prev_anim) = guard.collapse_anims.get(&fence_id) {
            let h = match prev_anim.phase {
                CollapseAnimPhase::WaitExpandSurface => prev_anim.from_phys_h,
                CollapseAnimPhase::WaitCollapseShrink => prev_anim.to_phys_h,
                CollapseAnimPhase::Animating => {
                    let raw_t = prev_anim.started_at.elapsed().as_secs_f32()
                        / (prev_anim.duration_ms.max(1) as f32 / 1000.0);
                    let eased = ease_out_cubic(raw_t);
                    (prev_anim.from_phys_h as f32
                        + (prev_anim.to_phys_h - prev_anim.from_phys_h) as f32 * eased)
                        .round() as i32
                }
            };
            (h, prev_anim.surface_phys_h.max(h))
        } else if my_hwnd != 0
            && let Some((_, t, _, b)) = desklite_win32::window_outer_rect(my_hwnd)
        {
            let h = (b - t).max(collapsed_phys_h);
            (h, h)
        } else if spec.collapsed {
            (collapsed_phys_h, collapsed_phys_h)
        } else {
            (expanded_phys_h, expanded_phys_h)
        };

    if my_hwnd != 0 {
        let surface_phys_h = existing_surface_h.max(to_phys_h).max(from_phys_h);
        let phase = if !new_collapsed && surface_phys_h > existing_surface_h + 2 {
            // Clip first so growing the HWND to `surface_phys_h` never exposes an unrendered slab.
            desklite_win32::set_window_clip_rect(my_hwnd, cur_w_phys, from_phys_h);
            desklite_win32::set_window_rect_no_z(
                my_hwnd,
                spec.rect.left,
                spec.rect.top,
                cur_w_phys,
                surface_phys_h,
            );
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(
                target_w,
                surface_phys_h as f32 / ppp,
            )));
            CollapseAnimPhase::WaitExpandSurface
        } else {
            desklite_win32::set_window_clip_rect(my_hwnd, cur_w_phys, from_phys_h);
            CollapseAnimPhase::Animating
        };
        guard.collapse_anims.insert(
            fence_id,
            CollapseAnimState {
                started_at: Instant::now(),
                duration_ms: 165,
                from_phys_h,
                to_phys_h,
                surface_phys_h,
                target_collapsed: new_collapsed,
                phase,
            },
        );
    } else {
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(target_w, target_h)));
    }
    ctx.request_repaint();
    guard
        .pending_collapse_until
        .insert(fence_id, Instant::now() + Duration::from_millis(650));
    if let Some(f) = guard.config.fence_mut(fence_id) {
        f.collapsed = new_collapsed;
        guard.dirty_layout = true;
        out.needs_immediate_save = true;
    }
}

pub enum SortMode {
    Name,
    Kind,
    Modified,
}

pub fn sort_fence_items(
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    out: &mut FenceUiOutput,
    mode: SortMode,
) {
    let mut guard = lock_unpoisoned(shared);
    let desktop_dirs = guard.desktop_dirs.clone();
    let old_items = guard
        .config
        .fence_mut(fence_id)
        .filter(|f| f.is_desktop_bucket(&desktop_dirs))
        .map(|f| f.items.clone());
    let mut changed = false;
    if let Some(f) = guard.config.fence_mut(fence_id)
        && f.is_desktop_bucket(&desktop_dirs)
    {
        sort_items_slice(&mut f.items, mode);
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

pub fn sort_items_slice(items: &mut [FenceItem], mode: SortMode) {
    match mode {
        SortMode::Name => {
            items.sort_by(|a, b| {
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
            items.sort_by(|a, b| {
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
            items.sort_by(|a, b| {
                let mtime = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
                mtime(&b.path).cmp(&mtime(&a.path))
            });
        }
    }
    for (i, it) in items.iter_mut().enumerate() {
        it.order = i as i32;
    }
}

pub fn open_fence_menu(
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
    lock_unpoisoned(shared).push_command(
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
    let repaint_fence = |shared: &Arc<Mutex<SharedFenceState>>| {
        lock_unpoisoned(shared).request_repaint_fence(ctx, fence_id);
    };
    match cmd {
        CollapsedMenuCommand::ToggleCollapse => {
            let mut guard = lock_unpoisoned(shared);
            guard.pending_toggle_collapse.insert(fence_id);
            guard.request_repaint_fence(ctx, fence_id);
        }
        CollapsedMenuCommand::RenameFence => {
            let mut guard = lock_unpoisoned(shared);
            if let Some(name) = guard
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| f.name.clone())
            {
                guard.rename_prompt = Some(RenamePrompt {
                    fence_id,
                    draft: name,
                    needs_focus: true,
                });
                guard.request_repaint_fence(ctx, fence_id);
            }
        }
        CollapsedMenuCommand::ToggleKind => {
            let mut guard = lock_unpoisoned(shared);
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.kind = match f.kind {
                    FenceKind::FileList => FenceKind::IconGrid,
                    _ => FenceKind::FileList,
                };
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
            guard.request_repaint_fence(ctx, fence_id);
        }
        CollapsedMenuCommand::ToggleLock => {
            let mut guard = lock_unpoisoned(shared);
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
            guard.request_repaint_fence(ctx, fence_id);
        }
        CollapsedMenuCommand::DuplicateStyle => {
            if duplicate_fence_with_style(fence_id, shared, ctx).is_some() {
                let mut guard = lock_unpoisoned(shared);
                guard
                    .toast_msg
                    .insert(fence_id, ("已复制样式格子".to_string(), Instant::now()));
                guard.request_repaint_fence(ctx, fence_id);
                out.needs_immediate_save = true;
            } else {
                repaint_fence(shared);
            }
        }
        CollapsedMenuCommand::OpenParamsEditor => {
            open_param_editor_for_fence(fence_id, shared, ctx);
        }
        CollapsedMenuCommand::OpenPortalFolder => {
            let dir = {
                let guard = lock_unpoisoned(shared);
                let desktop_dirs = &guard.desktop_dirs;
                guard
                    .config
                    .fences
                    .iter()
                    .find(|f| f.id == fence_id)
                    .and_then(|f| f.external_portal_dir(desktop_dirs))
            };
            if let Some(dir) = dir {
                let _ = launch_item(&dir);
            }
        }
        CollapsedMenuCommand::UnbindPortal => {
            {
                let mut guard = lock_unpoisoned(shared);
                if let Some(f) = guard.config.fence_mut(fence_id) {
                    f.role = Some(FenceRole::DesktopBucket);
                    f.portal_dir = None;
                    f.items.clear();
                    guard.selected_items.remove(&fence_id);
                    guard.selection_anchor.remove(&fence_id);
                    guard.marquee_drag.remove(&fence_id);
                    guard.dirty_layout = true;
                    out.needs_immediate_save = true;
                }
                guard.push_command(ctx, FenceCommand::SyncPortals);
                guard.request_repaint_fence(ctx, fence_id);
            }
        }
        CollapsedMenuCommand::BindPortal => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestBindPortal(fence_id));
        }
        CollapsedMenuCommand::SortName => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Name);
            repaint_fence(shared);
        }
        CollapsedMenuCommand::SortKind => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Kind);
            repaint_fence(shared);
        }
        CollapsedMenuCommand::SortModified => {
            sort_fence_items(fence_id, shared, &mut out, SortMode::Modified);
            repaint_fence(shared);
        }
        CollapsedMenuCommand::PasteFromClipboard => {
            let spec_opt = lock_unpoisoned(shared)
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .cloned();
            if let Some(spec) = spec_opt {
                handle_paste_action(fence_id, &spec, shared, ctx, &mut out);
                repaint_fence(shared);
            }
        }
        CollapsedMenuCommand::UndoLast => {
            execute_undo(Some(fence_id), shared, ctx, &mut out);
            repaint_fence(shared);
        }
        CollapsedMenuCommand::SelectAll => {
            let mut guard = lock_unpoisoned(shared);
            let count = guard
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| f.items.len())
                .unwrap_or(0);
            if count > 0 {
                guard.clear_all_selections_except(Some(SelectionScope::Fence(fence_id)));
                guard.selected_items.insert(fence_id, (0..count).collect());
                guard.selection_anchor.insert(fence_id, 0);
            }
            guard.request_repaint_fence(ctx, fence_id);
        }
        CollapsedMenuCommand::RefreshPortal => {
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            let role = guard
                .config
                .fences
                .iter()
                .find(|f| f.id == fence_id)
                .map(|f| f.effective_role(&desktop_dirs));
            match role {
                Some(FenceRole::ExternalPortal { dir }) => {
                    if let Some(f) = guard.config.fence_mut(fence_id) {
                        f.items = scan_portal_directory(Path::new(&dir));
                    }
                }
                Some(FenceRole::UnfiledDesktop) => {
                    guard.refresh_all_unfiled_from_disk_if_present();
                }
                _ => {}
            }
            guard.request_repaint_fence(ctx, fence_id);
        }
        CollapsedMenuCommand::CreateBlank => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreateBlank);
        }
        CollapsedMenuCommand::CreateUnfiledDesktop => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreateUnfiledDesktop);
        }
        CollapsedMenuCommand::CreatePortal => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreatePortal);
        }
        CollapsedMenuCommand::DeleteFence => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestDelete(fence_id));
        }
        CollapsedMenuCommand::ExitApp => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestExitApp);
        }
    }
    out.needs_immediate_save
}
