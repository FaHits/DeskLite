use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    DesktopIconPos, FenceItem, FenceRole, ItemKind, Point, Rect, clamp_point_to_monitors,
    normalize_path_key,
};
use desklite_win32::{
    FreeDesktopEvent, FreeDesktopHotkey, FreeDesktopNavKey, FreeDesktopSnapshot,
    clipboard_has_files, copy_text_to_clipboard, cursor_screen_pos, delete_files_permanently,
    is_directly_on_desktop, launch_as_admin, launch_item, paste_files_to_directory_detailed,
    read_clipboard_files, rename_file_in_place, resolve_dropped_item, reveal_in_explorer,
    set_clipboard_files_for_window, show_file_properties, trash_files, user_desktop_dir,
};
use egui::ViewportId;

use crate::actions::{SortMode, sort_items_slice, start_async_drag_from_paths};
use crate::state::{
    FenceCommand, FenceUiOutput, FreeDesktopMenuCommand, FreeIconMenuCommand,
    FreeItemRenamePrompt, SelectionScope, SharedFenceState, UndoAction, lock_unpoisoned,
    request_repaint_all_fences,
};
use crate::undo::execute_undo;

#[derive(Debug, Default, Clone)]
pub struct FreeDesktopDispatchResult {
    pub needs_save: bool,
    pub needs_snapshot_refresh: bool,
    pub launch_path: Option<String>,
    pub begin_rename: Option<(String, String, Rect)>,
    pub commands: Vec<FenceCommand>,
    pub repaint_fences: Vec<i32>,
}

fn free_icon_kind(path: &str) -> ItemKind {
    let p = Path::new(path);
    if path.starts_with("::{") {
        ItemKind::VirtualShell
    } else if p.is_dir() {
        ItemKind::Folder
    } else if p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        ItemKind::Shortcut
    } else {
        ItemKind::File
    }
}

fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
}

fn free_icon_cell_size(state: &SharedFenceState) -> (i32, i32) {
    state.free_desktop_cell_size()
}

pub fn free_icon_screen_rect(state: &SharedFenceState, path: &str) -> Option<Rect> {
    let key = normalize_path_key(path);
    let (cw, ch) = free_icon_cell_size(state);
    state
        .config
        .active_free_icons()
        .iter()
        .find(|ic| normalize_path_key(&ic.id) == key)
        .map(|ic| Rect::new(ic.pos.x, ic.pos.y, ic.pos.x + cw, ic.pos.y + ch))
}

pub fn pending_free_rename(state: &SharedFenceState) -> Option<(String, String, Rect)> {
    let prompt = state.free_rename_prompt.as_ref()?;
    let rect = free_icon_screen_rect(state, &prompt.original_path)?;
    Some((
        prompt.original_path.clone(),
        prompt.draft.clone(),
        rect,
    ))
}

pub fn build_free_desktop_snapshot(state: &SharedFenceState) -> FreeDesktopSnapshot {
    state.build_free_desktop_snapshot()
}

fn hit_test_free_icon_index(state: &SharedFenceState, x: i32, y: i32) -> Option<usize> {
    if state.config.hide_all_fences {
        return None;
    }
    let (cw, ch) = free_icon_cell_size(state);
    state
        .config
        .active_free_icons()
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, ic)| {
            let right = ic.pos.x.saturating_add(cw);
            let bottom = ic.pos.y.saturating_add(ch);
            (x >= ic.pos.x && x < right && y >= ic.pos.y && y < bottom).then_some(idx)
        })
}

fn collect_non_empty_selected_fences(state: &SharedFenceState) -> Vec<i32> {
    state
        .selected_items
        .iter()
        .filter(|(_, s)| !s.is_empty())
        .map(|(&fid, _)| fid)
        .collect()
}

pub fn handle_free_desktop_drop(
    dropped_paths: Vec<String>,
    drop_screen: (i32, i32),
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let (changed, affected_fences) = handle_free_desktop_drop_core(dropped_paths, drop_screen, shared);
    if !changed {
        return;
    }
    out.needs_immediate_save = true;
    {
        let mut guard = lock_unpoisoned(shared);
        for fid in affected_fences {
            guard.request_repaint_fence(ctx, fid);
        }
        guard.mark_free_desktop_dirty(Some(ctx));
    }
    ctx.request_repaint_of(ViewportId::ROOT);
}

fn handle_free_desktop_drop_core(
    dropped_paths: Vec<String>,
    drop_screen: (i32, i32),
    shared: &Arc<Mutex<SharedFenceState>>,
) -> (bool, Vec<i32>) {
    if dropped_paths.is_empty() {
        return (false, Vec::new());
    }
    let mut external_to_copy = Vec::new();
    let mut direct_desktop = Vec::new();
    for p in dropped_paths {
        if is_directly_on_desktop(Path::new(&p)) {
            direct_desktop.push(p);
        } else {
            external_to_copy.push(p);
        }
    }

    let mut created_paths = Vec::new();
    if !external_to_copy.is_empty()
        && let Some(desktop) = user_desktop_dir()
        && let Ok(pairs) = paste_files_to_directory_detailed(&external_to_copy, &desktop, false)
    {
        for (_, dst) in pairs {
            created_paths.push(dst.to_string_lossy().into_owned());
        }
    }

    let mut all_to_place = direct_desktop.clone();
    all_to_place.extend(created_paths.iter().cloned());
    if all_to_place.is_empty() {
        return (false, Vec::new());
    }

    let mut affected_fences = Vec::new();
    {
        let mut guard = lock_unpoisoned(shared);
        let desktop_dirs = guard.desktop_dirs.clone();
        let mut removed_from_fences = Vec::new();
        for f in &mut guard.config.fences {
            if f.is_desktop_bucket(&desktop_dirs) {
                let rem: Vec<FenceItem> = f
                    .items
                    .iter()
                    .filter(|it| {
                        direct_desktop
                            .iter()
                            .any(|dp| normalize_path_key(&it.path) == normalize_path_key(dp))
                    })
                    .cloned()
                    .collect();
                if !rem.is_empty() {
                    let rem_paths: Vec<String> = rem.iter().map(|it| it.path.clone()).collect();
                    let fid = f.id;
                    removed_from_fences.push((fid, rem));
                    affected_fences.push((fid, rem_paths));
                }
            }
        }
        for (fid, rem_paths) in &affected_fences {
            let _ = guard
                .config
                .release_desktop_items(*fid, rem_paths, &desktop_dirs);
            guard.selected_items.remove(fid);
            guard.selection_anchor.remove(fid);
        }
        guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
        guard.place_released_icons_on_free_desktop(&all_to_place, Some(drop_screen));
        if !created_paths.is_empty() || !removed_from_fences.is_empty() {
            guard.push_undo(UndoAction::PasteFiles {
                fence_id: None,
                created_paths,
                moved_pairs: Vec::new(),
                removed_from_fences,
                added_paths: Vec::new(),
                is_portal: false,
            });
        }
        guard.dirty_layout = true;
        guard.bump_free_icon_epoch();
    }
    let fids = affected_fences.into_iter().map(|(fid, _)| fid).collect();
    (true, fids)
}

fn paste_to_free_desktop(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let Some((sources, is_cut)) = read_clipboard_files() else {
        if clipboard_has_files() {
            let mut guard = lock_unpoisoned(shared);
            guard.free_toast = Some(("剪贴板正忙，请重试".to_string(), Instant::now()));
            guard.mark_free_desktop_dirty(Some(ctx));
        }
        return;
    };
    if sources.is_empty() {
        return;
    }
    let Some(desktop) = user_desktop_dir() else {
        return;
    };

    let mut created_paths = Vec::new();
    let mut moved_pairs = Vec::new();
    let mut to_place = Vec::new();

    for s in &sources {
        let sp = Path::new(s);
        if is_directly_on_desktop(sp) {
            if is_cut {
                to_place.push(s.clone());
            } else if let Ok(pairs) =
                paste_files_to_directory_detailed(std::slice::from_ref(s), &desktop, false)
            {
                for (_, dst) in pairs {
                    let ds = dst.to_string_lossy().into_owned();
                    created_paths.push(ds.clone());
                    to_place.push(ds);
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
                to_place.push(ds);
            }
        }
    }

    if to_place.is_empty() {
        return;
    }

    let drop_pt = cursor_screen_pos();
    let mut affected = Vec::new();
    {
        let mut guard = lock_unpoisoned(shared);
        let desktop_dirs = guard.desktop_dirs.clone();
        let mut removed_from_fences = Vec::new();
        if is_cut {
            for f in &mut guard.config.fences {
                if f.is_desktop_bucket(&desktop_dirs) {
                    let rem: Vec<FenceItem> = f
                        .items
                        .iter()
                        .filter(|it| {
                            sources
                                .iter()
                                .any(|s| normalize_path_key(&it.path) == normalize_path_key(s))
                        })
                        .cloned()
                        .collect();
                    if !rem.is_empty() {
                        let rem_paths: Vec<String> = rem.iter().map(|it| it.path.clone()).collect();
                        let fid = f.id;
                        removed_from_fences.push((fid, rem));
                        affected.push((fid, rem_paths));
                    }
                }
            }
            for (fid, rem_paths) in &affected {
                let _ = guard
                    .config
                    .release_desktop_items(*fid, rem_paths, &desktop_dirs);
            }
            guard.cut_paths.clear();
        }
        guard.place_released_icons_on_free_desktop(&to_place, drop_pt);
        guard.push_undo(UndoAction::PasteFiles {
            fence_id: None,
            created_paths,
            moved_pairs,
            removed_from_fences,
            added_paths: Vec::new(),
            is_portal: false,
        });
        guard.free_toast = Some((format!("已粘贴 {} 项", to_place.len()), Instant::now()));
        guard.dirty_layout = true;
        for (fid, _) in &affected {
            guard.request_repaint_fence(ctx, *fid);
        }
        guard.bump_free_icon_epoch();
        guard.mark_free_desktop_dirty(Some(ctx));
    }
    out.needs_immediate_save = true;
}

fn commit_free_item_rename_core(
    old_path: &str,
    new_name: &str,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> bool {
    let old_p = Path::new(old_path);
    let trimmed = new_name.trim();
    if trimmed.is_empty()
        || old_p
            .file_name()
            .is_some_and(|n| n == std::ffi::OsStr::new(trimmed))
    {
        lock_unpoisoned(shared).free_rename_prompt = None;
        return false;
    }
    match rename_file_in_place(old_p, trimmed) {
        Ok(updated) => {
            let new_path = updated.path.clone();
            let new_key = normalize_path_key(&new_path);
            let old_key = normalize_path_key(old_path);
            let mut guard = lock_unpoisoned(shared);
            guard.invalidate_icon_texture(old_path);
            guard.invalidate_icon_texture(&new_path);
            if let Some(free) = guard.config.active_free_icons_mut()
                && let Some(slot) = free.iter_mut().find(|ic| normalize_path_key(&ic.id) == old_key)
            {
                slot.id = new_path.clone();
                slot.display_name = updated.display_name;
            }
            if guard.free_selected_paths.remove(&old_key) {
                guard.free_selected_paths.insert(new_key.clone());
            }
            if guard.free_selection_anchor.as_deref() == Some(old_key.as_str()) {
                guard.free_selection_anchor = Some(new_key);
            }
            guard.push_undo(UndoAction::RenameItem {
                fence_id: None,
                old_path: old_path.to_string(),
                new_path,
            });
            guard.free_rename_prompt = None;
            guard.dirty_layout = true;
            guard.bump_free_icon_epoch();
            true
        }
        Err(e) => {
            let mut guard = lock_unpoisoned(shared);
            guard.free_rename_prompt = None;
            guard.free_toast = Some((format!("重命名失败: {e}"), Instant::now()));
            false
        }
    }
}

pub fn handle_free_desktop_event(
    ev: FreeDesktopEvent,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FreeDesktopDispatchResult {
    let mut res = FreeDesktopDispatchResult::default();
    match ev {
        FreeDesktopEvent::IconClicked { path, ctrl, shift } => {
            let key = normalize_path_key(&path);
            let mut guard = lock_unpoisoned(shared);
            let prev_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            res.repaint_fences = prev_fences;

            if shift {
                let anchor_key = guard.free_selection_anchor.clone();
                let ordered_keys: Vec<String> = guard
                    .config
                    .active_free_icons()
                    .iter()
                    .map(|ic| normalize_path_key(&ic.id))
                    .collect();
                let anchor_idx = anchor_key
                    .as_ref()
                    .and_then(|ak| ordered_keys.iter().position(|k| k == ak));
                let target_idx = ordered_keys.iter().position(|k| k == &key);
                if !ctrl {
                    guard.free_selected_paths.clear();
                }
                if let (Some(a), Some(b)) = (anchor_idx, target_idx) {
                    for k in &ordered_keys[a.min(b)..=a.max(b)] {
                        guard.free_selected_paths.insert(k.clone());
                    }
                } else {
                    guard.free_selected_paths.insert(key.clone());
                    guard.free_selection_anchor = Some(key);
                }
            } else if ctrl {
                if !guard.free_selected_paths.remove(&key) {
                    guard.free_selected_paths.insert(key.clone());
                }
                guard.free_selection_anchor = Some(key.clone());
                guard.free_focus_path = Some(key);
            } else {
                guard.free_selected_paths.clear();
                guard.free_selected_paths.insert(key.clone());
                guard.free_selection_anchor = Some(key.clone());
                guard.free_focus_path = Some(key);
            }
            if shift {
                guard.free_focus_path = Some(normalize_path_key(&path));
            }
            res.needs_snapshot_refresh = true;
        }
        FreeDesktopEvent::IconDoubleClicked { path } => {
            let mut guard = lock_unpoisoned(shared);
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            res.launch_path = Some(path);
        }
        FreeDesktopEvent::BlankClicked => {
            let mut guard = lock_unpoisoned(shared);
            let had_free = !guard.free_selected_paths.is_empty();
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(None);
            res.needs_snapshot_refresh = had_free;
        }
        FreeDesktopEvent::BlankDoubleClicked => {
            res.commands.push(FenceCommand::ToggleHideAllFences);
        }
        FreeDesktopEvent::MarqueeSelect { rect, additive } => {
            let mut guard = lock_unpoisoned(shared);
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            let (cw, ch) = free_icon_cell_size(&guard);
            let mut next_selected: HashSet<String> = if additive {
                guard.free_selected_paths.clone()
            } else {
                HashSet::new()
            };
            let mut first_hit: Option<String> = None;
            for ic in guard.config.active_free_icons() {
                let item_rect = Rect::new(ic.pos.x, ic.pos.y, ic.pos.x + cw, ic.pos.y + ch);
                if rects_intersect(item_rect, rect) {
                    let k = normalize_path_key(&ic.id);
                    if first_hit.is_none() {
                        first_hit = Some(k.clone());
                    }
                    next_selected.insert(k);
                }
            }
            if guard.free_selected_paths != next_selected {
                guard.free_selected_paths = next_selected;
                if !additive {
                    guard.free_selection_anchor = first_hit;
                }
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopEvent::ContextMenuBlank { screen_x, screen_y } => {
            let mut guard = lock_unpoisoned(shared);
            let had_free = !guard.free_selected_paths.is_empty();
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(None);
            res.needs_snapshot_refresh = had_free;
            res.commands.push(FenceCommand::ShowFreeDesktopMenu {
                screen_x,
                screen_y,
            });
        }
        FreeDesktopEvent::ContextMenuIcon {
            path,
            screen_x,
            screen_y,
        } => {
            let key = normalize_path_key(&path);
            let mut guard = lock_unpoisoned(shared);
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            if !guard.free_selected_paths.contains(&key) {
                guard.free_selected_paths.clear();
                guard.free_selected_paths.insert(key.clone());
                guard.free_selection_anchor = Some(key.clone());
                guard.free_focus_path = Some(key);
                res.needs_snapshot_refresh = true;
            }
            let mut paths: Vec<String> = guard
                .config
                .active_free_icons()
                .iter()
                .filter(|ic| {
                    guard
                        .free_selected_paths
                        .contains(&normalize_path_key(&ic.id))
                })
                .map(|ic| ic.id.clone())
                .collect();
            if paths.is_empty() {
                paths.push(path);
            }
            res.commands.push(FenceCommand::ShowFreeIconMenu {
                paths,
                screen_x,
                screen_y,
            });
        }
        FreeDesktopEvent::StartDragIcons { start_screen } => {
            let drag_paths = {
                let mut guard = lock_unpoisoned(shared);
                res.repaint_fences = collect_non_empty_selected_fences(&guard);
                guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
                if let Some(hit_idx) =
                    hit_test_free_icon_index(&guard, start_screen.0, start_screen.1)
                    && let Some(hit_icon) = guard.config.active_free_icons().get(hit_idx).cloned()
                {
                    let hit_key = normalize_path_key(&hit_icon.id);
                    if !guard.free_selected_paths.contains(&hit_key) {
                        guard.free_selected_paths.clear();
                        guard.free_selected_paths.insert(hit_key.clone());
                        guard.free_selection_anchor = Some(hit_key);
                        res.needs_snapshot_refresh = true;
                    }
                    let mut selected: Vec<String> = guard
                        .config
                        .active_free_icons()
                        .iter()
                        .filter(|ic| {
                            guard
                                .free_selected_paths
                                .contains(&normalize_path_key(&ic.id))
                        })
                        .map(|ic| ic.id.clone())
                        .collect();
                    if selected.is_empty() {
                        selected.push(hit_icon.id);
                    }
                    selected
                } else {
                    Vec::new()
                }
            };
            if !drag_paths.is_empty() {
                start_async_drag_from_paths(None, drag_paths, shared);
            }
        }
        FreeDesktopEvent::FilesDroppedFromExplorer {
            paths,
            screen_x,
            screen_y,
        } => {
            let (changed, fids) =
                handle_free_desktop_drop_core(paths, (screen_x, screen_y), shared);
            res.needs_save = changed;
            res.needs_snapshot_refresh = changed;
            res.repaint_fences = fids;
        }
        FreeDesktopEvent::FocusLost => {
            let mut guard = lock_unpoisoned(shared);
            if !guard.is_focus_clear_exempt(SelectionScope::FreeDesktop, None)
                && !guard.free_selected_paths.is_empty()
            {
                guard.free_selected_paths.clear();
                guard.free_selection_anchor = None;
                guard.free_focus_path = None;
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopEvent::Hotkey(hk) => {
            handle_free_desktop_hotkey(hk, shared, &mut res);
        }
        FreeDesktopEvent::RenameCommitted { old_path, new_name } => {
            let changed = commit_free_item_rename_core(&old_path, &new_name, shared);
            res.needs_save = changed;
            res.needs_snapshot_refresh = true;
        }
        FreeDesktopEvent::RenameCancelled => {
            lock_unpoisoned(shared).free_rename_prompt = None;
        }
    }
    res
}

fn collect_ordered_free_selected_paths(guard: &SharedFenceState) -> Vec<String> {
    guard
        .config
        .active_free_icons()
        .iter()
        .filter(|ic| {
            guard
                .free_selected_paths
                .contains(&normalize_path_key(&ic.id))
        })
        .map(|ic| ic.id.clone())
        .collect()
}

fn navigate_free_desktop_icons(
    guard: &mut SharedFenceState,
    key: FreeDesktopNavKey,
    shift: bool,
) -> bool {
    let icons = guard.config.active_free_icons().to_vec();
    if icons.is_empty() {
        return false;
    }
    let (_, ch) = guard.free_desktop_cell_size();
    let row_band = (ch / 2).max(1);
    let mut ordered_indices: Vec<usize> = (0..icons.len()).collect();
    ordered_indices.sort_by_key(|&idx| {
        let pos = icons[idx].pos;
        (pos.y.div_euclid(row_band), pos.x, pos.y)
    });

    let cur_key_opt = guard
        .free_focus_path
        .clone()
        .or_else(|| guard.free_selection_anchor.clone())
        .or_else(|| {
            ordered_indices
                .iter()
                .map(|&i| normalize_path_key(&icons[i].id))
                .find(|k| guard.free_selected_paths.contains(k))
        });

    let cur_idx_opt = cur_key_opt
        .as_ref()
        .and_then(|ck| icons.iter().position(|ic| normalize_path_key(&ic.id) == *ck));

    let prev_selected = guard.free_selected_paths.clone();
    let prev_focus = guard.free_focus_path.clone();

    let Some(cur_idx) = cur_idx_opt else {
        let pick_idx = match key {
            FreeDesktopNavKey::End => *ordered_indices.last().unwrap(),
            _ => ordered_indices[0],
        };
        let k = normalize_path_key(&icons[pick_idx].id);
        guard.free_selected_paths.clear();
        guard.free_selected_paths.insert(k.clone());
        guard.free_selection_anchor = Some(k.clone());
        guard.free_focus_path = Some(k);
        return true;
    };

    let cur_pos = icons[cur_idx].pos;
    let next_idx = match key {
        FreeDesktopNavKey::Home => ordered_indices[0],
        FreeDesktopNavKey::End => *ordered_indices.last().unwrap(),
        FreeDesktopNavKey::Left => icons
            .iter()
            .enumerate()
            .filter(|(_, ic)| ic.pos.x < cur_pos.x)
            .min_by_key(|(_, ic)| (cur_pos.x - ic.pos.x) + 3 * (ic.pos.y - cur_pos.y).abs())
            .map(|(idx, _)| idx)
            .unwrap_or(cur_idx),
        FreeDesktopNavKey::Right => icons
            .iter()
            .enumerate()
            .filter(|(_, ic)| ic.pos.x > cur_pos.x)
            .min_by_key(|(_, ic)| (ic.pos.x - cur_pos.x) + 3 * (ic.pos.y - cur_pos.y).abs())
            .map(|(idx, _)| idx)
            .unwrap_or(cur_idx),
        FreeDesktopNavKey::Up => icons
            .iter()
            .enumerate()
            .filter(|(_, ic)| ic.pos.y < cur_pos.y)
            .min_by_key(|(_, ic)| (cur_pos.y - ic.pos.y) + 3 * (ic.pos.x - cur_pos.x).abs())
            .map(|(idx, _)| idx)
            .unwrap_or(cur_idx),
        FreeDesktopNavKey::Down => icons
            .iter()
            .enumerate()
            .filter(|(_, ic)| ic.pos.y > cur_pos.y)
            .min_by_key(|(_, ic)| (ic.pos.y - cur_pos.y) + 3 * (ic.pos.x - cur_pos.x).abs())
            .map(|(idx, _)| idx)
            .unwrap_or(cur_idx),
    };

    let next_key = normalize_path_key(&icons[next_idx].id);
    if shift {
        let anchor_key = guard
            .free_selection_anchor
            .clone()
            .unwrap_or_else(|| normalize_path_key(&icons[cur_idx].id));
        guard.free_selection_anchor = Some(anchor_key.clone());
        let anchor_pos = ordered_indices
            .iter()
            .position(|&i| normalize_path_key(&icons[i].id) == anchor_key);
        let next_pos = ordered_indices.iter().position(|&i| i == next_idx);
        guard.free_selected_paths.clear();
        if let (Some(a), Some(b)) = (anchor_pos, next_pos) {
            for &idx in &ordered_indices[a.min(b)..=a.max(b)] {
                guard
                    .free_selected_paths
                    .insert(normalize_path_key(&icons[idx].id));
            }
        } else {
            guard.free_selected_paths.insert(next_key.clone());
        }
        guard.free_focus_path = Some(next_key);
    } else {
        guard.free_selected_paths.clear();
        guard.free_selected_paths.insert(next_key.clone());
        guard.free_selection_anchor = Some(next_key.clone());
        guard.free_focus_path = Some(next_key);
    }

    prev_selected != guard.free_selected_paths || prev_focus != guard.free_focus_path
}

fn type_ahead_free_desktop(guard: &mut SharedFenceState, ch: char) -> bool {
    if ch.is_control() {
        return false;
    }
    let ch_lower = ch.to_lowercase().to_string();
    if ch_lower.trim().is_empty() {
        return false;
    }
    let icons = guard.config.active_free_icons().to_vec();
    if icons.is_empty() {
        return false;
    }
    let now = Instant::now();
    let next_q = match &guard.free_type_ahead {
        Some((prev, at)) if now.duration_since(*at) < Duration::from_millis(850) => {
            format!("{prev}{ch_lower}")
        }
        _ => ch_lower.clone(),
    };
    guard.free_type_ahead = Some((next_q.clone(), now));

    let cur_idx = guard
        .free_focus_path
        .as_ref()
        .or(guard.free_selection_anchor.as_ref())
        .and_then(|fk| icons.iter().position(|ic| normalize_path_key(&ic.id) == *fk));
    let start = cur_idx.map(|i| (i + 1) % icons.len()).unwrap_or(0);

    let hit = (0..icons.len())
        .map(|off| (start + off) % icons.len())
        .find(|&idx| icons[idx].display_name.to_lowercase().starts_with(&next_q))
        .or_else(|| {
            (0..icons.len())
                .map(|off| (start + off) % icons.len())
                .find(|&idx| icons[idx].display_name.to_lowercase().starts_with(&ch_lower))
        });

    let Some(idx) = hit else {
        return false;
    };
    let key = normalize_path_key(&icons[idx].id);
    guard.free_selected_paths.clear();
    guard.free_selected_paths.insert(key.clone());
    guard.free_selection_anchor = Some(key.clone());
    guard.free_focus_path = Some(key);
    true
}

fn handle_free_desktop_hotkey(
    hk: FreeDesktopHotkey,
    shared: &Arc<Mutex<SharedFenceState>>,
    res: &mut FreeDesktopDispatchResult,
) {
    match hk {
        FreeDesktopHotkey::SelectAll => {
            let mut guard = lock_unpoisoned(shared);
            if !guard.config.active_free_icons().is_empty() {
                res.repaint_fences = collect_non_empty_selected_fences(&guard);
                guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
                let keys: Vec<String> = guard
                    .config
                    .active_free_icons()
                    .iter()
                    .map(|ic| normalize_path_key(&ic.id))
                    .collect();
                guard.free_selection_anchor = keys.first().cloned();
                guard.free_focus_path = keys.first().cloned();
                guard.free_selected_paths = keys.into_iter().collect();
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::Copy | FreeDesktopHotkey::Cut => {
            let cut = matches!(hk, FreeDesktopHotkey::Cut);
            let (hwnd, paths) = {
                let guard = lock_unpoisoned(shared);
                (guard.free_desktop_hwnd, collect_ordered_free_selected_paths(&guard))
            };
            if paths.is_empty() {
                return;
            }
            if set_clipboard_files_for_window(hwnd, &paths, cut).is_ok() {
                let mut guard = lock_unpoisoned(shared);
                guard.cut_paths.clear();
                if cut {
                    for p in &paths {
                        guard.cut_paths.insert(normalize_path_key(p));
                    }
                }
                let verb = if cut { "已剪切" } else { "已复制" };
                guard.free_toast = Some((format!("{verb} {} 项", paths.len()), Instant::now()));
                res.needs_snapshot_refresh = true;
            } else {
                lock_unpoisoned(shared).free_toast =
                    Some(("剪贴板被占用，请重试".to_string(), Instant::now()));
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::CopyPaths => {
            let paths = {
                let guard = lock_unpoisoned(shared);
                collect_ordered_free_selected_paths(&guard)
            };
            if !paths.is_empty() && copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                lock_unpoisoned(shared).free_toast =
                    Some(("已复制路径".to_string(), Instant::now()));
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::Paste => {
            res.commands
                .push(FenceCommand::FreeDesktopMenuAction(FreeDesktopMenuCommand::PasteToDesktop));
        }
        FreeDesktopHotkey::Undo => {
            res.commands
                .push(FenceCommand::FreeDesktopMenuAction(FreeDesktopMenuCommand::UndoLast));
        }
        FreeDesktopHotkey::Rename => {
            let mut guard = lock_unpoisoned(shared);
            let paths = collect_ordered_free_selected_paths(&guard);
            if let Some(first) = paths.first().cloned()
                && let Some(rect) = free_icon_screen_rect(&guard, &first)
            {
                let file_name = Path::new(&first)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| first.clone());
                guard.free_rename_prompt = Some(FreeItemRenamePrompt {
                    original_path: first.clone(),
                    draft: file_name.clone(),
                    needs_focus: true,
                });
                res.begin_rename = Some((first, file_name, rect));
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::Delete { permanent } => {
            let paths = {
                let guard = lock_unpoisoned(shared);
                collect_ordered_free_selected_paths(&guard)
            };
            if paths.is_empty() {
                return;
            }
            let op_res = if permanent {
                delete_files_permanently(&paths)
            } else {
                trash_files(&paths)
            };
            match op_res {
                Ok(true) => {
                    let mut guard = lock_unpoisoned(shared);
                    let desktop_dirs = guard.desktop_dirs.clone();
                    for p in &paths {
                        guard.invalidate_icon_texture(p);
                        let _ = guard.config.remove_desktop_path_everywhere(p, &desktop_dirs);
                        guard.free_selected_paths.remove(&normalize_path_key(p));
                    }
                    guard.free_selection_anchor = None;
                    guard.free_focus_path = None;
                    let verb = if permanent { "已永久删除" } else { "已移至回收站" };
                    guard.free_toast = Some((format!("{verb} {} 项", paths.len()), Instant::now()));
                    guard.dirty_layout = true;
                    guard.bump_free_icon_epoch();
                    res.needs_save = true;
                    res.needs_snapshot_refresh = true;
                }
                Ok(false) => {}
                Err(e) => {
                    lock_unpoisoned(shared).free_toast =
                        Some((format!("删除失败: {e}"), Instant::now()));
                    res.needs_snapshot_refresh = true;
                }
            }
        }
        FreeDesktopHotkey::Refresh => {
            lock_unpoisoned(shared).bump_free_icon_epoch();
            res.commands.push(FenceCommand::RefreshDesktop);
        }
        FreeDesktopHotkey::ClearSelection => {
            let mut guard = lock_unpoisoned(shared);
            let had_any = !guard.free_selected_paths.is_empty()
                || guard.free_selection_anchor.is_some()
                || !guard.cut_paths.is_empty();
            if had_any {
                guard.free_selected_paths.clear();
                guard.free_selection_anchor = None;
                guard.free_focus_path = None;
                guard.cut_paths.clear();
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::OpenSelected => {
            let paths = {
                let guard = lock_unpoisoned(shared);
                collect_ordered_free_selected_paths(&guard)
            };
            for p in paths.into_iter().take(8) {
                if let Err(e) = launch_item(&p) {
                    lock_unpoisoned(shared).free_toast =
                        Some((format!("打开失败: {e}"), Instant::now()));
                    res.needs_snapshot_refresh = true;
                    break;
                }
            }
        }
        FreeDesktopHotkey::Properties => {
            let first = {
                let guard = lock_unpoisoned(shared);
                collect_ordered_free_selected_paths(&guard).into_iter().next()
            };
            if let Some(p) = first {
                let _ = show_file_properties(&p);
            }
        }
        FreeDesktopHotkey::Navigate { key, shift } => {
            let mut guard = lock_unpoisoned(shared);
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            if navigate_free_desktop_icons(&mut guard, key, shift) {
                res.needs_snapshot_refresh = true;
            }
        }
        FreeDesktopHotkey::TypeAhead(ch) => {
            let mut guard = lock_unpoisoned(shared);
            res.repaint_fences = collect_non_empty_selected_fences(&guard);
            guard.clear_all_selections_except(Some(SelectionScope::FreeDesktop));
            if type_ahead_free_desktop(&mut guard, ch) {
                res.needs_snapshot_refresh = true;
            }
        }
    }
}

fn arrange_free_icons(
    shared: &Arc<Mutex<SharedFenceState>>,
    mode: SortMode,
    ctx: &egui::Context,
) -> bool {
    let mut guard = lock_unpoisoned(shared);
    let current = guard.config.active_free_icons().to_vec();
    if current.is_empty() {
        return false;
    }
    let mut items: Vec<FenceItem> = current
        .iter()
        .enumerate()
        .map(|(i, ic)| FenceItem {
            display_name: ic.display_name.clone(),
            order: i as i32,
            kind: free_icon_kind(&ic.id),
            path: ic.id.clone(),
        })
        .collect();
    sort_items_slice(&mut items, mode);

    let monitors = guard.topology.monitors().to_vec();
    let (step_x, step_y) = guard.free_desktop_cell_size();
    let work = monitors
        .iter()
        .find(|m| m.is_primary)
        .or_else(|| monitors.first())
        .map(|m| m.work_area)
        .unwrap_or(Rect::new(0, 0, 1920, 1040));
    let start_x = work.left + 20;
    let start_y = work.top + 20;
    let max_rows = ((work.bottom - start_y).max(step_y) / step_y).max(1);

    let mut arranged = Vec::with_capacity(items.len());
    for (idx, it) in items.into_iter().enumerate() {
        let col = (idx / max_rows as usize) as i32;
        let row = (idx % max_rows as usize) as i32;
        let raw = Point {
            x: start_x + col * step_x,
            y: start_y + row * step_y,
        };
        let pos = if monitors.is_empty() {
            raw
        } else {
            clamp_point_to_monitors(raw, (step_x, step_y), &monitors)
                .map(|(p, _)| p)
                .unwrap_or(raw)
        };
        arranged.push(DesktopIconPos {
            id: it.path,
            display_name: it.display_name,
            pos,
        });
    }
    if let Some(free) = guard.config.active_free_icons_mut() {
        *free = arranged;
    }
    guard.free_toast = Some(("已重新排列桌面图标".to_string(), Instant::now()));
    guard.dirty_layout = true;
    guard.mark_free_desktop_dirty(Some(ctx));
    true
}

pub fn apply_free_desktop_menu_command(
    cmd: FreeDesktopMenuCommand,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> bool {
    let mut out = FenceUiOutput::default();
    match cmd {
        FreeDesktopMenuCommand::CreateBlank => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreateBlank);
        }
        FreeDesktopMenuCommand::CreateUnfiledDesktop => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreateUnfiledDesktop);
        }
        FreeDesktopMenuCommand::CreatePortal => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestCreatePortal);
        }
        FreeDesktopMenuCommand::ToggleHideAllFences => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::ToggleHideAllFences);
        }
        FreeDesktopMenuCommand::ArrangeByName => {
            return arrange_free_icons(shared, SortMode::Name, ctx);
        }
        FreeDesktopMenuCommand::ArrangeByKind => {
            return arrange_free_icons(shared, SortMode::Kind, ctx);
        }
        FreeDesktopMenuCommand::ArrangeByModified => {
            return arrange_free_icons(shared, SortMode::Modified, ctx);
        }
        FreeDesktopMenuCommand::RefreshDesktop => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RefreshDesktop);
        }
        FreeDesktopMenuCommand::PasteToDesktop => {
            paste_to_free_desktop(shared, ctx, &mut out);
        }
        FreeDesktopMenuCommand::UndoLast => {
            execute_undo(None, shared, ctx, &mut out);
        }
        FreeDesktopMenuCommand::OpenDesktopFolder => {
            if let Some(dir) = user_desktop_dir() {
                let _ = launch_item(&dir.to_string_lossy());
            }
        }
        FreeDesktopMenuCommand::ExitApp => {
            lock_unpoisoned(shared).push_command(ctx, FenceCommand::RequestExitApp);
        }
    }
    out.needs_immediate_save
}

pub fn apply_free_icon_menu_command(
    paths: &[String],
    cmd: FreeIconMenuCommand,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> bool {
    if paths.is_empty() {
        return false;
    }
    let mut out = FenceUiOutput::default();
    match cmd {
        FreeIconMenuCommand::Open => {
            for p in paths.iter().take(8) {
                if let Err(e) = launch_item(p) {
                    lock_unpoisoned(shared).free_toast =
                        Some((format!("打开失败: {e}"), Instant::now()));
                    break;
                }
            }
        }
        FreeIconMenuCommand::OpenAsAdmin => {
            if let Some(first) = paths.first()
                && let Err(e) = launch_as_admin(first)
            {
                lock_unpoisoned(shared).free_toast =
                    Some((format!("提权运行失败: {e}"), Instant::now()));
            }
        }
        FreeIconMenuCommand::RevealInExplorer => {
            if let Some(first) = paths.first() {
                let _ = reveal_in_explorer(first);
            }
        }
        FreeIconMenuCommand::Copy { cut } => {
            let hwnd = lock_unpoisoned(shared).free_desktop_hwnd;
            if set_clipboard_files_for_window(hwnd, paths, cut).is_ok() {
                let mut guard = lock_unpoisoned(shared);
                guard.cut_paths.clear();
                if cut {
                    for p in paths {
                        guard.cut_paths.insert(normalize_path_key(p));
                    }
                }
                let verb = if cut { "已剪切" } else { "已复制" };
                guard.free_toast = Some((format!("{verb} {} 项", paths.len()), Instant::now()));
                guard.mark_free_desktop_dirty(Some(ctx));
            } else {
                lock_unpoisoned(shared).free_toast =
                    Some(("剪贴板被占用，请重试".to_string(), Instant::now()));
            }
        }
        FreeIconMenuCommand::CopyPaths => {
            if copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                let mut guard = lock_unpoisoned(shared);
                guard.free_toast = Some(("已复制路径".to_string(), Instant::now()));
                guard.mark_free_desktop_dirty(Some(ctx));
            }
        }
        FreeIconMenuCommand::Rename => {
            if let Some(first) = paths.first() {
                let file_name = Path::new(first)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| first.clone());
                let mut guard = lock_unpoisoned(shared);
                guard.free_rename_prompt = Some(FreeItemRenamePrompt {
                    original_path: first.clone(),
                    draft: file_name,
                    needs_focus: true,
                });
                guard.mark_free_desktop_dirty(Some(ctx));
            }
        }
        FreeIconMenuCommand::MoveToFence(target_fid) => {
            let moved = {
                let mut guard = lock_unpoisoned(shared);
                let desktop_dirs = guard.desktop_dirs.clone();
                let is_bucket = guard.config.fences.iter().any(|f| {
                    f.id == target_fid
                        && f.effective_role(&desktop_dirs) == FenceRole::DesktopBucket
                });
                if is_bucket {
                    let prev_free_icons: Vec<DesktopIconPos> = guard
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
                    let items: Vec<FenceItem> = paths
                        .iter()
                        .enumerate()
                        .map(|(i, p)| resolve_dropped_item(Path::new(p), i as i32))
                        .collect();
                    if guard.config.assign_desktop_items_to_bucket(
                        target_fid,
                        items.clone(),
                        &desktop_dirs,
                    ) {
                        for p in paths {
                            guard.free_selected_paths.remove(&normalize_path_key(p));
                        }
                        guard.push_undo(UndoAction::MoveFreeIconsToBucket {
                            dst_fence_id: target_fid,
                            items,
                            prev_free_icons,
                        });
                        guard.dirty_layout = true;
                        guard.bump_free_icon_epoch();
                        guard.mark_free_desktop_dirty(Some(ctx));
                        out.needs_immediate_save = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if moved {
                request_repaint_all_fences(shared, ctx);
            }
        }
        FreeIconMenuCommand::Trash => match trash_files(paths) {
            Ok(true) => {
                {
                    let mut guard = lock_unpoisoned(shared);
                    let desktop_dirs = guard.desktop_dirs.clone();
                    for p in paths {
                        guard.invalidate_icon_texture(p);
                        let _ = guard.config.remove_desktop_path_everywhere(p, &desktop_dirs);
                        guard.free_selected_paths.remove(&normalize_path_key(p));
                    }
                    guard.free_toast = Some((format!("已移至回收站 {} 项", paths.len()), Instant::now()));
                    guard.dirty_layout = true;
                    guard.bump_free_icon_epoch();
                    guard.mark_free_desktop_dirty(Some(ctx));
                    out.needs_immediate_save = true;
                }
                request_repaint_all_fences(shared, ctx);
            }
            Ok(false) => {}
            Err(e) => {
                lock_unpoisoned(shared).free_toast =
                    Some((format!("删除失败: {e}"), Instant::now()));
            }
        },
        FreeIconMenuCommand::Properties => {
            if let Some(first) = paths.first() {
                let _ = show_file_properties(first);
            }
        }
    }
    out.needs_immediate_save
}
