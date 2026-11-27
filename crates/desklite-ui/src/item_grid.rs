use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use desklite_core::{FenceItem, normalize_path_key};
use desklite_win32::{
    copy_text_to_clipboard, focus_window_now, is_ctrl_key_down, is_directly_on_desktop,
    read_clipboard_text,
};
use egui::{
    Align, Color32, CornerRadius, Key, PointerButton, Pos2, Sense, Stroke, TextureHandle, Ui,
    Vec2, text::LayoutJob,
};

use crate::state::{FenceUiOutput, SharedFenceState, UndoAction, lock_unpoisoned};

pub enum ItemUiAction {
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
    DeleteSelectedPermanent,
    BeginItemDrag { idx: usize, ctrl: bool, shift: bool },
    StartDragOut { idx: usize, ctrl: bool, shift: bool },
}

#[allow(clippy::too_many_arguments)]
pub fn handle_item_interactions(
    resp: &egui::Response,
    my_hwnd: isize,
    idx: usize,
    item: &FenceItem,
    is_selected: bool,
    selected_count: usize,
    is_bucket: bool,
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
        let mods = resp.ctx.input(|i| i.modifiers);
        let ctrl = mods.command || is_ctrl_key_down();
        let shift = mods.shift;
        if is_bucket {
            *out_action = Some(ItemUiAction::BeginItemDrag { idx, ctrl, shift });
        } else {
            *out_action = Some(ItemUiAction::StartDragOut { idx, ctrl, shift });
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
        if is_bucket && (ui.button(remove_lbl).clicked() || k_e) {
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

pub fn compute_reorder_target(
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

pub fn paint_reorder_indicator(
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

pub fn apply_reorder_items(
    fence_id: i32,
    moving_indices: &[usize],
    insert_idx: usize,
    shared: &Arc<Mutex<SharedFenceState>>,
    out: &mut FenceUiOutput,
) {
    if moving_indices.is_empty() {
        return;
    }
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
        let moving_set: HashSet<usize> = moving_indices.iter().copied().collect();
        let before_count = moving_indices.iter().filter(|&&m| m < insert_idx).count();
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

pub fn paint_animated_item_bg(
    ui: &Ui,
    rect: egui::Rect,
    anim_id: egui::Id,
    hovered: bool,
    selected: bool,
    rounding: u8,
) {
    let sel_t = ui.ctx().animate_bool_responsive(anim_id.with("sel"), selected);
    let hov_t = ui
        .ctx()
        .animate_bool_responsive(anim_id.with("hov"), hovered && !selected);
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

pub fn grid_item_metrics(size: Vec2) -> (f32, f32, f32) {
    let font_size = (11.2 + (size.x.min(size.y) - 80.0).max(0.0) * 0.035).clamp(11.5, 14.5);
    let est_two_line_h = font_size * 2.55;
    let max_by_w = (size.x * 0.60).max(28.0);
    let max_by_h = (size.y - 14.0 - est_two_line_h).max(28.0);
    let icon_sz = max_by_w.min(max_by_h).clamp(28.0, 96.0).round();
    let gap = (icon_sz * 0.14).clamp(4.0, 9.0).round();
    (icon_sz, font_size, gap)
}

#[allow(clippy::too_many_arguments)]
pub fn paint_grid_item_in_rect(
    ui: &mut Ui,
    scope_id: i32,
    item: &FenceItem,
    tex: Option<&TextureHandle>,
    rect: egui::Rect,
    resp: &egui::Response,
    selected: bool,
    is_cut: bool,
) -> bool {
    let mut was_truncated = false;
    if ui.is_rect_visible(rect) {
        let anim_id =
            ui.make_persistent_id(("grid_item_anim", scope_id, normalize_path_key(&item.path)));
        paint_animated_item_bg(ui, rect.shrink(2.0), anim_id, resp.hovered(), selected, 6);
        let alpha_tint = if is_cut {
            Color32::from_rgba_unmultiplied(255, 255, 255, 130)
        } else {
            Color32::WHITE
        };

        let (icon_sz, font_size, gap) = grid_item_metrics(rect.size());
        let clipped = ui.painter().with_clip_rect(rect.shrink(2.0));
        let font = egui::FontId::proportional(font_size);
        let max_text_w = (rect.width() - 8.0).max(20.0);
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
    was_truncated
}

pub fn render_grid_item(
    ui: &mut Ui,
    scope_id: i32,
    item: &FenceItem,
    tex: Option<&TextureHandle>,
    selected: bool,
    is_cut: bool,
    size: Vec2,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let was_truncated =
        paint_grid_item_in_rect(ui, scope_id, item, tex, rect, &resp, selected, is_cut);
    if was_truncated {
        resp.on_hover_text(&item.display_name)
    } else {
        resp
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_list_item(
    ui: &mut Ui,
    scope_id: i32,
    item: &FenceItem,
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
        let anim_id =
            ui.make_persistent_id(("list_item_anim", scope_id, normalize_path_key(&item.path)));
        paint_animated_item_bg(ui, rect, anim_id, resp.hovered(), selected, 5);
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

pub fn file_stem_char_len(name: &str) -> usize {
    if let Some(dot) = name.rfind('.')
        && dot > 0
    {
        return name[..dot].chars().count();
    }
    name.chars().count()
}

pub fn handle_text_edit_clipboard(ctx: &egui::Context, id: egui::Id, draft: &mut String) {
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

/// Renders an inline rename TextEdit inside a floating `egui::Area` WITHOUT holding `SharedFenceState` lock.
pub fn handle_rename_text_edit(
    ui: &mut Ui,
    edit_rect: egui::Rect,
    clip_rect: egui::Rect,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) -> (Option<(usize, String, String)>, bool) {
    let snap = {
        let guard = lock_unpoisoned(shared);
        guard
            .item_rename_prompt
            .as_ref()
            .filter(|rp| rp.fence_id == fence_id)
            .cloned()
    };
    let Some(mut rp) = snap else {
        return (None, false);
    };

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

    let mut commit = None;
    let mut cancel = false;
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

    let mut guard = lock_unpoisoned(shared);
    if let Some(ref mut cur) = guard.item_rename_prompt
        && cur.fence_id == fence_id
        && cur.item_idx == rp.item_idx
    {
        cur.draft = rp.draft;
        cur.needs_focus = rp.needs_focus;
    }

    (commit, cancel)
}

pub fn render_grid_item_renaming(
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
pub fn render_list_item_renaming(
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

pub fn draw_header_icon_btn(
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

pub fn fit_single_line(
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

pub fn fit_two_lines(
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
