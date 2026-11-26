use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    DesktopIconPos, FenceItem, FenceRole, ItemKind, Point, Rect, clamp_point_to_monitors,
    normalize_path_key,
};
use desklite_win32::{
    Win32Hotkey, allow_foreground_self, attach_free_desktop_canvas, clipboard_has_files,
    copy_text_to_clipboard, cursor_screen_pos, find_window_by_exact_title, focus_window_now,
    foreground_window_handle, free_desktop_hwnd as win32_free_desktop_hwnd, is_ctrl_key_down,
    is_directly_on_desktop, is_free_desktop_hwnd, is_primary_mouse_down, launch_as_admin,
    launch_item, paste_files_to_directory_detailed, read_clipboard_files, rename_file_in_place,
    resolve_dropped_item, reveal_in_explorer, set_active_text_edit_hwnd,
    set_clipboard_files_for_window, show_file_properties, take_win32_hotkeys, trash_files,
    user_desktop_dir, window_outer_rect,
};
use egui::{
    Color32, CornerRadius, Key, PointerButton, Pos2, Sense, Stroke, Ui, Vec2, ViewportId,
};

use crate::actions::{SortMode, sort_items_slice, start_async_drag_from_paths};
use crate::item_grid::{
    file_stem_char_len, grid_item_metrics, handle_text_edit_clipboard, paint_grid_item_in_rect,
};
use crate::state::{
    COLLAPSED_HEIGHT, FREE_DESKTOP_TITLE, FenceCommand, FenceUiOutput, FreeDesktopMenuCommand,
    FreeIconDragState, FreeIconMenuCommand, FreeItemRenamePrompt, FreeMarqueeState,
    SharedFenceState, UndoAction, flush_pending_repaints, lock_unpoisoned,
    request_repaint_all_fences,
};
use crate::undo::execute_undo;

const FREE_SCOPE_ID: i32 = -1;

fn ensure_free_desktop_attached(shared: &Arc<Mutex<SharedFenceState>>) -> isize {
    let existing = lock_unpoisoned(shared).free_desktop_hwnd;
    if existing != 0 && is_free_desktop_hwnd(existing) {
        return existing;
    }
    let Some(raw_hwnd) = find_window_by_exact_title(FREE_DESKTOP_TITLE) else {
        return 0;
    };
    attach_free_desktop_canvas(raw_hwnd);
    if win32_free_desktop_hwnd() == raw_hwnd {
        lock_unpoisoned(shared).free_desktop_hwnd = raw_hwnd;
        raw_hwnd
    } else {
        0
    }
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

fn visible_fence_rects_for_hit(
    fences: &[desklite_core::FenceSpec],
    hide_all: bool,
    ppp: f32,
) -> Vec<Rect> {
    if hide_all {
        return Vec::new();
    }
    fences
        .iter()
        .map(|f| {
            if f.collapsed {
                let ch = (COLLAPSED_HEIGHT * ppp).round() as i32;
                Rect::new(f.rect.left, f.rect.top, f.rect.right, f.rect.top + ch)
            } else {
                f.rect
            }
        })
        .collect()
}

fn rect_contains_pt(r: Rect, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn handle_free_desktop_drop(
    dropped_paths: Vec<String>,
    drop_screen: (i32, i32),
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    if dropped_paths.is_empty() {
        return;
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
        return;
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
        guard.place_released_icons_on_free_desktop(&all_to_place, Some(drop_screen));
        guard.refresh_all_unfiled_from_disk_if_present();
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
    }
    out.needs_immediate_save = true;
    for (fid, _) in affected_fences {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
    ctx.request_repaint();
    ctx.request_repaint_of(ViewportId::ROOT);
}

fn paste_to_free_desktop(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let Some((sources, is_cut)) = read_clipboard_files() else {
        if clipboard_has_files() {
            lock_unpoisoned(shared).free_toast =
                Some(("剪贴板正忙，请重试".to_string(), Instant::now()));
            ctx.request_repaint();
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
        guard.refresh_all_unfiled_from_disk_if_present();
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
    }
    out.needs_immediate_save = true;
    for (fid, _) in affected {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
    ctx.request_repaint();
}

fn commit_free_item_rename(
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
        lock_unpoisoned(shared).free_rename_prompt = None;
        return;
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
            guard.refresh_all_unfiled_from_disk_if_present();
            guard.push_undo(UndoAction::RenameItem {
                fence_id: None,
                old_path: old_path.to_string(),
                new_path,
            });
            guard.free_rename_prompt = None;
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
            ctx.request_repaint();
        }
        Err(e) => {
            let mut guard = lock_unpoisoned(shared);
            guard.free_rename_prompt = None;
            guard.free_toast = Some((format!("重命名失败: {e}"), Instant::now()));
            ctx.request_repaint();
        }
    }
}

fn render_free_item_rename_area(
    ui: &mut Ui,
    edit_rect: egui::Rect,
    clip_rect: egui::Rect,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> (Option<(String, String)>, bool) {
    let snap = { lock_unpoisoned(shared).free_rename_prompt.clone() };
    let Some(mut rp) = snap else {
        return (None, false);
    };
    let te_id = ui.make_persistent_id(("free_item_rename_te", &rp.original_path));
    let box_w = edit_rect.width().max(64.0);
    let box_h = edit_rect.height().max(22.0);
    let area_resp = egui::Area::new(ui.make_persistent_id("free_item_rename_area"))
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

    let mut commit = None;
    let mut cancel = false;
    if ui.input(|i| i.key_pressed(Key::Enter)) {
        let trimmed = rp.draft.trim().to_string();
        if !trimmed.is_empty() {
            commit = Some((rp.original_path.clone(), trimmed));
        } else {
            cancel = true;
        }
    } else if ui.input(|i| i.key_pressed(Key::Escape)) {
        cancel = true;
    } else if out_te.lost_focus() {
        let trimmed = rp.draft.trim().to_string();
        if !trimmed.is_empty() {
            commit = Some((rp.original_path.clone(), trimmed));
        } else {
            cancel = true;
        }
    }

    let mut guard = lock_unpoisoned(shared);
    if let Some(ref mut cur) = guard.free_rename_prompt
        && cur.original_path == rp.original_path
    {
        cur.draft = rp.draft;
        cur.needs_focus = rp.needs_focus;
    }
    (commit, cancel)
}

/// Renders the transparent full-virtual-screen `FreeDesktop` layer (Route B) hosting all unfiled
/// desktop icons below DeskLite fences and above the Windows wallpaper (`WorkerW`/`Progman`).
pub fn render_free_desktop_viewport(
    ui: &mut Ui,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();
    let my_hwnd = ensure_free_desktop_attached(shared);

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

    let ppp = ctx.pixels_per_point().max(0.5);
    let (
        free_icons,
        cell_w,
        cell_h,
        monitors,
        vbounds,
        visible_fences,
        selected_keys,
        cut_paths,
        renaming_path,
        toast_text,
    ) = {
        let mut guard = lock_unpoisoned(shared);
        let toast_text = if let Some((msg, at)) = guard.free_toast.clone() {
            if at.elapsed() < Duration::from_millis(1400) {
                ctx.request_repaint_after(Duration::from_millis(80));
                Some(msg)
            } else {
                guard.free_toast = None;
                None
            }
        } else {
            None
        };
        let cw = guard.config.preferences.icon_cell_w.clamp(64, 160);
        let ch = guard.config.preferences.icon_cell_h.clamp(72, 180);
        let vb = guard.topology.virtual_bounds();
        let vf = visible_fence_rects_for_hit(&guard.config.fences, guard.config.hide_all_fences, ppp);
        let renaming_path = guard
            .free_rename_prompt
            .as_ref()
            .map(|r| r.original_path.clone());
        (
            guard.config.active_free_icons().to_vec(),
            cw,
            ch,
            guard.topology.monitors().to_vec(),
            vb,
            vf,
            guard.free_selected_paths.clone(),
            guard.cut_paths.clone(),
            renaming_path,
            toast_text,
        )
    };

    let focused_now = ctx.input(|i| i.focused);
    if my_hwnd != 0 {
        let text_editing_now = renaming_path.is_some() || ctx.egui_wants_keyboard_input();
        if text_editing_now {
            set_active_text_edit_hwnd(my_hwnd);
        } else if focused_now || foreground_window_handle() == my_hwnd {
            set_active_text_edit_hwnd(0);
        }
    }

    let (wl, wt) = window_outer_rect(my_hwnd)
        .map(|(l, t, _, _)| (l, t))
        .unwrap_or((vbounds.left, vbounds.top));

    // Handle external OS file drops onto FreeDesktop canvas
    let dropped_paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    if !dropped_paths.is_empty() {
        let drop_screen = cursor_screen_pos().unwrap_or_else(|| {
            let p = ctx
                .input(|i| i.pointer.hover_pos())
                .unwrap_or(Pos2::new(60.0, 60.0));
            (wl + (p.x * ppp).round() as i32, wt + (p.y * ppp).round() as i32)
        });
        handle_free_desktop_drop(dropped_paths, drop_screen, shared, &ctx, &mut out);
    }

    let canvas_rect = ui.max_rect();
    let bg_resp = ui.interact(
        canvas_rect,
        ui.make_persistent_id("free_desktop_canvas_bg"),
        Sense::click_and_drag(),
    );

    let item_logical_w = cell_w as f32;
    let item_logical_h = cell_h as f32;
    let item_size = Vec2::new(item_logical_w, item_logical_h);

    let icon_items: Vec<FenceItem> = free_icons
        .iter()
        .enumerate()
        .map(|(i, ic)| FenceItem {
            display_name: ic.display_name.clone(),
            order: i as i32,
            kind: free_icon_kind(&ic.id),
            path: ic.id.clone(),
        })
        .collect();
    let textures = {
        let mut guard = lock_unpoisoned(shared);
        guard.resolve_item_textures(&ctx, &icon_items)
    };

    let mut item_rects: Vec<(usize, String, egui::Rect)> = Vec::with_capacity(free_icons.len());
    let mut clicked_icon: Option<(usize, String)> = None;
    let mut double_clicked_icon: Option<String> = None;
    let mut right_clicked_icon: Option<(usize, String)> = None;
    let mut drag_started_icon: Option<(usize, String)> = None;
    let mut rename_commit: Option<(String, String)> = None;
    let mut rename_cancel = false;

    for (idx, (ic, item)) in free_icons.iter().zip(icon_items.iter()).enumerate() {
        let local_min = Pos2::new(
            (ic.pos.x - wl) as f32 / ppp,
            (ic.pos.y - wt) as f32 / ppp,
        );
        let rect = egui::Rect::from_min_size(local_min, item_size);
        let key = normalize_path_key(&ic.id);
        let is_selected = selected_keys.contains(&key);
        let is_cut = cut_paths.contains(&ic.id.to_ascii_lowercase());
        let tex = textures.get(idx).and_then(|t| t.as_ref());
        let is_renaming = renaming_path
            .as_ref()
            .is_some_and(|rp| normalize_path_key(rp) == key);

        if is_renaming {
            let (icon_sz, font_size, gap) = grid_item_metrics(item_size);
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
            let (cm, cn) = render_free_item_rename_area(ui, edit_rect, canvas_rect, shared, &ctx);
            if cm.is_some() {
                rename_commit = cm;
            }
            if cn {
                rename_cancel = true;
            }
            item_rects.push((idx, key, rect));
            continue;
        }

        let resp = ui.interact(
            rect,
            ui.make_persistent_id(("free_icon", &key)),
            Sense::click_and_drag(),
        );
        let was_truncated =
            paint_grid_item_in_rect(ui, FREE_SCOPE_ID, item, tex, rect, &resp, is_selected, is_cut);
        let resp = if was_truncated {
            resp.on_hover_text(&item.display_name)
        } else {
            resp
        };
        if resp.clicked() {
            clicked_icon = Some((idx, ic.id.clone()));
        }
        if resp.double_clicked() {
            double_clicked_icon = Some(ic.id.clone());
        }
        if resp.secondary_clicked() {
            right_clicked_icon = Some((idx, ic.id.clone()));
        }
        if resp.drag_started_by(PointerButton::Primary) {
            drag_started_icon = Some((idx, ic.id.clone()));
        }
        item_rects.push((idx, key, rect));
    }

    if let Some((old_p, new_n)) = rename_commit {
        commit_free_item_rename(&old_p, &new_n, shared, &ctx, &mut out);
    } else if rename_cancel {
        lock_unpoisoned(shared).free_rename_prompt = None;
    }

    // Handle icon interactions
    if let Some(path) = double_clicked_icon {
        if let Err(e) = launch_item(&path) {
            lock_unpoisoned(shared).free_toast = Some((format!("打开失败: {e}"), Instant::now()));
            ctx.request_repaint();
        }
    } else if let Some((idx, path)) = clicked_icon {
        if !focused_now {
            focus_window_now(my_hwnd);
        }
        let mods = ctx.input(|i| i.modifiers);
        let ctrl = mods.command || is_ctrl_key_down();
        let shift = mods.shift;
        let key = normalize_path_key(&path);
        let mut guard = lock_unpoisoned(shared);
        if shift {
            let anchor_idx = guard
                .free_selection_anchor
                .as_ref()
                .and_then(|ak| item_rects.iter().find(|(_, k, _)| k == ak).map(|(i, _, _)| *i))
                .unwrap_or(0);
            if !ctrl {
                guard.free_selected_paths.clear();
            }
            for (i, k, _) in &item_rects {
                if *i >= anchor_idx.min(idx) && *i <= anchor_idx.max(idx) {
                    guard.free_selected_paths.insert(k.clone());
                }
            }
        } else if ctrl {
            if !guard.free_selected_paths.remove(&key) {
                guard.free_selected_paths.insert(key.clone());
            }
            guard.free_selection_anchor = Some(key);
        } else {
            guard.free_selected_paths.clear();
            guard.free_selected_paths.insert(key.clone());
            guard.free_selection_anchor = Some(key);
        }
    }

    if let Some((_, path)) = right_clicked_icon {
        if !focused_now {
            focus_window_now(my_hwnd);
        }
        let key = normalize_path_key(&path);
        let paths: Vec<String> = {
            let mut guard = lock_unpoisoned(shared);
            if !guard.free_selected_paths.contains(&key) {
                guard.free_selected_paths.clear();
                guard.free_selected_paths.insert(key.clone());
                guard.free_selection_anchor = Some(key);
            }
            guard
                .config
                .active_free_icons()
                .iter()
                .filter(|ic| guard.free_selected_paths.contains(&normalize_path_key(&ic.id)))
                .map(|ic| ic.id.clone())
                .collect()
        };
        if !paths.is_empty() {
            let (sx, sy) = cursor_screen_pos().unwrap_or((120, 120));
            allow_foreground_self();
            lock_unpoisoned(shared).push_command(
                &ctx,
                FenceCommand::ShowFreeIconMenu {
                    paths,
                    screen_x: sx,
                    screen_y: sy,
                },
            );
        }
    }

    if let Some((idx, path)) = drag_started_icon {
        let mods = ctx.input(|i| i.modifiers);
        let ctrl = mods.command || is_ctrl_key_down();
        let shift = mods.shift;
        let key = normalize_path_key(&path);
        let mut guard = lock_unpoisoned(shared);
        if !guard.free_selected_paths.contains(&key) {
            if shift {
                let anchor_idx = guard
                    .free_selection_anchor
                    .as_ref()
                    .and_then(|ak| item_rects.iter().find(|(_, k, _)| k == ak).map(|(i, _, _)| *i))
                    .unwrap_or(0);
                if !ctrl {
                    guard.free_selected_paths.clear();
                }
                for (i, k, _) in &item_rects {
                    if *i >= anchor_idx.min(idx) && *i <= anchor_idx.max(idx) {
                        guard.free_selected_paths.insert(k.clone());
                    }
                }
            } else if ctrl {
                guard.free_selected_paths.insert(key.clone());
                guard.free_selection_anchor = Some(key);
            } else {
                guard.free_selected_paths.clear();
                guard.free_selected_paths.insert(key.clone());
                guard.free_selection_anchor = Some(key);
            }
        }
        let mut start_positions = HashMap::new();
        let mut paths = Vec::new();
        for ic in guard.config.active_free_icons() {
            let k = normalize_path_key(&ic.id);
            if guard.free_selected_paths.contains(&k) {
                start_positions.insert(k, ic.pos);
                paths.push(ic.id.clone());
            }
        }
        if !paths.is_empty()
            && let Some(cur) = cursor_screen_pos()
        {
            guard.free_icon_drag = Some(FreeIconDragState {
                origin_screen: cur,
                last_screen: cur,
                paths,
                start_positions,
                moved: false,
            });
        }
    }

    // Active local free-icon drag (escalates to async OLE drag if dragged over a fence or external window)
    let active_free_drag = lock_unpoisoned(shared).free_icon_drag.clone();
    if let Some(mut drag) = active_free_drag {
        if is_primary_mouse_down() {
            if let Some((cx, cy)) = cursor_screen_pos() {
                let over_fence = visible_fences
                    .iter()
                    .any(|r| rect_contains_pt(*r, cx, cy));
                let out_of_vbounds = !rect_contains_pt(vbounds, cx, cy);
                if over_fence || out_of_vbounds {
                    let drag_paths = drag.paths.clone();
                    {
                        let mut guard = lock_unpoisoned(shared);
                        guard.free_icon_drag = None;
                        if let Some(free) = guard.config.active_free_icons_mut() {
                            for ic in free.iter_mut() {
                                if let Some(&orig) =
                                    drag.start_positions.get(&normalize_path_key(&ic.id))
                                {
                                    ic.pos = orig;
                                }
                            }
                        }
                    }
                    start_async_drag_from_paths(None, drag_paths, shared);
                } else if (cx, cy) != drag.last_screen {
                    let dx = cx - drag.origin_screen.0;
                    let dy = cy - drag.origin_screen.1;
                    drag.last_screen = (cx, cy);
                    if dx.abs() > 3 || dy.abs() > 3 {
                        drag.moved = true;
                    }
                    let mut guard = lock_unpoisoned(shared);
                    let cell_margin = (cell_w.clamp(48, 180), cell_h.clamp(56, 200));
                    if let Some(free) = guard.config.active_free_icons_mut() {
                        for ic in free.iter_mut() {
                            if let Some(&orig) =
                                drag.start_positions.get(&normalize_path_key(&ic.id))
                            {
                                let raw = Point {
                                    x: orig.x + dx,
                                    y: orig.y + dy,
                                };
                                ic.pos = if monitors.is_empty() {
                                    raw
                                } else {
                                    clamp_point_to_monitors(raw, cell_margin, &monitors)
                                        .map(|(p, _)| p)
                                        .unwrap_or(raw)
                                };
                            }
                        }
                    }
                    guard.free_icon_drag = Some(drag);
                }
            }
            ctx.request_repaint();
        } else {
            let mut guard = lock_unpoisoned(shared);
            guard.free_icon_drag = None;
            if drag.moved {
                guard.mark_debounced_save(&ctx);
                out.needs_immediate_save = true;
            }
        }
    }

    // Blank desktop background interactions
    let drag_busy = lock_unpoisoned(shared).free_icon_drag.is_some();
    if !drag_busy {
        if bg_resp.clicked() {
            if !focused_now {
                focus_window_now(my_hwnd);
            }
            let mods = ctx.input(|i| i.modifiers);
            if !mods.ctrl && !mods.shift {
                let mut guard = lock_unpoisoned(shared);
                guard.free_selected_paths.clear();
                guard.free_selection_anchor = None;
            }
            let now = Instant::now();
            let mut dbl_fallback = false;
            {
                let mut guard = lock_unpoisoned(shared);
                if let Some(prev) = guard.free_last_bg_click
                    && now.duration_since(prev) <= Duration::from_millis(480)
                {
                    dbl_fallback = true;
                    guard.free_last_bg_click = None;
                } else {
                    guard.free_last_bg_click = Some(now);
                }
            }
            if dbl_fallback {
                lock_unpoisoned(shared).push_command(&ctx, FenceCommand::ToggleHideAllFences);
            }
        }
        if bg_resp.double_clicked() {
            let mut guard = lock_unpoisoned(shared);
            guard.free_last_bg_click = None;
            guard.push_command(&ctx, FenceCommand::ToggleHideAllFences);
        }
        if bg_resp.secondary_clicked() {
            if !focused_now {
                focus_window_now(my_hwnd);
            }
            let (sx, sy) = cursor_screen_pos().unwrap_or((120, 120));
            allow_foreground_self();
            lock_unpoisoned(shared).push_command(
                &ctx,
                FenceCommand::ShowFreeDesktopMenu {
                    screen_x: sx,
                    screen_y: sy,
                },
            );
        }

        // Marquee selection on blank FreeDesktop
        let pointer_pos = ctx.input(|i| i.pointer.interact_pos());
        let primary_down = ctx.input(|i| i.pointer.primary_down());
        if bg_resp.drag_started_by(PointerButton::Primary)
            && let Some(pos) = pointer_pos
        {
            let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
            let additive = ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift);
            let mut guard = lock_unpoisoned(shared);
            let base_selection = if additive {
                guard.free_selected_paths.clone()
            } else {
                HashSet::new()
            };
            guard.free_marquee = Some(FreeMarqueeState {
                origin,
                current: pos,
                additive,
                base_selection,
            });
        }
        let active_marquee = {
            let mut guard = lock_unpoisoned(shared);
            if primary_down && !bg_resp.drag_stopped() {
                if let Some(ref mut mq) = guard.free_marquee {
                    if let Some(pos) = pointer_pos {
                        mq.current = pos;
                    }
                    let sel_rect = egui::Rect::from_two_pos(mq.origin, mq.current);
                    let mut next_sel = if mq.additive {
                        mq.base_selection.clone()
                    } else {
                        HashSet::new()
                    };
                    for (_, key, r) in &item_rects {
                        if sel_rect.intersects(*r) {
                            next_sel.insert(key.clone());
                        }
                    }
                    let mq_clone = mq.clone();
                    guard.free_selected_paths = next_sel;
                    Some(mq_clone)
                } else {
                    None
                }
            } else {
                guard.free_marquee = None;
                None
            }
        };
        if let Some(mq) = active_marquee {
            let draw_rect =
                egui::Rect::from_two_pos(mq.origin, mq.current).intersect(canvas_rect);
            if draw_rect.width() > 1.0 && draw_rect.height() > 1.0 {
                let painter = ui.painter().with_clip_rect(canvas_rect);
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

    // Keyboard shortcuts for FreeDesktop
    let has_input_focus = focused_now || (my_hwnd != 0 && foreground_window_handle() == my_hwnd);
    let can_hotkeys = renaming_path.is_none()
        && lock_unpoisoned(shared).free_marquee.is_none()
        && lock_unpoisoned(shared).free_icon_drag.is_none()
        && !egui::Popup::is_any_open(&ctx);

    let win32_hotkeys = if my_hwnd != 0 {
        let list = take_win32_hotkeys(my_hwnd);
        if can_hotkeys { list } else { Vec::new() }
    } else {
        Vec::new()
    };

    let selected_paths_ordered: Vec<String> = free_icons
        .iter()
        .filter(|ic| selected_keys.contains(&normalize_path_key(&ic.id)))
        .map(|ic| ic.id.clone())
        .collect();

    if can_hotkeys {
        for hk in &win32_hotkeys {
            match hk {
                Win32Hotkey::SelectAll => {
                    let mut guard = lock_unpoisoned(shared);
                    guard.free_selected_paths = item_rects.iter().map(|(_, k, _)| k.clone()).collect();
                    guard.free_selection_anchor = item_rects.first().map(|(_, k, _)| k.clone());
                }
                Win32Hotkey::Copy => {
                    let _ = apply_free_icon_menu_command(
                        &selected_paths_ordered,
                        FreeIconMenuCommand::Copy { cut: false },
                        shared,
                        &ctx,
                    );
                }
                Win32Hotkey::Cut => {
                    let _ = apply_free_icon_menu_command(
                        &selected_paths_ordered,
                        FreeIconMenuCommand::Copy { cut: true },
                        shared,
                        &ctx,
                    );
                }
                Win32Hotkey::CopyPaths => {
                    let _ = apply_free_icon_menu_command(
                        &selected_paths_ordered,
                        FreeIconMenuCommand::CopyPaths,
                        shared,
                        &ctx,
                    );
                }
                Win32Hotkey::Paste => {
                    paste_to_free_desktop(shared, &ctx, &mut out);
                }
                Win32Hotkey::Undo => {
                    execute_undo(None, shared, &ctx, &mut out);
                }
            }
        }
    }

    if can_hotkeys && has_input_focus && win32_hotkeys.is_empty() {
        let (f2, enter, alt_enter, del, f5, esc, ctrl_a, ctrl_c, ctrl_sc, ctrl_x, ctrl_v, ctrl_z) =
            ctx.input(|i| {
                let ctrl_down = i.modifiers.command || is_ctrl_key_down();
                let cmd = my_hwnd == 0 && ctrl_down && !i.modifiers.alt;
                let sh = i.modifiers.shift;
                (
                    i.key_pressed(Key::F2),
                    !i.modifiers.alt && i.key_pressed(Key::Enter),
                    i.modifiers.alt && i.key_pressed(Key::Enter),
                    i.key_pressed(Key::Delete),
                    i.key_pressed(Key::F5),
                    i.key_pressed(Key::Escape),
                    cmd && !sh && i.key_pressed(Key::A),
                    cmd && !sh && i.key_pressed(Key::C),
                    cmd && sh && i.key_pressed(Key::C),
                    cmd && !sh && i.key_pressed(Key::X),
                    cmd && !sh && i.key_pressed(Key::V),
                    cmd && !sh && i.key_pressed(Key::Z),
                )
            });
        if ctrl_a {
            let mut guard = lock_unpoisoned(shared);
            guard.free_selected_paths = item_rects.iter().map(|(_, k, _)| k.clone()).collect();
        } else if ctrl_sc {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::CopyPaths,
                shared,
                &ctx,
            );
        } else if ctrl_c {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Copy { cut: false },
                shared,
                &ctx,
            );
        } else if ctrl_x {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Copy { cut: true },
                shared,
                &ctx,
            );
        } else if ctrl_v {
            paste_to_free_desktop(shared, &ctx, &mut out);
        } else if ctrl_z {
            execute_undo(None, shared, &ctx, &mut out);
        } else if f5 {
            lock_unpoisoned(shared).push_command(&ctx, FenceCommand::RefreshDesktop);
        } else if esc {
            let mut guard = lock_unpoisoned(shared);
            guard.free_selected_paths.clear();
            guard.free_selection_anchor = None;
            guard.cut_paths.clear();
        } else if f2 && !selected_paths_ordered.is_empty() {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Rename,
                shared,
                &ctx,
            );
        } else if alt_enter && !selected_paths_ordered.is_empty() {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Properties,
                shared,
                &ctx,
            );
        } else if enter && !selected_paths_ordered.is_empty() {
            let _ = apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Open,
                shared,
                &ctx,
            );
        } else if del
            && !selected_paths_ordered.is_empty()
            && apply_free_icon_menu_command(
                &selected_paths_ordered,
                FreeIconMenuCommand::Trash,
                shared,
                &ctx,
            )
        {
            out.needs_immediate_save = true;
        }
    }

    if let Some(ref msg) = toast_text {
        let primary_rect = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .map(|m| m.work_area)
            .unwrap_or(vbounds);
        let cx = ((primary_rect.left + primary_rect.right) / 2 - wl) as f32 / ppp;
        let cy = ((primary_rect.top + 28) - wt) as f32 / ppp;
        let pill_rect = egui::Rect::from_center_size(Pos2::new(cx, cy), Vec2::new(210.0, 28.0));
        ui.painter().rect_filled(
            pill_rect,
            CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(24, 28, 38, 225),
        );
        ui.painter().rect_stroke(
            pill_rect,
            CornerRadius::same(14),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(125, 190, 255, 140)),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            pill_rect.center(),
            egui::Align2::CENTER_CENTER,
            msg,
            egui::FontId::proportional(12.5),
            Color32::from_rgb(235, 244, 255),
        );
    }

    out
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
    let step_x = guard.config.preferences.icon_cell_w.clamp(48, 180);
    let step_y = guard.config.preferences.icon_cell_h.clamp(56, 200);
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
    ctx.request_repaint_of(ViewportId::from_hash_of("desklite_free_desktop"));
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
                        guard.cut_paths.insert(p.to_ascii_lowercase());
                    }
                }
                let verb = if cut { "已剪切" } else { "已复制" };
                guard.free_toast = Some((format!("{verb} {} 项", paths.len()), Instant::now()));
            } else {
                lock_unpoisoned(shared).free_toast =
                    Some(("剪贴板被占用，请重试".to_string(), Instant::now()));
            }
        }
        FreeIconMenuCommand::CopyPaths => {
            if copy_text_to_clipboard(&paths.join("\r\n")).is_ok() {
                lock_unpoisoned(shared).free_toast =
                    Some(("已复制路径".to_string(), Instant::now()));
            }
        }
        FreeIconMenuCommand::Rename => {
            if let Some(first) = paths.first() {
                let file_name = Path::new(first)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| first.clone());
                lock_unpoisoned(shared).free_rename_prompt = Some(FreeItemRenamePrompt {
                    original_path: first.clone(),
                    draft: file_name,
                    needs_focus: true,
                });
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
                        guard.refresh_all_unfiled_from_disk_if_present();
                        guard.push_undo(UndoAction::MoveFreeIconsToBucket {
                            dst_fence_id: target_fid,
                            items,
                            prev_free_icons,
                        });
                        guard.dirty_layout = true;
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
        FreeIconMenuCommand::Trash => {
            match trash_files(paths) {
                Ok(true) => {
                    {
                        let mut guard = lock_unpoisoned(shared);
                        let desktop_dirs = guard.desktop_dirs.clone();
                        for p in paths {
                            guard.invalidate_icon_texture(p);
                            let _ = guard.config.remove_desktop_path_everywhere(p, &desktop_dirs);
                            guard.free_selected_paths.remove(&normalize_path_key(p));
                        }
                        guard.refresh_all_unfiled_from_disk_if_present();
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                    request_repaint_all_fences(shared, ctx);
                }
                Ok(false) => {}
                Err(e) => {
                    lock_unpoisoned(shared).free_toast =
                        Some((format!("删除失败: {e}"), Instant::now()));
                }
            }
        }
        FreeIconMenuCommand::Properties => {
            if let Some(first) = paths.first() {
                let _ = show_file_properties(first);
            }
        }
    }
    ctx.request_repaint_of(ViewportId::from_hash_of("desklite_free_desktop"));
    out.needs_immediate_save
}
