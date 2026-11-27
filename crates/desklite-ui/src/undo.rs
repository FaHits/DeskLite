use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use desklite_core::{FenceItem, normalize_path_key};
use desklite_win32::{move_path_in_place, rename_file_in_place, scan_portal_directory, trash_files};
use egui::ViewportId;

use crate::state::{FenceUiOutput, SharedFenceState, UndoAction, lock_unpoisoned};

fn push_undo_toast(guard: &mut SharedFenceState, caller_fence_id: Option<i32>, msg: String) {
    if let Some(fid) = caller_fence_id {
        guard.toast_msg.insert(fid, (msg, Instant::now()));
    } else {
        guard.free_toast = Some((msg, Instant::now()));
    }
}

pub fn execute_undo(
    caller_fence_id: Option<i32>,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let action_opt = { lock_unpoisoned(shared).undo_stack.pop() };
    let Some(action) = action_opt else {
        let mut guard = lock_unpoisoned(shared);
        push_undo_toast(&mut guard, caller_fence_id, "无可撤销操作".to_string());
        ctx.request_repaint();
        return;
    };

    let mut notify_fences: HashSet<i32> = caller_fence_id.into_iter().collect();
    let mut notify_free = caller_fence_id.is_none();
    let mut label;

    match action {
        UndoAction::RenameItem {
            fence_id,
            old_path,
            new_path,
        } => {
            if let Some(fid) = fence_id {
                notify_fences.insert(fid);
            } else {
                notify_free = true;
            }
            label = "已撤销重命名";
            let old_name = Path::new(&old_path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !old_name.is_empty() && Path::new(&new_path).exists() {
                match rename_file_in_place(Path::new(&new_path), &old_name) {
                    Ok(updated) => {
                        let mut guard = lock_unpoisoned(shared);
                        let desktop_dirs = guard.desktop_dirs.clone();
                        guard.invalidate_icon_texture(&new_path);
                        guard.invalidate_icon_texture(&updated.path);
                        if let Some(fid) = fence_id {
                            if let Some(f) = guard.config.fence_mut(fid) {
                                if let Some(dir) = f.external_portal_dir(&desktop_dirs) {
                                    f.items = scan_portal_directory(Path::new(&dir));
                                } else if let Some(slot) = f.items.iter_mut().find(|it| {
                                    normalize_path_key(&it.path) == normalize_path_key(&new_path)
                                }) {
                                    let ord = slot.order;
                                    *slot = updated;
                                    slot.order = ord;
                                }
                            }
                        } else if let Some(free) = guard.config.active_free_icons_mut()
                            && let Some(slot) = free.iter_mut().find(|ic| {
                                normalize_path_key(&ic.id) == normalize_path_key(&new_path)
                            })
                        {
                            slot.id = updated.path;
                            slot.display_name = updated.display_name;
                        }
                        guard.refresh_all_unfiled_from_disk_if_present();
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                    Err(e) => {
                        let mut guard = lock_unpoisoned(shared);
                        push_undo_toast(
                            &mut guard,
                            caller_fence_id,
                            format!("撤销重命名失败: {e}"),
                        );
                        ctx.request_repaint();
                        return;
                    }
                }
            } else {
                let mut guard = lock_unpoisoned(shared);
                push_undo_toast(
                    &mut guard,
                    caller_fence_id,
                    "无法撤销重命名：文件不存在或重名".to_string(),
                );
                ctx.request_repaint();
                return;
            }
        }
        UndoAction::RenameFence { fence_id, old_name } => {
            notify_fences.insert(fence_id);
            label = "已撤销格子重命名";
            let mut guard = lock_unpoisoned(shared);
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.name = old_name;
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "目标格子已不存在，无法撤销";
            }
        }
        UndoAction::RemoveItems { fence_id, items } => {
            notify_fences.insert(fence_id);
            notify_free = true;
            label = "已撤销移出";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            let valid: Vec<FenceItem> = items
                .into_iter()
                .filter(|it| Path::new(&it.path).exists() || it.path.starts_with("::{"))
                .collect();
            if valid.is_empty() {
                label = "原文件已不存在，无法撤销移出";
            } else if guard
                .config
                .assign_desktop_items_to_bucket(fence_id, valid, &desktop_dirs)
            {
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "目标格子已不存在或不是收纳格";
            }
        }
        UndoAction::AddItems {
            fence_id,
            added_paths,
        } => {
            notify_fences.insert(fence_id);
            notify_free = true;
            label = "已撤销移入";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            if guard
                .config
                .release_desktop_items(fence_id, &added_paths, &desktop_dirs)
                > 0
            {
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.place_released_icons_on_free_desktop(&added_paths, None);
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "原格子或条目已不存在";
            }
        }
        UndoAction::MoveBetweenBuckets {
            src_fence_id,
            dst_fence_id,
            items,
        } => {
            notify_fences.insert(src_fence_id);
            notify_fences.insert(dst_fence_id);
            label = "已撤销跨格移动";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            if guard
                .config
                .assign_desktop_items_to_bucket(src_fence_id, items, &desktop_dirs)
            {
                guard.selected_items.remove(&dst_fence_id);
                guard.selection_anchor.remove(&dst_fence_id);
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "原格子已不存在，无法撤销移动";
            }
        }
        UndoAction::MoveFreeIconsToBucket {
            dst_fence_id,
            items,
            prev_free_icons,
        } => {
            notify_fences.insert(dst_fence_id);
            notify_free = true;
            label = "已撤销拖入格子";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            let paths: Vec<String> = items.iter().map(|it| it.path.clone()).collect();
            guard
                .config
                .release_desktop_items(dst_fence_id, &paths, &desktop_dirs);
            guard.selected_items.remove(&dst_fence_id);
            guard.selection_anchor.remove(&dst_fence_id);
            for ic in prev_free_icons {
                if Path::new(&ic.id).exists() {
                    let _ = guard.config.upsert_free_icon(ic, &desktop_dirs);
                }
            }
            guard.refresh_all_unfiled_from_disk_if_present();
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
        }
        UndoAction::ReleaseBucketToFree {
            src_fence_id,
            items,
        } => {
            notify_fences.insert(src_fence_id);
            notify_free = true;
            label = "已撤销移出到桌面";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            if guard
                .config
                .assign_desktop_items_to_bucket(src_fence_id, items, &desktop_dirs)
            {
                guard.refresh_all_unfiled_from_disk_if_present();
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "原格子已不存在，无法撤销";
            }
        }
        UndoAction::PasteFiles {
            fence_id,
            created_paths,
            moved_pairs,
            removed_from_fences,
            added_paths,
            is_portal,
        } => {
            if let Some(fid) = fence_id
                && caller_fence_id != Some(fid)
            {
                notify_fences.insert(fid);
            }
            notify_free = true;
            label = "已撤销粘贴";
            let mut move_errors = 0usize;
            for (src_before, dst_after) in &moved_pairs {
                if move_path_in_place(Path::new(dst_after), Path::new(src_before)).is_err() {
                    move_errors += 1;
                }
            }
            let existing_created: Vec<String> = created_paths
                .into_iter()
                .filter(|p| Path::new(p).exists())
                .collect();
            if !existing_created.is_empty() && trash_files(&existing_created).is_err() {
                move_errors += 1;
            }
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            for p in &existing_created {
                let _ = guard.config.remove_free_icon(p);
            }
            for (src_fid, items) in removed_from_fences {
                notify_fences.insert(src_fid);
                let _ = guard
                    .config
                    .assign_desktop_items_to_bucket(src_fid, items, &desktop_dirs);
            }
            if let Some(fid) = fence_id {
                if let Some(f) = guard.config.fence_mut(fid) {
                    if is_portal {
                        if let Some(dir) = f.external_portal_dir(&desktop_dirs) {
                            f.items = scan_portal_directory(Path::new(&dir));
                        }
                    } else if f.is_desktop_bucket(&desktop_dirs) {
                        let _ = guard
                            .config
                            .release_desktop_items(fid, &added_paths, &desktop_dirs);
                    }
                }
                guard.selected_items.remove(&fid);
                guard.selection_anchor.remove(&fid);
            }
            for (src_before, _) in &moved_pairs {
                if Path::new(src_before).exists()
                    && guard.config.desktop_owner_of(src_before, &desktop_dirs).is_none()
                {
                    guard.place_released_icons_on_free_desktop(
                        std::slice::from_ref(src_before),
                        None,
                    );
                }
            }
            if !is_portal {
                for added in &added_paths {
                    let added_key = normalize_path_key(added);
                    let was_created = existing_created
                        .iter()
                        .any(|c| normalize_path_key(c) == added_key);
                    if !was_created
                        && Path::new(added).exists()
                        && guard.config.desktop_owner_of(added, &desktop_dirs).is_none()
                    {
                        guard.place_released_icons_on_free_desktop(
                            std::slice::from_ref(added),
                            None,
                        );
                    }
                }
            }
            guard.refresh_all_unfiled_from_disk_if_present();
            guard.bump_free_icon_epoch();
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            if move_errors > 0 {
                label = "部分文件未能撤销还原";
            }
        }
        UndoAction::ReorderItems {
            fence_id,
            old_items,
        } => {
            notify_fences.insert(fence_id);
            label = "已撤销排序";
            let mut guard = lock_unpoisoned(shared);
            let desktop_dirs = guard.desktop_dirs.clone();
            if let Some(f) = guard.config.fence_mut(fence_id)
                && f.is_desktop_bucket(&desktop_dirs)
            {
                f.items = old_items;
                guard.selected_items.remove(&fence_id);
                guard.selection_anchor.remove(&fence_id);
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            } else {
                label = "原格子已不存在，无法撤销排序";
            }
        }
    }

    {
        let mut guard = lock_unpoisoned(shared);
        push_undo_toast(&mut guard, caller_fence_id, label.to_string());
        for fid in notify_fences {
            guard.request_repaint_fence(ctx, fid);
        }
        if notify_free {
            guard.mark_free_desktop_dirty(Some(ctx));
        }
    }
    ctx.request_repaint_of(ViewportId::ROOT);
}
