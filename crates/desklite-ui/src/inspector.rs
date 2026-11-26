use std::sync::{Arc, Mutex};
use std::time::Instant;

use desklite_core::{FenceKind, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, Rect};
use desklite_win32::{
    find_window_by_exact_title, focus_window_now, live_sizemove_rect_for_hwnd, set_hwnd_locked,
    style_floating_inspector_window,
};
use egui::{
    Align, Color32, CornerRadius, Frame, Key, Layout, Margin, PointerButton, Pos2, ScrollArea,
    Sense, Stroke, Ui, Vec2, ViewportCommand, ViewportId,
};

use crate::item_grid::draw_header_icon_btn;
use crate::state::{
    FenceUiOutput, INSPECTOR_HEIGHT, INSPECTOR_TITLE, INSPECTOR_WIDTH, InspectorState,
    SharedFenceState, UndoAction, apply_param_editor_rect, duplicate_fence_with_style,
    format_fence_title, lock_unpoisoned, request_repaint_all_fences, resolve_fence_layout,
};

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
    let y = spec_rect
        .top
        .clamp(work.top, (work.bottom - insp_h).max(work.top));
    (x, y)
}

pub fn open_param_editor_for_fence(
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    let (insp_hwnd, initial_pos, needs_uncollapse) = {
        let mut guard = lock_unpoisoned(shared);
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
            name_edit_initial: Some((fence_id, spec.name.clone())),
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

pub fn close_inspector_safely(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let mut repainted_fid = None;
    {
        let mut guard = lock_unpoisoned(shared);
        let (draft_to_commit, initial_name) = if let Some(ref mut ins) = guard.inspector {
            ins.open = false;
            ins.drag_grab = None;
            (ins.draft_name.take(), ins.name_edit_initial.take())
        } else {
            (None, None)
        };
        if let Some((fid, draft)) = draft_to_commit {
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty()
                && let Some(f) = guard.config.fence_mut(fid)
            {
                let baseline = initial_name
                    .filter(|(ifid, _)| *ifid == fid)
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| f.name.clone());
                f.name = trimmed.clone();
                if baseline != trimmed {
                    guard.push_undo(UndoAction::RenameFence {
                        fence_id: fid,
                        old_name: baseline,
                    });
                }
                repainted_fid = Some(fid);
            }
        }
        guard.dirty_layout = true;
    }
    out.needs_immediate_save = true;
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    if let Some(fid) = repainted_fid {
        ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", fid)));
    }
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

    let (insp_opt, all_fences, monitors, prefs_snap, hide_all_snap) = {
        let mut guard = lock_unpoisoned(shared);
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
            guard.config.hide_all_fences,
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
        let guard = lock_unpoisoned(shared);
        (
            guard
                .config
                .fences
                .iter()
                .find(|f| f.id == target_id)
                .cloned(),
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
        } else if let Some(ref mut ins) = lock_unpoisoned(shared).inspector {
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
                        let mut guard = lock_unpoisoned(shared);
                        let next_name = guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == picked)
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        if let Some(ref mut ins) = guard.inspector {
                            ins.target_fence_id = picked;
                            ins.draft_name = Some((picked, next_name.clone()));
                            ins.name_edit_initial = Some((picked, next_name));
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
                    && let Some(ref mut ins) = lock_unpoisoned(shared).inspector
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
                            let mut name_committed = false;
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
                                    if name_resp.lost_focus()
                                        || (name_resp.has_focus()
                                            && ui.input(|i| i.key_pressed(Key::Enter)))
                                    {
                                        name_committed = true;
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
                                        .add(egui::Slider::new(&mut alpha, 40..=245).show_value(false))
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
                                    if ui.checkbox(&mut locked, "锁定位置与尺寸").changed() {
                                        lock_changed = true;
                                    }
                                });
                            });

                            let is_list_mode = matches!(next_kind, FenceKind::FileList);
                            let mut ov = spec.layout_override.clone().unwrap_or_default();
                            let (_, _, _, _) = resolve_fence_layout(&spec, &prefs_snap);
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

                            let mut g_cell_w = prefs_snap.icon_cell_w.clamp(64, 160);
                            let mut g_cell_h = prefs_snap.icon_cell_h.clamp(72, 180);
                            let mut g_row_h = prefs_snap.list_row_h.clamp(24, 56);
                            let mut g_list_icon = prefs_snap.list_icon_px.clamp(14, 36);
                            let mut snap_px = prefs_snap.snap_threshold_px.clamp(0, 40);
                            let mut hide_all = hide_all_snap;
                            let mut prefs_changed = false;
                            let mut hide_all_changed = false;

                            inspector_card(ui, "全局默认排版与桌面设置", |ui| {
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
                                labeled_row(ui, "桌面显隐", |ui| {
                                    if ui
                                        .checkbox(&mut hide_all, "隐藏全部格子（双击桌面切换）")
                                        .changed()
                                    {
                                        hide_all_changed = true;
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
                                || name_committed
                                || kind_changed
                                || alpha_changed
                                || lock_changed
                                || single_changed
                                || prefs_changed
                                || hide_all_changed
                            {
                                let mut guard = lock_unpoisoned(shared);
                                let old_baseline = guard
                                    .inspector
                                    .as_ref()
                                    .and_then(|ins| ins.name_edit_initial.as_ref())
                                    .filter(|(fid, _)| *fid == target_id)
                                    .map(|(_, s)| s.clone());

                                if let Some(ref mut ins) = guard.inspector {
                                    if name_committed {
                                        let trimmed = name_buf.trim().to_string();
                                        ins.draft_name = Some((target_id, trimmed.clone()));
                                        if !trimmed.is_empty() {
                                            ins.name_edit_initial =
                                                Some((target_id, trimmed.clone()));
                                        }
                                        name_buf = trimmed;
                                    } else if name_changed {
                                        ins.draft_name = Some((target_id, name_buf.clone()));
                                    }
                                }
                                let mut push_rename_undo = None;
                                if let Some(f) = guard.config.fence_mut(target_id) {
                                    if name_changed || name_committed {
                                        let valid = if name_committed {
                                            name_buf.trim().to_string()
                                        } else {
                                            name_buf.clone()
                                        };
                                        if !valid.trim().is_empty() {
                                            if name_committed
                                                && let Some(prev) = old_baseline
                                                && prev != valid
                                            {
                                                push_rename_undo = Some(UndoAction::RenameFence {
                                                    fence_id: target_id,
                                                    old_name: prev,
                                                });
                                            }
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
                                if let Some(act) = push_rename_undo {
                                    guard.push_undo(act);
                                }
                                if prefs_changed {
                                    guard.config.preferences.icon_cell_w = g_cell_w;
                                    guard.config.preferences.icon_cell_h = g_cell_h;
                                    guard.config.preferences.list_row_h = g_row_h;
                                    guard.config.preferences.list_icon_px = g_list_icon;
                                    guard.config.preferences.snap_threshold_px = snap_px;
                                }
                                if hide_all_changed {
                                    guard.config.hide_all_fences = hide_all;
                                }
                                guard.mark_debounced_save(&ctx);
                            }
                            if prefs_changed || hide_all_changed {
                                request_repaint_all_fences(shared, &ctx);
                                ctx.request_repaint_of(ViewportId::ROOT);
                            } else if name_changed
                                || name_committed
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
                        lock_unpoisoned(shared).toast_msg.insert(
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
